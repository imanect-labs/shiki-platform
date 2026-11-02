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
    /// プロビジョンしたテーブルへ「追加だけ」（submitter・D-113）を付与するロール ID。追加でき、自分が
    /// 追加したレコードだけ読める。
    #[serde(default)]
    pub submitter_roles: Vec<String>,
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
                submitter_roles: req.submitter_roles,
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

/// インストール一覧（管理 UI 用）。呼ぶ人の組織が入れたアプリだけを返す（PIT-45・BUG-197）。
///
/// 各項目に `table_count`（アプリが持つ、呼ぶ人の組織の生きているテーブルの数）を添える。0 より多いアプリだけ、
/// 画面がテーブルを使える部署・グループを変える口を出す（GAP-68）。
///
/// 呼ぶ人がアプリの持ち主なら、自動実行（event・cron）の関数の直近の失敗を `function_failures`
/// （[`app_platform::AppFunctionFailure`] の配列・新しい順）に添える（失敗が無いときと、持ち主でないときは付けない・GAP-60）。
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
    let ids: Vec<Uuid> = items.iter().map(|i| i.app_id).collect();
    let counts = state.data.app_table_counts(&ctx, &ids).await?;
    // 失敗の添え物を読めない（OpenFGA の一時的な障害など）ときも、一覧は返す（添えないのは持ち主でないときと同じ
    // 見え方なので、明かすことは増えない）。
    let failures = match state.installs.function_failures(&ctx, &ids).await {
        Ok(failures) => failures,
        Err(e) => {
            tracing::warn!(error = %e, "自動実行の関数の失敗を読めません（一覧には添えません）");
            std::collections::HashMap::new()
        }
    };
    let items: Vec<serde_json::Value> = items
        .into_iter()
        .map(|i| {
            let count = counts.get(&i.app_id).copied().unwrap_or(0);
            let failed = failures.get(&i.app_id).cloned();
            let mut v = serde_json::to_value(i).unwrap_or_default();
            if let Some(obj) = v.as_object_mut() {
                obj.insert("table_count".into(), count.into());
                if let Some(failed) = failed {
                    obj.insert(
                        "function_failures".into(),
                        serde_json::to_value(failed).unwrap_or_default(),
                    );
                }
            }
            v
        })
        .collect();
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

/// アプリのテーブルと、それを使える部署・グループ（GAP-68・インストールとアンインストールができる人だけ）。
#[utoipa::path(
    get,
    path = "/apps/installations/{app_id}/roles",
    params(("app_id" = Uuid, Path, description = "アプリ（mini_app_code artifact）ID")),
    responses(
        (status = 200, description = "アプリのテーブルと、見られる・編集できる・追加だけの部署・グループ", body = app_platform::AppTableAccess),
        (status = 403, description = "アーティファクト owner でない"),
        (status = 404, description = "インストールが存在しない"),
    ),
    security(("session" = [])),
)]
pub async fn app_table_roles(
    State(state): State<AppState>,
    AuthContextExt(ctx): AuthContextExt,
    Path(app_id): Path<Uuid>,
) -> Result<Json<app_platform::AppTableAccess>, ApiError> {
    Ok(Json(state.installs.table_roles(&ctx, app_id).await?))
}

/// アプリのテーブルを使える部署・グループを足す・外す（GAP-68）。
///
/// 意味はインストールの要求の `viewer_roles`・`editor_roles`・`submitter_roles` と同じで、アプリのすべてのテーブルに
/// 効く。足す部署・グループは同じテナントのディレクトリに居ること。変えた内容は監査に残る。
#[utoipa::path(
    patch,
    path = "/apps/installations/{app_id}/roles",
    params(("app_id" = Uuid, Path, description = "アプリ（mini_app_code artifact）ID")),
    request_body = app_platform::AppTableRolesChange,
    responses(
        (status = 200, description = "変えた後のアプリのテーブルと部署・グループ", body = app_platform::AppTableAccess),
        (status = 400, description = "部署・グループの id が不正・見つからない・足して外す食い違い・件数の上限・テーブルが無い"),
        (status = 403, description = "アーティファクト owner でない"),
        (status = 404, description = "インストールが存在しない"),
    ),
    security(("session" = [])),
)]
pub async fn update_app_table_roles(
    State(state): State<AppState>,
    AuthContextExt(ctx): AuthContextExt,
    trace: TraceIdExt,
    Path(app_id): Path<Uuid>,
    Json(change): Json<app_platform::AppTableRolesChange>,
) -> Result<Json<app_platform::AppTableAccess>, ApiError> {
    Ok(Json(
        state
            .installs
            .update_table_roles(&ctx, app_id, change, trace.as_deref())
            .await?,
    ))
}

