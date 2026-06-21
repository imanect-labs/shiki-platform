//! BFF 認証エンドポイント（`/auth/*`）。
//!
//! OIDC Authorization Code + PKCE の code 受け／token 交換をサーバ側で実施し、
//! ブラウザには不透明セッション Cookie のみを渡す（トークンは置かない）。
//! docs/auth/browser-token-strategy.md / roadmap phase-0 Task 0.11(#55)。

pub mod callback;
pub mod login;
pub mod logout;
pub mod session;

use axum_extra::extract::cookie::{Cookie, SameSite};
use base64::Engine;
use serde::{Deserialize, Serialize};

use crate::{config::AuthConfig, error::ApiError};

pub use callback::callback;
pub use login::login;
pub use logout::logout;
pub use session::session;

/// OIDC code フローの相関 Cookie 名（state/PKCE verifier を運ぶ・短命・httpOnly）。
const FLOW_COOKIE: &str = "shiki_oidc_flow";
/// 相関 Cookie の有効秒数（ログイン往復に十分・かつ短命）。
const FLOW_TTL_SECS: i64 = 600;

/// CSRF ヘッダ名（double-submit。CSRF Cookie の値と突合する）。
pub const CSRF_HEADER: &str = "x-csrf-token";

/// OIDC code フローの相関状態（callback で検証する。ブラウザに出るが httpOnly）。
#[derive(Debug, Serialize, Deserialize)]
struct FlowState {
    /// CSRF/リプレイ対策の state（callback の `state` クエリと一致必須）。
    state: String,
    /// PKCE code_verifier（token 交換でのみ使用）。
    verifier: String,
}

/// セッション/CSRF Cookie をセットする際の共通属性で Cookie を組み立てる。
fn build_cookie(
    name: &str,
    value: String,
    http_only: bool,
    secure: bool,
    max_age_secs: i64,
) -> Cookie<'static> {
    Cookie::build((name.to_string(), value))
        .http_only(http_only)
        .secure(secure)
        // OIDC の state/PKCE 相関 Cookie はトップレベル GET ナビゲーションで callback に
        // 戻る際に送出される必要があるため Lax 必須（ADR §7.1）。本体 Cookie も揃える。
        .same_site(SameSite::Lax)
        .path("/")
        .max_age(time::Duration::seconds(max_age_secs))
        .build()
}

/// Cookie を即時失効させる削除用 Cookie（同名・同 path・Max-Age 0）。
fn removal_cookie(name: &str, secure: bool) -> Cookie<'static> {
    build_cookie(name, String::new(), true, secure, 0)
}

/// 相関 Cookie（FlowState を base64(JSON) で格納）。
fn flow_cookie(flow: &FlowState, secure: bool) -> Cookie<'static> {
    let json = serde_json::to_vec(flow).expect("FlowState の serialize は無謬");
    let value = base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(json);
    build_cookie(FLOW_COOKIE, value, true, secure, FLOW_TTL_SECS)
}

/// 相関 Cookie をパースして FlowState を取り出す。
fn parse_flow(value: &str) -> Option<FlowState> {
    let bytes = base64::engine::general_purpose::URL_SAFE_NO_PAD
        .decode(value)
        .ok()?;
    serde_json::from_slice(&bytes).ok()
}

/// session 解決前のテナントスコープ。
///
/// `single`（オンプレ/cell・既定）は設定の固定値を使う（principal 非依存で解決可能）。
/// `multi`（SaaS）は host/サブドメインからの解決（SAAS.1）が必要で、起動時ガード
/// （config.validate）で multi 自体を拒否しているため、ここに到達しない設計。
fn session_tenant_scope(auth: &AuthConfig) -> Result<String, ApiError> {
    match auth.tenancy {
        crate::config::Tenancy::Single => auth
            .tenant_id
            .as_deref()
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .map(str::to_string)
            .ok_or_else(|| {
                tracing::error!("single-tenant モードで auth.tenant_id が空（設定ミス）");
                ApiError::Internal("tenant 設定不備".into())
            }),
        crate::config::Tenancy::Multi => Err(ApiError::Internal(
            "multi-tenant の session スコープは host ベース解決（SAAS.1）未実装".into(),
        )),
    }
}
