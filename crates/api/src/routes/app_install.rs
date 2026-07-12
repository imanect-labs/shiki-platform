//! ミニアプリ配布 API — 同意インストール／アンインストール／オフライン import（Task 9.13b）。
//!
//! インストール認可は mini_app_code アーティファクトの **owner ReBAC**（InstallService 内で
//! 検証・human 確定判断）。信頼鍵（app_trusted_key）の登録/失効は /admin 面（provisioner
//! Bearer・`admin_route_decls` 側）にのみ載せる。

use app_platform::{InstallRequest, Installed, MiniAppManifest, RegistryEntry, TrustedKey};
use axum::{
    extract::{Path, State},
    Json,
};
use serde::{Deserialize, Serialize};
use utoipa::ToSchema;
use uuid::Uuid;

use crate::{
    error::ApiError,
    extract::{AuthContextExt, TraceIdExt},
    state::AppState,
};

/// インストール要求（同意内容）。
#[derive(Debug, Deserialize, ToSchema)]
pub struct InstallAppRequest {
    pub name: String,
    pub version: String,
    /// 同意して付与するスコープ（requested の部分集合・未知スコープは 400）。
    pub granted_scopes: Vec<String>,
    /// プロビジョンしたテーブルへ viewer を付与するロール ID。
    #[serde(default)]
    pub viewer_roles: Vec<String>,
    /// プロビジョンしたテーブルへ editor を付与するロール ID。
    #[serde(default)]
    pub editor_roles: Vec<String>,
}

/// インストール応答。
#[derive(Debug, Serialize, ToSchema)]
pub struct InstallAppResponse {
    pub app_id: Uuid,
    pub installed_version: String,
    pub granted_scopes: Vec<String>,
    pub table_ids: Vec<Uuid>,
    pub client_id_b1: Option<String>,
    pub client_id_b2: Option<String>,
}

impl From<Installed> for InstallAppResponse {
    fn from(i: Installed) -> Self {
        InstallAppResponse {
            app_id: i.installation.app_id,
            installed_version: i.installation.installed_version.clone(),
            granted_scopes: i.installation.granted_scopes.clone(),
            table_ids: i.table_ids,
            client_id_b1: i.installation.client_id_b1,
            client_id_b2: i.installation.client_id_b2,
        }
    }
}

/// アプリを同意インストールする（owner・テーブル自動プロビジョン＋client 登録）。
#[utoipa::path(
    post,
    path = "/apps/installations",
    request_body = InstallAppRequest,
    responses(
        (status = 200, description = "インストール完了", body = InstallAppResponse),
        (status = 400, description = "granted ⊄ requested・未知スコープ・信頼ティア不許可"),
        (status = 403, description = "アーティファクト owner でない・署名検証失敗"),
        (status = 404, description = "レジストリに存在しない"),
        (status = 409, description = "yank 済み・テーブル名衝突"),
    ),
    security(("session" = [])),
)]
pub async fn install_app(
    State(state): State<AppState>,
    AuthContextExt(ctx): AuthContextExt,
    trace: TraceIdExt,
    Json(req): Json<InstallAppRequest>,
) -> Result<Json<InstallAppResponse>, ApiError> {
    let installed = state
        .installs
        .install(
            &ctx,
            InstallRequest {
                name: req.name,
                version: req.version,
                granted_scopes: req.granted_scopes,
                viewer_roles: req.viewer_roles,
                editor_roles: req.editor_roles,
            },
            trace.as_deref(),
        )
        .await?;
    // B2 secret はサーバ側 secrets 保管（Task 9.12 で HostCall 付与）。応答へは返さない。
    if installed.client_secret_b2.is_some() {
        tracing::info!(app_id = %installed.installation.app_id, "B2 client secret を受領（secrets 保管は Task 9.12）");
    }
    Ok(Json(installed.into()))
}

/// インストール一覧（管理 UI 用）。
#[utoipa::path(
    get,
    path = "/apps/installations",
    responses((status = 200, description = "インストール一覧")),
    security(("session" = [])),
)]
pub async fn list_installations(
    State(state): State<AppState>,
    AuthContextExt(ctx): AuthContextExt,
) -> Result<Json<serde_json::Value>, ApiError> {
    let items = state
        .installs
        .installations()
        .list(&ctx)
        .await
        .map_err(|e| ApiError::Internal(format!("installations: {e}")))?;
    Ok(Json(serde_json::json!({ "items": items })))
}

