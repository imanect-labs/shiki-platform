//! ゲートウェイのルータと二重ゲート middleware（Task 9.6・design §4.3/§4.6）。
//!
//! 第2リスナ（別オリジン）に載せる `Router`。全ルートに二重ゲートを一律適用する:
//! ①JWKS トークン検証 → ②ルート→必要スコープの宣言的マップ → ③`granted_scopes` 突合
//! （同意失効の即時反映）→ ④ハンドラ内 per-call OpenFGA（呼出ユーザーの ReBAC）。
//! 全ての許可/拒否を監査へ残す（拒否は security タグ付き・trace_id 貫通）。
//!
//! **呼び出す主体**: 利用者のトークン（B1 の PKCE・B2 が交換したもの）は利用者（`user`）。B2 の自動実行の
//! サービスアカウントのトークン（`azp` が有効なインストールの B2 で、`sub` がそのサービスアカウントの利用者）だけを
//! アプリの主体（`miniapp:<tenant>|<app_id>`・BUG-183）として扱い、テナントと組織はインストールから決める。アプリの
//! 主体が通れるルートはルート表の `app_principal` のものだけ（アプリのテーブルと通知の記録）。
//!
//! **組織**: 利用者の経路も、組織はトークンではなくインストールから決め、呼ぶ人がその組織の `organization#member`
//! かを OpenFGA で確かめる（member でなければ 403・D-130）。アプリのトークンには所属グループを載せない。

use std::sync::Arc;
use storage::audit::AuditAction;

