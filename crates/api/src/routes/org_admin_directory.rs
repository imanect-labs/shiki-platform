//! AD/ロール管理。認可とCSRFは中央ルートポリシーを通す。
use crate::{
    directory_admin::{
        self,
        connection::{keycloak_error, validate},
        plan::{AutoHierarchy, AutoPlanInput, DirectoryPlan},
    },
    error::ApiError,
    extract::{AuthContextExt, TraceIdExt},
    keycloak_admin::{
        ldap::{LdapConnectionInput, LdapConnectionStatus},
        KeycloakAdmin,
    },
    server::{AccessPolicy, RouteDecl},
    AppState,
};
use axum::{
    extract::{Path, Query, State},
    routing::{get, post},
    Json,
};
use serde::{Deserialize, Serialize};
use storage::directory::admin::{
    AdGroupPage, DirectoryChangeSet, DirectoryOperation, ManagedRole, ManagedRolePage,
};
use utoipa::{IntoParams, ToSchema};
use uuid::Uuid;

use crate::directory_admin::import::DirectoryImportInput;
pub(crate) fn route_decls() -> Vec<RouteDecl> {
    vec![
        RouteDecl::new(
            "/org-admin/directory/members",
            &["GET"],
            AccessPolicy::OrgAdmin,
            || get(members),
        ),
        RouteDecl::new(
            "/org-admin/directory/preview",
            &["POST"],
            AccessPolicy::OrgAdmin,
            || post(preview).layer(axum::extract::DefaultBodyLimit::max(16 * 1024 * 1024)),
        ),
        RouteDecl::new(
            "/org-admin/directory/folders",
            &["GET"],
            AccessPolicy::OrgAdmin,
            || get(folders),
        ),
        RouteDecl::new(
            "/org-admin/directory/import-plan",
            &["POST"],
            AccessPolicy::OrgAdmin,
            || post(import_plan),
        ),
        RouteDecl::new(
            "/org-admin/directory/operations",
            &["GET"],
            AccessPolicy::OrgAdmin,
            || get(operations),
        ),
        RouteDecl::new(
            "/org-admin/directory",
            &["GET"],
            AccessPolicy::OrgAdmin,
            || get(info),
        ),
        RouteDecl::new(
            "/org-admin/directory/roles",
            &["GET"],
            AccessPolicy::OrgAdmin,
            || get(roles),
        ),
        RouteDecl::new(
            "/org-admin/directory/groups",
            &["GET"],
            AccessPolicy::OrgAdmin,
            || get(groups),
        ),
        RouteDecl::new(
            "/org-admin/directory/apply",
            &["POST"],
            AccessPolicy::OrgAdmin,
            || post(apply).layer(axum::extract::DefaultBodyLimit::max(16 * 1024 * 1024)),
        ),
        RouteDecl::new(
            "/org-admin/directory/operations/{id}",
            &["GET"],
            AccessPolicy::OrgAdmin,
            || get(operation),
        ),
        RouteDecl::new(
            "/org-admin/directory/operations/{id}/retry",
            &["POST"],
            AccessPolicy::OrgAdmin,
            || post(retry),
        ),
        RouteDecl::new(
            "/org-admin/directory/connection",
            &["GET", "PUT"],
            AccessPolicy::OrgAdmin,
            || get(connection).put(save_connection),
        ),
        RouteDecl::new(
            "/org-admin/directory/connection/test",
            &["POST"],
            AccessPolicy::OrgAdmin,
            || post(test_connection),
        ),
        RouteDecl::new(
            "/org-admin/directory/sync",
            &["POST"],
            AccessPolicy::OrgAdmin,
            || post(sync),
        ),
        RouteDecl::new(
            "/org-admin/directory/auto-plan",
            &["POST"],
            AccessPolicy::OrgAdmin,
            || post(auto_plan),
        ),
    ]
}

