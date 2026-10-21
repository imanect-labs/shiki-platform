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

/// テナントのワークフロー実行履歴 保持期間の設定リクエスト（#448）。
#[derive(Debug, Deserialize, ToSchema)]
pub struct TenantWorkflowRetentionRequest {
    /// 保持日数（1〜3650）。terminal になってからこの日数を過ぎた run 履歴を日次 GC が消す。
    ///
    /// **i64 で受けて範囲はハンドラで見る。** i32 で受けると `4000000000` のような桁溢れが
    /// `Json` 抽出の時点で 422 になり、範囲外は 400 という OpenAPI/運用ドキュメントの記述と
    /// 食い違う（同じ「範囲外」なのに値によって応答が変わる）。範囲自体は schema にも出して
    /// 生成クライアント側でも見えるようにする（受け口の型と許容範囲は別物）。
    #[schema(minimum = 1, maximum = 3650)]
    pub retention_days: i64,
}

/// テナントのワークフロー実行履歴の保持期間を設定する（#448）。
///
/// 起算は **run が terminal になった時刻**で、実行中の run は保持期間を跨いでも消えない。
/// 期限を過ぎた run と、その `step_execution` / `run_event` / `wait_subscription`（FK CASCADE）・
/// `effect_journal` を日次 GC が消す。既定は 90 日。
///
/// **短くする方向は即座に効く**（次回の日次 GC で、新しい期限を過ぎた履歴が消える）。監査要件で
/// 履歴が要る場合は縮める前に退避すること。
#[utoipa::path(
    put,
    path = "/admin/tenants/{tenant_id}/workflow-retention",
    params(("tenant_id" = String, Path, description = "テナント識別子")),
    request_body = TenantWorkflowRetentionRequest,
    responses(
        (status = 204, description = "保持期間を更新した"),
        (status = 400, description = "tenant_id または retention_days が不正（範囲外・桁溢れを含む）"),
        (status = 422, description = "リクエスト本文が JSON として不正（retention_days の欠落・型違い）"),
        (status = 401, description = "provisioner トークンが無効"),
        (status = 404, description = "active なテナントが存在しない"),
    ),
    security(("provisioner_token" = [])),
)]
pub async fn set_tenant_workflow_retention(
    State(state): State<AppState>,
    Path(tenant_id): Path<String>,
    Json(req): Json<TenantWorkflowRetentionRequest>,
) -> Result<StatusCode, ApiError> {
    validate_tenant_id(&tenant_id)
        .map_err(|_| ApiError::BadRequest("tenant_id に使用できない文字が含まれています".into()))?;
    // 範囲の正本は storage 側（DB の CHECK は `> 0` しか見ない）。i32 に収まらない値もここで
    // 400 に畳む（`i32::try_from` の失敗＝範囲外なので、検証と同じ扱いにする）。
    let days = i32::try_from(req.retention_days).map_err(|_| {
        ApiError::BadRequest(format!(
            "保持日数は {}〜{} 日で指定してください（指定値: {}）",
            storage::tenant::MIN_WORKFLOW_RETENTION_DAYS,
            storage::tenant::MAX_WORKFLOW_RETENTION_DAYS,
            req.retention_days
        ))
    })?;
    storage::tenant::validate_workflow_retention_days(days)
        .map_err(|e| ApiError::BadRequest(e.to_string()))?;
    let updated = state
        .tenants
        .set_workflow_retention_days(&tenant_id, days)
        .await?;
    if !updated {
        return Err(ApiError::NotFound);
    }
    tracing::info!(
        %tenant_id,
        retention_days = days,
        "ワークフロー実行履歴の保持期間を更新しました"
    );
    Ok(StatusCode::NO_CONTENT)
}

/// データ面の purge（FGA タプル → オブジェクト → DB 行）。
///
/// 構造化データ（式インデックス・FGA タプル・Task 9.2）→ storage（FGA タプル・オブジェクト・
/// **tenant_id を持つ全 DB 行**）→ RAG（Qdrant/Tantivy）の順に撤去する（#420）。
/// 前後の理由は本体のコメント参照（列挙が必要なものは前、索引の再生成入力を断つものは後）。
async fn purge_tenant_data(
    state: &AppState,
    tenant_id: &str,
    org: &str,
    actor: &str,
) -> Result<(), ApiError> {
    // ⚠️ 順序が重要（Codex P1・#420）: **サブシステムの purge を先に**回す。
    // `storage.purge_tenant` は tenant_id を持つ全テーブルを一掃するようになったため、これを先に
    // 走らせると `data.purge_tenant` が `data_table` を列挙できず、**data_table の FGA タプルと
    // 物理インデックスが撤去されないまま残る**（行だけ消えて副産物が孤児化する）。
    // 各サブシステムが自分の副産物（FGA タプル・物理索引・ベクタ/全文索引）を回収してから、
    // storage の汎用掃き出しを最後に走らせて「DB 行が 1 行も残らない」ことを保証する。
    // ① 構造化データを**先に**（Codex P1・#420）: storage の汎用掃き出しが data_table を消すと、
    //    DataStore が列挙できず FGA タプルと物理インデックスが孤児化する。
    let data_tables = state
        .data
        .purge_tenant(tenant_id)
        .await
        .map_err(|e| ApiError::Internal(format!("data purge: {e}")))?;
    if data_tables > 0 {
        tracing::info!(%tenant_id, data_tables, "tenant purge: 構造化データを撤去");
    }
    // ② storage: FGA タプル・オブジェクト・tenant_id を持つ全 DB 行（outbox / ジョブ含む）。
    state.storage.purge_tenant(tenant_id, org, actor).await?;
    // ③ RAG を**最後に**（Codex P1・#420）: RAG を先に消すと、まだ残っている
    //    `storage_event_outbox` のイベントや claim 済みジョブがその後に処理され、Qdrant/Tantivy の
    //    索引が**再作成**される。storage が outbox とジョブ行を消した後に走らせれば、索引を
    //    再生成する入力が残らない。rag_chunk 行は既に消えているが purge は冪等で、
    //    ベクタ/全文索引は tenant_id 指定で消すため行の有無に依存しない。
    state
        .rag_admin
        .purge_tenant(tenant_id)
        .await
        .map_err(|e| ApiError::Internal(format!("rag purge: {e}")))?;
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