/// アンインストール（失効→テーブル archive→tuple 撤去→client 無効化）。
#[utoipa::path(
    delete,
    path = "/apps/installations/{app_id}",
    params(("app_id" = Uuid, Path, description = "アプリ（mini_app_code artifact）ID")),
    responses(
        (status = 204, description = "アンインストール完了"),
        (status = 403, description = "アーティファクト owner でない"),
        (status = 404, description = "インストールが存在しない"),
    ),
    security(("session" = [])),
)]
pub async fn uninstall_app(
    State(state): State<AppState>,
    AuthContextExt(ctx): AuthContextExt,
    trace: TraceIdExt,
    Path(app_id): Path<Uuid>,
) -> Result<axum::http::StatusCode, ApiError> {
    state
        .installs
        .uninstall(&ctx, app_id, trace.as_deref())
        .await?;
    Ok(axum::http::StatusCode::NO_CONTENT)
}

/// レジストリ一覧（インストール UI 用・publish 済みエントリ）。
#[utoipa::path(
    get,
    path = "/apps/registry",
    responses((status = 200, description = "レジストリ一覧")),
    security(("session" = [])),
)]
pub async fn list_registry(
    State(state): State<AppState>,
    AuthContextExt(ctx): AuthContextExt,
) -> Result<Json<serde_json::Value>, ApiError> {
    let items = state
        .installs
        .registry()
        .list(&ctx, "mini_app_code", 200)
        .await?;
    Ok(Json(serde_json::json!({ "items": items })))
}

/// オフライン import 要求（署名付きマニフェスト・エアギャップ配布）。
#[derive(Debug, Deserialize, ToSchema)]
pub struct ImportAppRequest {
    pub manifest: MiniAppManifest,
    /// ed25519 署名（hex 128 文字・対象は canonical manifest digest）。
    pub signature_hex: String,
    /// 検証に使う信頼鍵の key_id（app_trusted_key・失効済みは不可）。
    pub key_id: String,
}

/// 署名付きマニフェストを検証して import（artifact 作成＋不変 publish）する。
#[utoipa::path(
    post,
    path = "/apps/registry/import",
    request_body = ImportAppRequest,
    responses(
        (status = 200, description = "レジストリ登録済み", body = RegistryEntry),
        (status = 403, description = "署名検証失敗・鍵不明/失効"),
        (status = 409, description = "同名 version は登録済み（不変 publish）"),
    ),
    security(("session" = [])),
)]
pub async fn import_app(
    State(state): State<AppState>,
    AuthContextExt(ctx): AuthContextExt,
    trace: TraceIdExt,
    Json(req): Json<ImportAppRequest>,
) -> Result<Json<RegistryEntry>, ApiError> {
    let signature = hex::decode(req.signature_hex.trim())
        .map_err(|_| ApiError::BadRequest("signature_hex が不正です".into()))?;
    let entry = state
        .installs
        .import_signed(
            &ctx,
            req.manifest,
            &signature,
            &req.key_id,
            trace.as_deref(),
        )
        .await?;
    Ok(Json(entry))
}

/// 信頼鍵の登録要求（/admin 面・provisioner Bearer・tenant を明示）。
#[derive(Debug, Deserialize, ToSchema)]
pub struct AddTrustedKeyRequest {
    pub tenant_id: String,
    #[serde(default)]
    pub org: Option<String>,
    pub key_id: String,
    /// ed25519 公開鍵（hex 64 文字）。
    pub public_key_hex: String,
    #[serde(default)]
    pub note: Option<String>,
}

/// provisioner を actor とする合成 AuthContext（信頼鍵はテナント横断の管理操作）。
fn provisioner_ctx(actor: &str, tenant_id: &str, org: Option<&str>) -> authz::AuthContext {
    authz::AuthContext::new(
        authz::Principal {
            kind: authz::PrincipalKind::User,
            id: actor.to_string(),
            email: None,
            groups: vec![],
            roles: vec![],
            tenant_id: Some(tenant_id.to_string()),
        },
        org.unwrap_or(tenant_id).to_string(),
        tenant_id.to_string(),
    )
}

