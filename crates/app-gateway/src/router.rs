//! ゲートウェイのルータと二重ゲート middleware（Task 9.6・design §4.3/§4.6）。
//!
//! 第2リスナ（別オリジン）に載せる `Router`。全ルートに二重ゲートを一律適用する:
//! ①JWKS トークン検証 → ②ルート→必要スコープの宣言的マップ → ③`granted_scopes` 突合
//! （同意失効の即時反映）→ ④ハンドラ内 per-call OpenFGA（呼出ユーザーの ReBAC）。
//! 全ての許可/拒否を監査へ残す（拒否は security タグ付き・trace_id 貫通）。

use std::sync::Arc;

use authz::{AuthContext, AuthzClient, Principal, PrincipalKind};
use axum::{
    extract::{MatchedPath, State},
    http::{header::AUTHORIZATION, Request, StatusCode},
    middleware::Next,
    response::{IntoResponse, Json, Response},
    routing::get,
    Extension, Router,
};
use serde_json::json;
use storage::audit::{AuditEntry, AuditRecorder, Decision};

use crate::{
    notification::NotificationStore,
    ports::RagPort,
    scope_map::{required_scope_for, RouteScope},
    token::{verify_gateway_token, GatewayIdentity, GatewayTokenConfig, KeyResolver},
    usage, AppInstallation, AppInstallationStore, GatewayError,
};

/// 能力アダプタ（Task 9.8）の委譲先チョークポイント束。
///
/// ゲートウェイは権限判定を自前で持たない——`DataStore` / `StorageService` / [`RagPort`] の
/// 内部にある per-call OpenFGA（第4ゲート）へ呼出ユーザーの [`AuthContext`] を渡すだけ。
#[derive(Clone)]
pub struct CapabilityDeps {
    /// 利用量計上（`app_capability_usage`）と outbox 覗き見に使う。ハンドラの生 SQL 禁止。
    pub db: sqlx::PgPool,
    pub storage: Arc<storage::StorageService>,
    pub data: Arc<data::DataStore>,
    pub fsms: Arc<data::FsmStore>,
    pub rag: Arc<dyn RagPort>,
    pub notifications: NotificationStore,
}

/// ゲートウェイの共有状態（第2リスナの `Router` へ載せる）。
#[derive(Clone)]
pub struct GatewayState {
    pub installations: AppInstallationStore,
    pub keys: Arc<dyn KeyResolver>,
    pub token_cfg: GatewayTokenConfig,
    /// per-call OpenFGA（ハンドラの第4ゲート）に使う認可クライアント。
    pub authz: Arc<dyn AuthzClient>,
    pub audit: AuditRecorder,
    /// tenant クレームを必須にするか（SaaS `multi` は true・fail-closed）。
    ///
    /// `multi` テナンシーで true にすると、tenant クレームの無いトークンは `default_tenant` へ
    /// フォールバックせず拒否する（他テナントの既定テナントへ紛れ込むのを防ぐ）。`single`
    /// （オンプレ/cell）は false でフォールバックを許す。
    pub require_tenant_claim: bool,
    /// single テナントのフォールバック（`require_tenant_claim=false` のときのみ使う）。
    pub default_tenant: String,
    pub default_org: String,
    /// 能力アダプタの委譲先（Task 9.8）。
    pub caps: CapabilityDeps,
}

/// 二重ゲートを通過した呼出コンテキスト（ハンドラは `Extension` で読む）。
#[derive(Clone)]
pub struct GatewayCtx {
    /// 呼出**ユーザー**の AuthContext（per-call OpenFGA はこの主体で評価する）。
    pub auth: AuthContext,
    pub identity: GatewayIdentity,
    pub installation: AppInstallation,
}