use authz::{AuthContext, AuthzClient, Consistency, Principal, PrincipalKind, Relation};
use axum::{
    extract::{MatchedPath, State},
    http::{header, header::AUTHORIZATION, HeaderMap, HeaderValue, Method, Request, StatusCode},
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
    scope_map::{required_scope_for, route_for, RouteScope},
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
    /// llm.invoke の委譲先（Task 9.9・未構成なら 502）。会計（llm_usage）はこの中。
    pub llm: Option<Arc<llm_gateway::LlmGateway>>,
    /// agent.invoke の委譲先（Task 9.9・実装は api 配線＝agent-core の重依存を持ち込まない）。
    pub agent: Arc<dyn crate::ports::AgentPort>,
    /// AI 日次予算の管理者キャップ（マイクロ USD・アプリ宣言との min が実効上限）。
    pub ai_daily_cap_usd_micros: i64,
    /// B2 関数起動の委譲先（Task 9.12・実装は api 配線）。
    pub functions: Arc<dyn crate::ports::FunctionPort>,
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
    /// `tenant_id` の解決規則（内部 API の `resolve_tenant_id` と単一定義・#808）。
    ///
    /// `single`（オンプレ/cell）は [`authz::TenantSource::Fixed`] で設定の固定値を使い、
    /// トークンの tenant クレームは**無視**する（クレームでテナントを選ばせない）。
    /// `multi`（SaaS）は [`authz::TenantSource::Claim`] で tenant クレームを必須にし、
    /// 欠落は既定テナントへフォールバックせず拒否する（fail-closed）。
    pub tenant_source: authz::TenantSource,
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

/// アプリの主体（自動実行）が、通さないルートを呼んだときの文（BUG-183）。
const APP_PRINCIPAL_DENIED: &str =
    "自動で動くアプリの処理が使えるのは、アプリのテーブルと通知だけです";

/// 呼ぶ人が、アプリをインストールした組織のメンバーでないときの文（D-130）。
const NOT_ORG_MEMBER: &str =
    "このアプリを使えるのは、アプリをインストールした組織のメンバーだけです";

/// 401 の固定の文言（理由を含めない）。
const UNAUTHENTICATED_MSG: &str =
    "認証に失敗しました。アクセストークンが無いか、無効または期限切れです";

impl IntoResponse for GatewayError {
    fn into_response(self) -> Response {
        let status = match self {
            GatewayError::Unauthenticated(_) => StatusCode::UNAUTHORIZED,
            GatewayError::Forbidden(_) => StatusCode::FORBIDDEN,
            GatewayError::NotFound => StatusCode::NOT_FOUND,
            GatewayError::Invalid(_) => StatusCode::BAD_REQUEST,
            GatewayError::Conflict(_) => StatusCode::CONFLICT,
            GatewayError::RateLimited(_) => StatusCode::TOO_MANY_REQUESTS,
            GatewayError::Upstream(_) => StatusCode::BAD_GATEWAY,
            GatewayError::Internal(_) => StatusCode::INTERNAL_SERVER_ERROR,
        };
        // 401 は理由（JWT の解析・署名検証・クレームの誤り）を返さない。内部の型名・ライブラリの英語の文言が
        // 混ざり（`JWT ヘッダ不正: JSON error: expected value…`）、トークンの組み立てを試す手掛かりにもなる
        // （3 回目の検証 3-B-5）。理由はログにだけ残し、本文は固定の構造化した形にする。
        if let GatewayError::Unauthenticated(reason) = &self {
            tracing::info!(reason = %reason, "gateway: 認証に失敗（401）");
            let mut resp = (
                status,
                Json(json!({
                    "status": 401,
                    "code": "unauthenticated",
                    "error": UNAUTHENTICATED_MSG,
                })),
            )
                .into_response();
            resp.headers_mut().insert(
                header::WWW_AUTHENTICATE,
                HeaderValue::from_static("Bearer error=\"invalid_token\""),
            );
            return resp;
        }
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
        // CORS は dual_gate より外側（preflight を Bearer 無しでも処理する）。
        .layer(axum::middleware::from_fn(cors))
        .with_state(state)
}

/// CORS 付与（B1 の opaque origin＝`Origin: null` からの能力呼び出しを許可する）。
///
/// ゲートウェイは **Bearer 専用（cookie/credentials なし）** なのでオリジンを反射しても
/// アンビエント権限は漏れない（CORS が守るのは cookie ベース認証＝ここには無い）。認可は
/// 二重ゲート（token∩granted∩ReBAC）が担う。preflight（OPTIONS）は `dual_gate` より外側で
/// 204 応答する（プリフライトは Bearer を運べないため）。
async fn cors(req: Request<axum::body::Body>, next: Next) -> Response {
    let origin = req.headers().get(header::ORIGIN).cloned();
    if req.method() == Method::OPTIONS {
        let mut resp = StatusCode::NO_CONTENT.into_response();
        apply_cors(resp.headers_mut(), origin.as_ref());
        return resp;
    }
    let mut resp = next.run(req).await;
    apply_cors(resp.headers_mut(), origin.as_ref());
    resp
}

fn apply_cors(headers: &mut HeaderMap, origin: Option<&HeaderValue>) {
    // credentials は付与しない（Bearer 専用）ため、オリジン反射で安全。Origin 欠落
    // （同一オリジン/サーバ間）は "null" を返す（害はない）。
    let allow = origin
        .cloned()
        .unwrap_or_else(|| HeaderValue::from_static("null"));
    headers.insert(header::ACCESS_CONTROL_ALLOW_ORIGIN, allow);
    headers.insert(
        header::ACCESS_CONTROL_ALLOW_METHODS,
        HeaderValue::from_static("GET, POST, PATCH, DELETE, OPTIONS"),
    );
    headers.insert(
        header::ACCESS_CONTROL_ALLOW_HEADERS,
        HeaderValue::from_static("authorization, content-type"),
    );
    // オリジンごとに応答が変わるためキャッシュ健全性のため Vary を付ける。
    headers.insert(header::VARY, HeaderValue::from_static("origin"));
    headers.insert(
        header::ACCESS_CONTROL_MAX_AGE,
        HeaderValue::from_static("600"),
    );
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
            // アプリの主体（自動実行）の分は、利用者と分けて記録する（サービスアカウントの id は記録しない・BUG-204）。
            let is_app = ctx.auth.principal.kind == PrincipalKind::MiniApp;
            req.extensions_mut().insert(ctx);
            let resp = next.run(req).await;
            if resp.status().is_success() {
                if let Some(RouteScope::Scoped(cap)) = usage_key {
                    let principal = if is_app {
                        usage::UsagePrincipal::App
                    } else {
                        usage::UsagePrincipal::User(&user_sub)
                    };
                    // 計上失敗はリクエストを止めない（best-effort・欠落は tracing に残す）。
                    if let Err(e) =
                        usage::record_usage(&state.caps.db, &tenant, &org, app_id, principal, cap)
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
    let entry = route_for(&method, &route)
        .ok_or_else(|| GatewayError::Forbidden("未宣言のルートです".into()))?;

    // ① トークン検証（JWKS・aud=gateway・azp）。
    let token = bearer(req)?;
    let identity = verify_gateway_token(&token, &*state.keys, &state.token_cfg).await?;

    // B2 の自動実行のサービスアカウントのトークンは、アプリの主体（BUG-183）。テナントと組織はトークンのクレームでは
    // なくインストールから決める（サービスアカウントの利用者には tenant の属性も所属も無い）。B2 が交換した利用者の
    // トークンは sub が利用者なので当たらず、下の利用者の経路を通る。
    if let Some(svc) = state
        .installations
        .resolve_service_identity(&identity.client_id, &identity.user_sub)
        .await?
    {
        let auth =
            AuthContext::for_miniapp(svc.tenant_id, svc.org, &svc.installation.app_id.to_string());
        if !entry.app_principal {
            record(state, &auth, &svc.installation, &route, Decision::Deny).await;
            return Err(GatewayError::Forbidden(APP_PRINCIPAL_DENIED.into()));
        }
        return require_scope(state, &route, entry.scope, auth, identity, svc.installation).await;
    }

    // テナント解決（内部 API と同じ規則）。multi では tenant クレーム必須（欠落は fail-closed で
    // 拒否＝既定テナントへ紛れ込ませない）。single は設定の固定値を使いクレームを無視する。
    let tenant = state
        .tenant_source
        .resolve(identity.tenant.as_deref())
        .map_err(|e| GatewayError::Unauthenticated(e.to_string()))?;

    // ③ インストール解決（azp→granted_scopes・revoked/不在は 403＝同意失効の即時反映）。
    let resolved = state
        .installations
        .resolve_active_by_client(&tenant, &identity.client_id)
        .await?
        .ok_or_else(|| GatewayError::Forbidden("有効なインストールがありません".into()))?;

    // 組織はトークンではなくインストールから決め、呼ぶ人がその組織のメンバー（OpenFGA の organization#member）かを
    // 確かめる（D-130）。アプリのトークンには所属グループを載せない（B1 ではトークンがアプリのコードに渡る）。member の
    // タプルはログインとセッションの更新が所属グループからそろえる（api の routes/auth/org_member）。外した直後から
    // 断るよう、強い整合で引く。
    let auth = AuthContext::new(
        Principal {
            kind: PrincipalKind::User,
            id: identity.user_sub.clone(),
            email: None,
            groups: vec![],
            roles: vec![],
            tenant_id: Some(tenant.clone()),
        },
        resolved.org,
        tenant,
    );
    let installation = resolved.installation;
    let member = state
        .authz
        .check(
            &auth.subject(),
            Relation::Member,
            &auth.ns().organization(&auth.org),
            Consistency::HigherConsistency,
        )
        .await
        .map_err(|e| GatewayError::Internal(format!("authz: {e}")))?;
    if !member {
        record(state, &auth, &installation, &route, Decision::Deny).await;
        return Err(GatewayError::Forbidden(NOT_ORG_MEMBER.into()));
    }

    require_scope(state, &route, entry.scope, auth, identity, installation).await
}

/// スコープ要件: granted_scopes ∩ token_scopes に必要スコープが含まれること（利用者・アプリの主体で共通）。
async fn require_scope(
    state: &GatewayState,
    route: &str,
    requirement: RouteScope,
    auth: AuthContext,
    identity: GatewayIdentity,
    installation: AppInstallation,
) -> Result<GatewayCtx, GatewayError> {
    if let RouteScope::Scoped(need) = requirement {
        let granted = installation
            .granted_scopes
            .iter()
            .any(|g| g == need.as_str());
        let in_token = identity.token_scopes.contains(&need);
        if !(granted && in_token) {
            record(state, &auth, &installation, route, Decision::Deny).await;
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
                action: AuditAction::GatewayCall,
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
///
/// `principal` は呼び出す主体の種類（利用者は `user`・自動実行のアプリの主体は `app`・BUG-183）。
async fn whoami(Extension(ctx): Extension<GatewayCtx>) -> Json<serde_json::Value> {
    let principal = match ctx.auth.principal.kind {
        PrincipalKind::MiniApp => "app",
        _ => "user",
    };
    Json(json!({
        "principal": principal,
        "user_sub": ctx.identity.user_sub,
        "client_id": ctx.identity.client_id,
        "app_id": ctx.installation.app_id,
        "app_name": ctx.installation.app_name,
        "granted_scopes": ctx.installation.granted_scopes,
    }))
}