/// アプリ利用量（capability＋llm・user×app 集計・Task 9.15）。
#[utoipa::path(
    get,
    path = "/apps/{app_id}/usage",
    params(("app_id" = Uuid, Path, description = "アプリ（mini_app_code artifact）ID")),
    responses(
        (status = 200, description = "利用量集計"),
        (status = 403, description = "アーティファクト owner でない"),
    ),
    security(("session" = [])),
)]
pub async fn app_usage(
    State(state): State<AppState>,
    AuthContextExt(ctx): AuthContextExt,
    Path(app_id): Path<Uuid>,
) -> Result<Json<app_platform::AppUsage>, ApiError> {
    let usage = state.app_usage.app_usage(&ctx, app_id).await?;
    Ok(Json(usage))
}

/// レジストリ一覧（インストール UI 用・呼ぶ人の組織が publish したエントリ・BUG-197）。
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
///
/// `tenant_id`/`org` はリクエスト（ボディ/クエリ）由来なので信頼せず、テナント作成
/// （`routes::admin`）と同じ `authz::validate_tenant_id` で検証してから組み立てる
/// （FGA 識別子 `<type>:<tenant>|<local>`・`app_trusted_key` 行の名前空間を崩させない）。
fn provisioner_ctx(
    actor: &str,
    tenant_id: &str,
    org: Option<&str>,
) -> Result<authz::AuthContext, ApiError> {
    authz::validate_tenant_id(tenant_id)
        .map_err(|_| ApiError::BadRequest("tenant_id に使用できない文字が含まれています".into()))?;
    if let Some(org) = org {
        authz::validate_tenant_id(org)
            .map_err(|_| ApiError::BadRequest("org に使用できない文字が含まれています".into()))?;
    }
    Ok(authz::AuthContext::new(
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
    ))
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
    let ctx = provisioner_ctx(&actor.0, &req.tenant_id, req.org.as_deref())?;
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
    let ctx = provisioner_ctx(&actor.0, &q.tenant_id, None)?;
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
    let ctx = provisioner_ctx(&actor.0, &q.tenant_id, None)?;
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
        r(
            "/apps/installations/{app_id}/roles",
            &["GET", "PATCH"],
            Session,
            || get(app_table_roles).patch(update_app_table_roles),
        ),
        r("/apps/registry/import", &["POST"], Session, || {
            post(import_app)
        }),
        r("/apps/registry", &["GET"], Session, || get(list_registry)),
        r("/apps/{app_id}/usage", &["GET"], Session, || get(app_usage)),
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn provisioner_ctx_accepts_valid_tenant_and_defaults_org() {
        let ctx = provisioner_ctx("prov", "a-corp", None).expect("valid");
        assert_eq!(ctx.tenant_id, "a-corp");
        assert_eq!(ctx.org, "a-corp");
        let ctx = provisioner_ctx("prov", "a-corp", Some("sales")).expect("valid");
        assert_eq!(ctx.org, "sales");
    }

    #[test]
    fn provisioner_ctx_rejects_forbidden_tenant_id() {
        // 名前空間区切り・構造文字・空・予約名はいずれも 400（fail-closed）。
        for bad in ["", "a|b", "a:b", "a#b", "a@b", "a/b", "a b", ".", ".."] {
            assert!(
                matches!(
                    provisioner_ctx("prov", bad, None),
                    Err(ApiError::BadRequest(_))
                ),
                "tenant_id={bad:?} は拒否されるべき"
            );
        }
    }

    #[test]
    fn provisioner_ctx_rejects_forbidden_org() {
        for bad in ["", "x|y", "x/y"] {
            assert!(
                matches!(
                    provisioner_ctx("prov", "a-corp", Some(bad)),
                    Err(ApiError::BadRequest(_))
                ),
                "org={bad:?} は拒否されるべき"
            );
        }
    }
}