impl IntoResponse for GatewayError {
    fn into_response(self) -> Response {
        let status = match self {
            GatewayError::Unauthenticated(_) => StatusCode::UNAUTHORIZED,
            GatewayError::Forbidden(_) => StatusCode::FORBIDDEN,
            GatewayError::NotFound => StatusCode::NOT_FOUND,
            GatewayError::Invalid(_) => StatusCode::BAD_REQUEST,
            GatewayError::Conflict(_) => StatusCode::CONFLICT,
            GatewayError::Upstream(_) => StatusCode::BAD_GATEWAY,
            GatewayError::Internal(_) => StatusCode::INTERNAL_SERVER_ERROR,
        };
        (status, Json(json!({ "error": self.to_string() }))).into_response()
    }
}

/// ゲートウェイの `Router` を組む（全ルートに二重ゲートを適用）。
///
/// 能力アダプタは [`crate::routes::capability_router`] にルートを足し、[`crate::scope_map`] にも
/// 対応スコープ行を追加する（両者の欠落は middleware が fail-closed で拒否する）。
pub fn build_gateway_router(state: GatewayState) -> Router {
    Router::new()
        .route("/gw/whoami", get(whoami))
        .merge(crate::routes::capability_router())
        .layer(axum::middleware::from_fn_with_state(
            state.clone(),
            dual_gate,
        ))
        .with_state(state)
}

/// Bearer トークンを取り出す（`Authorization: Bearer <t>`）。
fn bearer(req: &Request<axum::body::Body>) -> Result<String, GatewayError> {
    let raw = req
        .headers()
        .get(AUTHORIZATION)
        .and_then(|v| v.to_str().ok())
        .ok_or_else(|| GatewayError::Unauthenticated("Authorization ヘッダがありません".into()))?;
    raw.strip_prefix("Bearer ")
        .filter(|t| !t.is_empty())
        .map(str::to_string)
        .ok_or_else(|| GatewayError::Unauthenticated("Bearer トークン形式が不正です".into()))
}

/// 二重ゲート middleware（①トークン ②スコープマップ ③granted_scopes 突合）。
///
/// ④の per-call OpenFGA はハンドラ側（能力アダプタ）が [`GatewayCtx::auth`] で評価する。
/// 成功応答（2xx）した Scoped ルートは利用量を (ユーザー×アプリ×能力×日) で計上する
/// （Task 9.8・ハンドラに散らさない単一計上点）。
async fn dual_gate(
    State(state): State<GatewayState>,
    matched: Option<MatchedPath>,
    mut req: Request<axum::body::Body>,
    next: Next,
) -> Response {
    // ルータに登録の無いパス（fallback）は MatchedPath を持たない → 404（500 にしない）。
    let Some(matched) = matched else {
        return GatewayError::NotFound.into_response();
    };
    let method = req.method().as_str().to_string();
    match authorize(&state, &matched, &mut req).await {
        Ok(ctx) => {
            record(
                &state,
                &ctx.auth,
                &ctx.installation,
                matched.as_str(),
                Decision::Allow,
            )
            .await;
            let usage_key = required_scope_for(&method, matched.as_str());
            let (tenant, org) = (ctx.auth.tenant_id.clone(), ctx.auth.org.clone());
            let (app_id, user_sub) = (ctx.installation.app_id, ctx.identity.user_sub.clone());
            req.extensions_mut().insert(ctx);
            let resp = next.run(req).await;
            if resp.status().is_success() {
                if let Some(RouteScope::Scoped(cap)) = usage_key {
                    // 計上失敗はリクエストを止めない（best-effort・欠落は tracing に残す）。
                    if let Err(e) =
                        usage::record_usage(&state.caps.db, &tenant, &org, app_id, &user_sub, cap)
                            .await
                    {
                        tracing::warn!(error = %e, capability = cap.as_str(), "利用量計上に失敗");
                    }
                }
            }
            resp
        }
        Err(e) => e.into_response(),
    }
}