/// 信頼鍵を登録する（/admin 面・provisioner Bearer）。
#[utoipa::path(
    post,
    path = "/admin/trusted-keys",
    request_body = AddTrustedKeyRequest,
    responses(
        (status = 200, description = "登録済み"),
        (status = 400, description = "鍵形式不正"),
        (status = 409, description = "key_id 重複"),
    ),
)]
pub(crate) async fn admin_add_trusted_key(
    State(state): State<AppState>,
    axum::Extension(actor): axum::Extension<crate::routes::admin::ProvisionerIdentity>,
    Json(req): Json<AddTrustedKeyRequest>,
) -> Result<Json<TrustedKey>, ApiError> {
    let ctx = provisioner_ctx(&actor.0, &req.tenant_id, req.org.as_deref());
    let key = hex::decode(req.public_key_hex.trim())
        .map_err(|_| ApiError::BadRequest("public_key_hex が不正です".into()))?;
    let added = state
        .installs
        .trusted_keys()
        .add(&ctx, &req.key_id, &key, req.note.as_deref())
        .await?;
    Ok(Json(added))
}

#[derive(Debug, Deserialize, utoipa::IntoParams)]
pub struct TrustedKeyTenantQuery {
    pub tenant_id: String,
}

/// 有効な信頼鍵の一覧（/admin 面）。
#[utoipa::path(
    get,
    path = "/admin/trusted-keys",
    params(TrustedKeyTenantQuery),
    responses((status = 200, description = "一覧")),
)]
pub(crate) async fn admin_list_trusted_keys(
    State(state): State<AppState>,
    axum::Extension(actor): axum::Extension<crate::routes::admin::ProvisionerIdentity>,
    axum::extract::Query(q): axum::extract::Query<TrustedKeyTenantQuery>,
) -> Result<Json<serde_json::Value>, ApiError> {
    let ctx = provisioner_ctx(&actor.0, &q.tenant_id, None);
    let items = state.installs.trusted_keys().list_active(&ctx).await?;
    Ok(Json(serde_json::json!({ "items": items })))
}

/// 信頼鍵を失効させる（/admin 面・行は監査のため残す）。
#[utoipa::path(
    delete,
    path = "/admin/trusted-keys/{key_id}",
    params(("key_id" = String, Path, description = "鍵識別子"), TrustedKeyTenantQuery),
    responses((status = 204, description = "失効済み"), (status = 404, description = "存在しない/失効済み")),
)]
pub(crate) async fn admin_revoke_trusted_key(
    State(state): State<AppState>,
    axum::Extension(actor): axum::Extension<crate::routes::admin::ProvisionerIdentity>,
    Path(key_id): Path<String>,
    axum::extract::Query(q): axum::extract::Query<TrustedKeyTenantQuery>,
) -> Result<axum::http::StatusCode, ApiError> {
    let ctx = provisioner_ctx(&actor.0, &q.tenant_id, None);
    state.installs.trusted_keys().revoke(&ctx, &key_id).await?;
    Ok(axum::http::StatusCode::NO_CONTENT)
}

/// 配布（インストール/import）ルート宣言。
pub(crate) fn app_install_route_decls() -> Vec<crate::server::RouteDecl> {
    use crate::server::AccessPolicy::Session;
    use axum::routing::{delete, get, post};
    let r = crate::server::RouteDecl::new;
    vec![
        r("/apps/installations", &["GET", "POST"], Session, || {
            get(list_installations).post(install_app)
        }),
        r("/apps/installations/{app_id}", &["DELETE"], Session, || {
            delete(uninstall_app)
        }),
        r("/apps/registry/import", &["POST"], Session, || {
            post(import_app)
        }),
        r("/apps/registry", &["GET"], Session, || get(list_registry)),
    ]
}

/// 信頼鍵管理（/admin 面）ルート宣言。
pub(crate) fn trusted_key_route_decls() -> Vec<crate::server::RouteDecl> {
    use crate::server::AccessPolicy::Provisioner;
    use axum::routing::{delete, get};
    let r = crate::server::RouteDecl::new;
    vec![
        r("/admin/trusted-keys", &["GET", "POST"], Provisioner, || {
            get(admin_list_trusted_keys).post(admin_add_trusted_key)
        }),
        r(
            "/admin/trusted-keys/{key_id}",
            &["DELETE"],
            Provisioner,
            || delete(admin_revoke_trusted_key),
        ),
    ]
}
