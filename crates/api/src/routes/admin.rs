//! テナント・プロビジョニング admin API（SAAS.2 / #87）。
//!
//! `/admin/*` は BFF セッションではなく **Bearer JWT（provisioner service account）** で
//! 認証する管理プレーン。単一の [`require_provisioner`] middleware が
//! JWT 検証（iss/aud/exp/JWKS）＋ `azp == auth.provisioner_client_id` を一律強制し、
//! ハンドラ個別のチェックを持たない（宣言的・集中 PEP）。
//! **config（provisioner 資格情報）が無ければルート自体を組み込まない**（fail-closed）。
//!
//! テナント作成/削除はテナント横断の操作のため `AuthContext` を取らず、対象 tenant_id を
//! 明示引数で受ける（アンビエントではなく明示スコープ）。

use axum::{
    extract::{Extension, Path, Request, State},
    http::{header::AUTHORIZATION, StatusCode},
    middleware::Next,
    response::Response,
    Json,
};
use rand::{distributions::Alphanumeric, Rng};
use serde::{Deserialize, Serialize};
use utoipa::ToSchema;

use crate::{
    error::ApiError, extract::validate_tenant_id, keycloak_admin::KeycloakAdmin,
    middleware::auth::verify_access_token, state::AppState,
};

/// 認証済み provisioner の識別子（監査 actor 用・#91 M-7）。`azp`（トークン発行 client）を
/// 運び、ハンドラが監査ログの actor 列に刻む。request extension で受け渡す。
#[derive(Debug, Clone)]
pub struct ProvisionerIdentity(pub String);

/// `/admin/*` の認証 middleware。Bearer JWT を検証し、`azp` が provisioner client と
/// 一致することを要求する。失敗はすべて 401（存在秘匿はしない: admin API は発見可能でよい）。
/// 検証済みの `azp` を [`ProvisionerIdentity`] として extension に載せ、監査の actor に使う。
pub async fn require_provisioner(
    State(state): State<AppState>,
    mut req: Request,
    next: Next,
) -> Result<Response, ApiError> {
    // config 未設定ならルートが組み込まれないため、ここに来る時点で Some のはずだが
    // 二重に fail-closed（設定が後から欠けても素通りさせない）。
    let Some((provisioner_id, _)) = state.config.auth.provisioner_credentials() else {
        return Err(ApiError::Unauthorized);
    };
    let token = req
        .headers()
        .get(AUTHORIZATION)
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.strip_prefix("Bearer "))
        .ok_or(ApiError::Unauthorized)?;
    let claims = verify_access_token(&state, token).await?;
    let azp = claims.azp.as_deref();
    if azp != Some(provisioner_id) {
        tracing::warn!(
            ?azp,
            "admin API: azp が provisioner client と不一致（拒否）"
        );
        return Err(ApiError::Unauthorized);
    }
    // 監査 actor 用に検証済み azp を運ぶ（`provisioner:<azp>` で通常ユーザー subject と区別）。
    req.extensions_mut()
        .insert(ProvisionerIdentity(format!("provisioner:{provisioner_id}")));
    Ok(next.run(req).await)
}

/// テナント作成リクエスト。
#[derive(Debug, Deserialize, ToSchema)]
pub struct CreateTenantRequest {
    /// テナント識別子（名前空間キー。`| : # @` 空白は不可）。
    pub tenant_id: String,
    /// 組織 slug（省略時は tenant_id と同値。Keycloak group 名になる）。
    #[serde(default)]
    pub org: Option<String>,
    pub display_name: String,
    /// 初期 admin ユーザーのメール。
    pub admin_email: String,
    /// 初期 admin の username（省略時は admin_email）。
    #[serde(default)]
    pub admin_username: Option<String>,
}

/// テナント作成応答。`temp_password` は**新規作成時のみ**返る（一度きり・保存されない）。
#[derive(Debug, Serialize, ToSchema)]
pub struct CreateTenantResponse {
    pub tenant_id: String,
    pub org: String,
    pub status: String,
    pub admin_user_id: String,
    /// 初期 admin の一時パスワード（初回ログインで変更必須）。既存ユーザーなら null。
    pub temp_password: Option<String>,
}