/// 二重ゲートの本体（成功で [`GatewayCtx`]・失敗は監査 deny 後に Err）。
async fn authorize(
    state: &GatewayState,
    matched: &MatchedPath,
    req: &mut Request<axum::body::Body>,
) -> Result<GatewayCtx, GatewayError> {
    let method = req.method().as_str().to_string();
    let route = matched.as_str().to_string();

    // ② ルート→必要スコープ（未宣言は fail-closed で拒否＝到達不能）。
    let requirement = required_scope_for(&method, &route)
        .ok_or_else(|| GatewayError::Forbidden("未宣言のルートです".into()))?;

    // ① トークン検証（JWKS・aud=gateway・azp）。
    let token = bearer(req)?;
    let identity = verify_gateway_token(&token, &*state.keys, &state.token_cfg).await?;

    // テナント解決。multi では tenant クレーム必須（欠落は fail-closed で拒否＝既定テナントへ
    // 紛れ込ませない）。single はフォールバックを許す。
    let tenant = match identity.tenant.clone() {
        Some(t) if !t.is_empty() => t,
        _ if state.require_tenant_claim => {
            return Err(GatewayError::Unauthenticated(
                "tenant クレームがありません".into(),
            ));
        }
        _ => state.default_tenant.clone(),
    };
    let auth = AuthContext::new(
        Principal {
            kind: PrincipalKind::User,
            id: identity.user_sub.clone(),
            email: None,
            groups: vec![],
            roles: vec![],
            tenant_id: Some(tenant.clone()),
        },
        state.default_org.clone(),
        tenant.clone(),
    );

    // ③ インストール解決（azp→granted_scopes・revoked/不在は 403＝同意失効の即時反映）。
    let installation = state
        .installations
        .resolve_active_by_client(&tenant, &identity.client_id)
        .await?
        .ok_or_else(|| GatewayError::Forbidden("有効なインストールがありません".into()))?;

    // スコープ要件: granted_scopes ∩ token_scopes に必要スコープが含まれること。
    if let RouteScope::Scoped(need) = requirement {
        let granted = installation
            .granted_scopes
            .iter()
            .any(|g| g == need.as_str());
        let in_token = identity.token_scopes.contains(&need);
        if !(granted && in_token) {
            record(state, &auth, &installation, &route, Decision::Deny).await;
            return Err(GatewayError::Forbidden(format!(
                "スコープ {} が付与されていません",
                need.as_str()
            )));
        }
    }

    Ok(GatewayCtx {
        auth,
        identity,
        installation,
    })
}

/// ゲートウェイ判定を監査へ残す（拒否は security タグ・trace_id は後続 PR12 で貫通強化）。
async fn record(
    state: &GatewayState,
    auth: &AuthContext,
    installation: &AppInstallation,
    route: &str,
    decision: Decision,
) {
    let meta = json!({
        "route": route,
        "app_id": installation.app_id,
        "app_name": installation.app_name,
        "security": decision == Decision::Deny,
    });
    // 監査失敗はリクエストを止めない（best-effort・記録欠落は tracing に残す）。
    if let Err(e) = state
        .audit
        .record(
            auth,
            AuditEntry {
                action: "gateway.call",
                object_type: "miniapp",
                object_id: &installation.app_id.to_string(),
                decision,
                trace_id: None,
                metadata: meta,
            },
        )
        .await
    {
        tracing::warn!(error = %e, "ゲートウェイ監査の記録に失敗");
    }
}

/// 呼出主体の自己情報（app_id・granted_scopes・user sub）。能力スコープ不要。
async fn whoami(Extension(ctx): Extension<GatewayCtx>) -> Json<serde_json::Value> {
    Json(json!({
        "user_sub": ctx.identity.user_sub,
        "client_id": ctx.identity.client_id,
        "app_id": ctx.installation.app_id,
        "app_name": ctx.installation.app_name,
        "granted_scopes": ctx.installation.granted_scopes,
    }))
}