#[derive(Serialize, ToSchema)]
pub struct DirectoryInfo {
    pub role_id_prefix: String,
    pub keycloak_available: bool,
    pub ldap_allowed_hosts: Vec<String>,
}
#[derive(Deserialize, IntoParams)]
pub struct DirectoryQuery {
    #[serde(default)]
    pub q: String,
    pub parent: Option<String>,
    pub cursor: Option<String>,
    pub limit: Option<usize>,
    /// 選択済みIDを明示取得する（最大100件、カンマ区切り）。
    pub ids: Option<String>,
}
#[derive(Deserialize, ToSchema)]
pub struct DirectorySyncInput {
    pub idempotency_key: Uuid,
}
#[derive(Deserialize, ToSchema)]
pub struct DirectoryRetryInput {
    /// 中断した旧実行の停止と、外部サービスの反映状態を運用者が確認した。
    #[serde(default)]
    pub recovery_confirmed: bool,
}

#[utoipa::path(get, path = "/org-admin/directory", responses((status = 200, body = DirectoryInfo)), security(("session" = [])))]
pub async fn info(
    State(state): State<AppState>,
    AuthContextExt(ctx): AuthContextExt,
) -> Json<DirectoryInfo> {
    Json(DirectoryInfo {
        role_id_prefix: storage::directory::admin::role_id_prefix(&ctx),
        keycloak_available: KeycloakAdmin::from_config(&state.http, &state.config.auth).is_ok(),
        ldap_allowed_hosts: state.config.auth.directory_ldap_allowed_hosts.clone(),
    })
}
#[utoipa::path(get, path = "/org-admin/directory/roles", params(DirectoryQuery), responses((status = 200, body = ManagedRolePage)), security(("session" = [])))]
pub async fn roles(
    State(state): State<AppState>,
    AuthContextExt(ctx): AuthContextExt,
    Query(query): Query<DirectoryQuery>,
) -> Result<Json<ManagedRolePage>, ApiError> {
    let store = state.directory.admin();
    if let Some(ids) = query.ids {
        let ids: Vec<String> = ids
            .split(',')
            .filter(|s| !s.is_empty())
            .map(str::to_string)
            .collect();
        let items: Vec<ManagedRole> = store.roles_by_ids(&ctx, &ids).await?;
        return Ok(Json(ManagedRolePage {
            total: i64::try_from(items.len()).unwrap_or(100),
            items,
            next_cursor: None,
        }));
    }
    Ok(Json(
        store
            .roles(
                &ctx,
                &query.q,
                query.parent.as_deref(),
                query.cursor.as_deref(),
                query.limit.unwrap_or(40),
            )
            .await?,
    ))
}
#[utoipa::path(get, path = "/org-admin/directory/groups", params(DirectoryQuery), responses((status = 200, body = AdGroupPage)), security(("session" = [])))]
pub async fn groups(
    State(state): State<AppState>,
    AuthContextExt(ctx): AuthContextExt,
    Query(query): Query<DirectoryQuery>,
) -> Result<Json<AdGroupPage>, ApiError> {
    Ok(Json(
        state
            .directory
            .admin()
            .groups(
                &ctx,
                &query.q,
                query.cursor.as_deref(),
                query.limit.unwrap_or(40),
                &parse_ou(query.parent.as_deref())?,
            )
            .await?,
    ))
}
#[utoipa::path(post, path = "/org-admin/directory/apply", request_body = DirectoryChangeSet, responses((status = 200, body = DirectoryOperation)), security(("session" = [])))]
pub async fn apply(
    State(state): State<AppState>,
    AuthContextExt(ctx): AuthContextExt,
    trace: TraceIdExt,
    Json(input): Json<DirectoryChangeSet>,
) -> Result<Json<DirectoryOperation>, ApiError> {
    Ok(Json(
        directory_admin::submit(&state, &ctx, &input, trace.as_deref()).await?,
    ))
}
#[utoipa::path(get, path = "/org-admin/directory/operations/{id}", params(("id" = Uuid, Path)), responses((status = 200, body = DirectoryOperation)), security(("session" = [])))]
pub async fn operation(
    State(state): State<AppState>,
    AuthContextExt(ctx): AuthContextExt,
    Path(id): Path<Uuid>,
) -> Result<Json<DirectoryOperation>, ApiError> {
    Ok(Json(state.directory.admin().operation(&ctx, id).await?))
}
#[utoipa::path(post, path = "/org-admin/directory/operations/{id}/retry", request_body = DirectoryRetryInput, params(("id" = Uuid, Path)), responses((status = 200, body = DirectoryOperation)), security(("session" = [])))]
pub async fn retry(
    State(state): State<AppState>,
    AuthContextExt(ctx): AuthContextExt,
    trace: TraceIdExt,
    Path(id): Path<Uuid>,
    Json(input): Json<DirectoryRetryInput>,
) -> Result<Json<DirectoryOperation>, ApiError> {
    let store = state.directory.admin();
    let _lease = store.lease(&ctx).await?.ok_or(ApiError::Conflict)?;
    if store.operation(&ctx, id).await?.error_code.as_deref() == Some("execution_interrupted")
        && !input.recovery_confirmed
    {
        return Err(ApiError::invalid_input(
            "運用者が以前の実行を停止し、外部サービスの反映状態を確認してから再実行してください",
        ));
    }
    Ok(Json(
        store
            .retry_operation(&ctx, id, trace.as_deref(), input.recovery_confirmed)
            .await?,
    ))
}
#[utoipa::path(get, path = "/org-admin/directory/connection", responses((status = 200, body = LdapConnectionStatus)), security(("session" = [])))]
pub async fn connection(
    State(state): State<AppState>,
    AuthContextExt(ctx): AuthContextExt,
) -> Result<Json<LdapConnectionStatus>, ApiError> {
    let kc = KeycloakAdmin::from_config(&state.http, &state.config.auth).map_err(keycloak_error)?;
    let mut status = kc.ldap_connection(&ctx).await.map_err(keycloak_error)?;
    status.recovery_required = state
        .directory
        .admin()
        .connection_recovery_required(&ctx)
        .await?;
    Ok(Json(status))
}
#[utoipa::path(put, path = "/org-admin/directory/connection", request_body = LdapConnectionInput, responses((status = 200, body = LdapConnectionStatus)), security(("session" = [])))]
pub async fn save_connection(
    State(state): State<AppState>,
    AuthContextExt(ctx): AuthContextExt,
    trace: TraceIdExt,
    Json(input): Json<LdapConnectionInput>,
) -> Result<Json<LdapConnectionStatus>, ApiError> {
    validate(&state.config.auth, &input)?;
    Ok(Json(
        crate::directory_admin::connection::save(&state, &ctx, &input, trace.as_deref()).await?,
    ))
}
#[utoipa::path(post, path = "/org-admin/directory/connection/test", request_body = LdapConnectionInput, responses((status = 200, body = bool)), security(("session" = [])))]
pub async fn test_connection(
    State(state): State<AppState>,
    AuthContextExt(ctx): AuthContextExt,
    Json(input): Json<LdapConnectionInput>,
) -> Result<Json<bool>, ApiError> {
    validate(&state.config.auth, &input)?;
    let store = state.directory.admin();
    let _lease = store.lease(&ctx).await?.ok_or(ApiError::Conflict)?;
    let kc = KeycloakAdmin::from_config(&state.http, &state.config.auth).map_err(keycloak_error)?;
    kc.test_ldap(&ctx, &input).await.map_err(keycloak_error)?;
    Ok(Json(true))
}
#[utoipa::path(post, path = "/org-admin/directory/sync", request_body = DirectorySyncInput, responses((status = 200, body = DirectoryOperation)), security(("session" = [])))]
pub async fn sync(
    State(state): State<AppState>,
    AuthContextExt(ctx): AuthContextExt,
    trace: TraceIdExt,
    Json(input): Json<DirectorySyncInput>,
) -> Result<Json<DirectoryOperation>, ApiError> {
    Ok(Json(
        directory_admin::submit_sync(&state, &ctx, input.idempotency_key, trace.as_deref()).await?,
    ))
}
#[utoipa::path(post, path = "/org-admin/directory/auto-plan", request_body = AutoPlanInput, responses((status = 200, body = DirectoryPlan)), security(("session" = [])))]
pub async fn auto_plan(
    State(state): State<AppState>,
    AuthContextExt(ctx): AuthContextExt,
    Json(input): Json<AutoPlanInput>,
) -> Result<Json<DirectoryPlan>, ApiError> {
    if matches!(input.hierarchy, AutoHierarchy::Group) {
        let kc =
            KeycloakAdmin::from_config(&state.http, &state.config.auth).map_err(keycloak_error)?;
        if !kc
            .ldap_connection(&ctx)
            .await
            .map_err(keycloak_error)?
            .preserve_group_inheritance
        {
            return Err(ApiError::invalid_input("接続設定でグループ階層の保持を有効にして、再同期してください。複数親がある場合はOUを使用します"));
        }
    }
    Ok(Json(
        directory_admin::plan::create(&state, &ctx, input).await?,
    ))
}