/// テナントを 1 操作で作成する（冪等・SAAS.2）。
///
/// tenant 行 upsert → Keycloak group/初期 admin → FGA org member タプル → directory 投入。
/// 各段は冪等で、途中失敗は同一リクエストの再実行で収束する。
#[utoipa::path(
    post,
    path = "/admin/tenants",
    request_body = CreateTenantRequest,
    responses(
        (status = 201, description = "テナントを作成した（既存なら現状を返す）", body = CreateTenantResponse),
        (status = 400, description = "不正な tenant_id / 入力"),
        (status = 401, description = "provisioner トークンが無効"),
    ),
    security(("provisioner_token" = [])),
)]
pub async fn create_tenant(
    State(state): State<AppState>,
    Extension(actor): Extension<ProvisionerIdentity>,
    Json(req): Json<CreateTenantRequest>,
) -> Result<(StatusCode, Json<CreateTenantResponse>), ApiError> {
    // tenant_id は FGA 識別子/オブジェクトキーの名前空間になるため、空と禁止文字を拒否
    // （resolve_tenant_id と同一ルール。空はパスで DELETE できない幽霊テナントを生む）。
    if req.tenant_id.trim().is_empty() {
        return Err(ApiError::BadRequest("tenant_id が空です".into()));
    }
    validate_tenant_id(&req.tenant_id)
        .map_err(|_| ApiError::BadRequest("tenant_id に使用できない文字が含まれています".into()))?;
    let org = req.org.clone().unwrap_or_else(|| req.tenant_id.clone());
    if org.trim().is_empty() || req.display_name.trim().is_empty() {
        return Err(ApiError::BadRequest("org / display_name が空です".into()));
    }
    // org は Keycloak group パス（`/{org}`）と FGA 識別子・runtime org 解決（先頭セグメント）に
    // 使われるため、tenant_id と同じ禁止文字ルール（`/` 含む）を適用する。
    validate_tenant_id(&org)
        .map_err(|_| ApiError::BadRequest("org に使用できない文字が含まれています".into()))?;
    let username = req
        .admin_username
        .clone()
        .unwrap_or_else(|| req.admin_email.clone());

    // 1. レジストリへ登録（tombstone の再利用は拒否）。
    let tenant = state
        .tenants
        .upsert_active(&req.tenant_id, &org, &req.display_name)
        .await?;

    // 2. Keycloak: group と初期 admin ユーザー（冪等）。
    let kc = KeycloakAdmin::from_config(&state.http, &state.config.auth)
        .map_err(|e| ApiError::Internal(format!("keycloak admin: {e}")))?;
    kc.ensure_group(&org)
        .await
        .map_err(|e| ApiError::Internal(format!("keycloak group: {e}")))?;
    let temp_password = generate_temp_password();
    let (admin_user_id, issued_password) = kc
        .ensure_tenant_admin(
            &req.tenant_id,
            &org,
            &username,
            &req.admin_email,
            &temp_password,
        )
        .await
        .map_err(|e| ApiError::Internal(format!("keycloak user: {e}")))?;

    // 3. FGA org member タプル（実行時と同じ ns 経路・冪等・監査つき）。
    state
        .storage
        .provision_tenant_admin(&req.tenant_id, &org, &admin_user_id, &actor.0)
        .await?;
    // 4. ユーザーディレクトリ（共有相手検索）へ投入（冪等 upsert）。
    state
        .directory
        .upsert_user(
            &admin_user_id,
            &req.tenant_id,
            &org,
            &req.admin_email,
            &req.admin_email,
        )
        .await?;

    Ok((
        StatusCode::CREATED,
        Json(CreateTenantResponse {
            tenant_id: tenant.tenant_id,
            org: tenant.org,
            status: tenant.status.as_str().to_string(),
            admin_user_id,
            temp_password: issued_password,
        }),
    ))
}

/// テナントを削除する（冪等・SAAS.2）。
///
/// 撤去順は fail-safe: まずアクセス面（セッション・IdP ユーザー）を落としてから
/// データ面（FGA タプル・オブジェクト・DB 行）を purge する。途中失敗は再実行で収束。
/// audit_log は削除証跡として**保持**する（改竄検知チェーン）。tenant 行は tombstone。
#[utoipa::path(
    delete,
    path = "/admin/tenants/{tenant_id}",
    params(("tenant_id" = String, Path, description = "テナント識別子")),
    responses(
        (status = 204, description = "テナントを撤去した（不在/撤去済みでも成功）"),
        (status = 401, description = "provisioner トークンが無効"),
    ),
    security(("provisioner_token" = [])),
)]
pub async fn delete_tenant(
    State(state): State<AppState>,
    Extension(actor): Extension<ProvisionerIdentity>,
    Path(tenant_id): Path<String>,
) -> Result<StatusCode, ApiError> {
    validate_tenant_id(&tenant_id)
        .map_err(|_| ApiError::BadRequest("tenant_id に使用できない文字が含まれています".into()))?;
    // 1. deleting へ（不在なら冪等成功）。org は登録簿から得る（無ければ tenant_id と同値とみなす）。
    let tenant = state.tenants.mark_deleting(&tenant_id).await?;
    let org = tenant
        .as_ref()
        .map_or_else(|| tenant_id.clone(), |t| t.org.clone());

    // 2. セッション即時失効（新規ログインは次段の IdP ユーザー削除で塞ぐ）。
    let sessions = state.sessions.delete_tenant(&tenant_id).await?;
    tracing::info!(%tenant_id, sessions, "tenant purge: セッションを失効");

    // 3. Keycloak: tenant 属性一致ユーザーと org group を撤去（冪等）。
    let kc = KeycloakAdmin::from_config(&state.http, &state.config.auth)
        .map_err(|e| ApiError::Internal(format!("keycloak admin: {e}")))?;
    let users = kc
        .find_users_by_tenant(&tenant_id)
        .await
        .map_err(|e| ApiError::Internal(format!("keycloak users 検索: {e}")))?;
    for u in &users {
        kc.delete_user(&u.id)
            .await
            .map_err(|e| ApiError::Internal(format!("keycloak user 削除: {e}")))?;
    }
    // org group は**他の未削除テナントが同じ org slug を使っていない時のみ**削除する
    // （共有 org の group を消すと他テナントの groups claim / org 解決が壊れる）。
    if state.tenants.org_shared_by_others(&org, &tenant_id).await? {
        tracing::info!(%tenant_id, %org, "tenant purge: org group は他テナントと共有中のため保持");
    } else {
        kc.delete_group_by_name(&org)
            .await
            .map_err(|e| ApiError::Internal(format!("keycloak group 削除: {e}")))?;
    }
    // ステップ 2 と IdP ユーザー削除の間に完了したログインが新セッションを作る競合に備え、
    // IdP 側を塞いだ後にもう一度セッションを失効させる（belt-and-braces）。
    let late_sessions = state.sessions.delete_tenant(&tenant_id).await?;
    if late_sessions > 0 {
        tracing::info!(%tenant_id, late_sessions, "tenant purge: 競合セッションを追加失効");
    }

    // 4. データ面の purge（storage / RAG / 構造化データ。audit は保持）。
    purge_tenant_data(&state, &tenant_id, &org, &actor.0).await?;

    // 5. tombstone 化。
    state.tenants.mark_deleted(&tenant_id).await?;
    tracing::info!(%tenant_id, kc_users = users.len(), "tenant purge: 完了");
    Ok(StatusCode::NO_CONTENT)
}

