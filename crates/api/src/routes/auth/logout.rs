//! `POST /auth/logout` — セッション破棄 + Cookie 破棄 + Keycloak SSO ログアウト URL 返却。
//!
//! 状態変更系のため double-submit CSRF を検証する（CSRF ヘッダ == CSRF Cookie == session）。

use axum::{extract::State, http::HeaderMap, Json};
use axum_extra::extract::cookie::CookieJar;
use serde::Serialize;

use super::{removal_cookie, session_tenant_scope, CSRF_HEADER};
use crate::{error::ApiError, state::AppState};

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
    let session_cfg = &state.config.session;
    let tenant_id = session_tenant_scope(&state.config.auth)?;

    let session_id = jar
        .get(&session_cfg.cookie_name)
        .map(|c| c.value().to_string());
    let csrf_cookie = jar
        .get(&session_cfg.csrf_cookie_name)
        .map(|c| c.value().to_string());
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
    let mut id_token_hint: Option<String> = None;
    if let Some(sid) = &session_id {
        if let Some(record) = state.sessions.get(&tenant_id, sid).await? {
            if csrf_header.as_deref() != Some(record.csrf_token.as_str()) {
                return Err(ApiError::Forbidden);
            }
            id_token_hint = record.id_token.clone();
            state.sessions.delete(&tenant_id, sid).await?;
        }
    }

    // end-session URL を構築（ブラウザのフル遷移で Keycloak セッションも終了させる）。
    let auth = &state.config.auth;
    let mut params: Vec<(&str, &str)> = vec![
        ("client_id", auth.client_id.as_str()),
        (
            "post_logout_redirect_uri",
            auth.post_logout_redirect_uri.as_str(),
        ),
    ];
    if let Some(hint) = id_token_hint.as_deref() {
        params.push(("id_token_hint", hint));
    }
    let end_session_url = reqwest::Url::parse_with_params(&auth.end_session_endpoint(), &params)
        .map_err(|e| ApiError::Internal(format!("end-session URL 構築に失敗: {e}")))?
        .to_string();

    // Cookie を破棄。
    let secure = session_cfg.secure;
    let jar = jar
        .add(removal_cookie(&session_cfg.cookie_name, secure))
        .add(removal_cookie(&session_cfg.csrf_cookie_name, secure));

    Ok((jar, Json(LogoutResponse { end_session_url })))
}