#[utoipa::path(post, path = "/org-admin/directory/import-plan", request_body = DirectoryImportInput, responses((status = 200, body = DirectoryPlan)), security(("session" = [])))]
pub async fn import_plan(
    State(state): State<AppState>,
    AuthContextExt(ctx): AuthContextExt,
    Json(input): Json<DirectoryImportInput>,
) -> Result<Json<DirectoryPlan>, ApiError> {
    Ok(Json(
        directory_admin::import::create(&state, &ctx, input).await?,
    ))
}
#[utoipa::path(get, path = "/org-admin/directory/operations", responses((status = 200, body = [DirectoryOperation])), security(("session" = [])))]
pub async fn operations(
    State(state): State<AppState>,
    AuthContextExt(ctx): AuthContextExt,
) -> Result<Json<Vec<DirectoryOperation>>, ApiError> {
    Ok(Json(state.directory.admin().operations(&ctx).await?))
}

fn parse_ou(path: Option<&str>) -> Result<Vec<String>, ApiError> {
    let path: Vec<String> = path
        .map_or_else(|| Ok(vec![]), serde_json::from_str)
        .map_err(|_| ApiError::BadRequest("OUパスが不正です".into()))?;
    if path.len() > 128 || path.iter().any(|s| s.len() > 4096) {
        return Err(ApiError::BadRequest("OUパスが長すぎます".into()));
    }
    Ok(path)
}
#[utoipa::path(get, path = "/org-admin/directory/folders", params(DirectoryQuery), responses((status = 200, body = storage::directory::admin::AdFolderPage)), security(("session" = [])))]
pub async fn folders(
    State(state): State<AppState>,
    AuthContextExt(ctx): AuthContextExt,
    Query(query): Query<DirectoryQuery>,
) -> Result<Json<storage::directory::admin::AdFolderPage>, ApiError> {
    Ok(Json(
        state
            .directory
            .admin()
            .folders(
                &ctx,
                &parse_ou(query.parent.as_deref())?,
                query.cursor.as_deref(),
            )
            .await?,
    ))
}

#[utoipa::path(post, path = "/org-admin/directory/preview", request_body = DirectoryChangeSet, responses((status = 200, body = DirectoryPlan)), security(("session" = [])))]
pub async fn preview(
    State(state): State<AppState>,
    AuthContextExt(ctx): AuthContextExt,
    Json(input): Json<DirectoryChangeSet>,
) -> Result<Json<DirectoryPlan>, ApiError> {
    Ok(Json(
        directory_admin::preview::create(&state, &ctx, &input).await?,
    ))
}

#[utoipa::path(get, path = "/org-admin/directory/members", params(DirectoryQuery), responses((status = 200, body = storage::directory::admin::DirectoryMemberPage)), security(("session" = [])))]
pub async fn members(
    State(state): State<AppState>,
    AuthContextExt(ctx): AuthContextExt,
    Query(query): Query<DirectoryQuery>,
) -> Result<Json<storage::directory::admin::DirectoryMemberPage>, ApiError> {
    Ok(Json(
        state
            .directory
            .admin()
            .members(&ctx, &query.q, query.cursor.as_deref())
            .await?,
    ))
}