/// テナントの自律エージェントポリシ設定リクエスト（#350）。
#[derive(Debug, Deserialize, ToSchema)]
pub struct TenantAutonomousPolicyRequest {
    /// 全自動（bypass）承認モードを許可するか（false で org 全体の利用を禁止する）。
    pub allow_bypass: bool,
}

/// テナントの自律エージェントポリシを設定する（org 管理者キャップ・#350）。
///
/// `allow_bypass=false` で当該テナントの全自動（bypass）承認モードを禁止する。チャット API は
/// 明示エラーで弾き、実行中に残っていた bypass は承認必須へクランプされる（黙って実行しない）。
#[utoipa::path(
    put,
    path = "/admin/tenants/{tenant_id}/autonomous-policy",
    params(("tenant_id" = String, Path, description = "テナント識別子")),
    request_body = TenantAutonomousPolicyRequest,
    responses(
        (status = 204, description = "ポリシを更新した"),
        (status = 400, description = "tenant_id が不正"),
        (status = 401, description = "provisioner トークンが無効"),
        (status = 404, description = "active なテナントが存在しない"),
    ),
    security(("provisioner_token" = [])),
)]
pub async fn set_tenant_autonomous_policy(
    State(state): State<AppState>,
    Path(tenant_id): Path<String>,
    Json(req): Json<TenantAutonomousPolicyRequest>,
) -> Result<StatusCode, ApiError> {
    validate_tenant_id(&tenant_id)
        .map_err(|_| ApiError::BadRequest("tenant_id に使用できない文字が含まれています".into()))?;
    let updated = state
        .tenants
        .set_autonomous_bypass(&tenant_id, req.allow_bypass)
        .await?;
    if !updated {
        return Err(ApiError::NotFound);
    }
    Ok(StatusCode::NO_CONTENT)
}

/// データ面の purge（FGA タプル → オブジェクト → DB 行）。
///
/// storage（node/artifact/secret 等）→ RAG（chunk/jobq/Qdrant/Tantivy）→
/// 構造化データ（data_table CASCADE・式インデックス・FGA タプル・Task 9.2）の順に撤去する。
async fn purge_tenant_data(
    state: &AppState,
    tenant_id: &str,
    org: &str,
    actor: &str,
) -> Result<(), ApiError> {
    state.storage.purge_tenant(tenant_id, org, actor).await?;
    state
        .rag_admin
        .purge_tenant(tenant_id)
        .await
        .map_err(|e| ApiError::Internal(format!("rag purge: {e}")))?;
    let data_tables = state
        .data
        .purge_tenant(tenant_id)
        .await
        .map_err(|e| ApiError::Internal(format!("data purge: {e}")))?;
    if data_tables > 0 {
        tracing::info!(%tenant_id, data_tables, "tenant purge: 構造化データを撤去");
    }
    Ok(())
}

/// 一時パスワードを生成する（24 文字英数）。初回ログインで変更必須（UPDATE_PASSWORD）。
fn generate_temp_password() -> String {
    rand::thread_rng()
        .sample_iter(&Alphanumeric)
        .take(24)
        .map(char::from)
        .collect()
}
