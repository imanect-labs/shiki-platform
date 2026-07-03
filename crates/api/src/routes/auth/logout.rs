//! `POST /auth/logout` — セッション破棄 + Cookie 破棄 + Keycloak SSO ログアウト URL 返却。
//!
//! 状態変更系のため double-submit CSRF を検証する（CSRF ヘッダ == CSRF Cookie == session）。

use axum::{extract::State, http::HeaderMap, Json};
use axum_extra::extract::cookie::CookieJar;
use serde::Serialize;

use super::{removal_cookie, CSRF_HEADER};
use crate::{
    error::ApiError,
    session::{decode_session_cookie, CSRF_COOKIE, SESSION_COOKIE},
    state::AppState,
};

#[derive(Debug, Serialize)]
pub struct LogoutResponse {
    /// ブラウザがフル遷移して Keycloak SSO ログアウトを完了するための URL。
    pub end_session_url: String,
}

/// ログアウト。CSRF 検証 → セッション削除 → Cookie 破棄 → end-session URL を返す。
pub async fn logout(
    State(state): State<AppState>,
    headers: HeaderMap,
    jar: CookieJar,
) -> Result<(CookieJar, Json<LogoutResponse>), ApiError> {
    // テナントスコープ＋ session id は Cookie から復元する（multi テナント対応）。
    let resolved = jar.get(SESSION_COOKIE).and_then(|c| {
        decode_session_cookie(c.value()).map(|(s, t)| (s.to_string(), t.to_string()))
    });
    let csrf_cookie = jar.get(CSRF_COOKIE).map(|c| c.value().to_string());
    let csrf_header = headers
        .get(CSRF_HEADER)
        .and_then(|v| v.to_str().ok())
        .map(str::to_string);

    // double-submit CSRF: ヘッダと Cookie が一致しなければ拒否（強制ログアウト CSRF 防止）。
    match (&csrf_header, &csrf_cookie) {
        (Some(h), Some(c)) if h == c && !h.is_empty() => {}
        _ => return Err(ApiError::Forbidden),
    }

    // セッションが存在すれば、CSRF を session 値とも突合してから削除する。
    if let Some((session_id, tenant_id)) = &resolved {
        if let Some(record) = state.sessions.get(tenant_id, session_id).await? {
            if csrf_header.as_deref() != Some(record.csrf_token.as_str()) {
                return Err(ApiError::Forbidden);
            }
            state.sessions.delete(tenant_id, session_id).await?;
        }
    }

    // end-session URL を構築（ブラウザのフル遷移で Keycloak セッションも終了させる）。
    // BFF 不変条件によりトークンはブラウザに出さないため、`id_token_hint`（OIDC トークン）は
    // **使わない**。client_id + post_logout_redirect_uri で行う（Keycloak が確認画面を挟む場合がある）。
    let auth = &state.config.auth;
    let params: [(&str, &str); 2] = [
        ("client_id", auth.client_id.as_str()),
        (
            "post_logout_redirect_uri",
            auth.post_logout_redirect_uri.as_str(),
        ),
    ];
    let end_session_url = reqwest::Url::parse_with_params(&auth.end_session_endpoint(), &params)
        .map_err(|e| ApiError::Internal(format!("end-session URL 構築に失敗: {e}")))?
        .to_string();

    // Cookie を破棄。
    let secure = state.config.session.secure;
    let jar = jar
        .add(removal_cookie(SESSION_COOKIE, secure))
        .add(removal_cookie(CSRF_COOKIE, secure));

    Ok((jar, Json(LogoutResponse { end_session_url })))
}
