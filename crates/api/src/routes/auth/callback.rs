//! `GET /auth/callback` — code を受け、サーバ側で token 交換しセッションを作る。

use std::time::Duration;

use axum::{
    extract::{Query, State},
    response::Redirect,
};
use axum_extra::extract::cookie::CookieJar;
use serde::Deserialize;

use super::{build_cookie, parse_flow, removal_cookie, FLOW_COOKIE};
use crate::{
    error::ApiError,
    extract::resolve_tenant_id,
    middleware::{auth::verify_access_token, claims},
    oidc,
    session::{new_opaque_token, SessionRecord},
    state::AppState,
};

#[derive(Debug, Deserialize)]
pub struct CallbackQuery {
    code: Option<String>,
    state: Option<String>,
    error: Option<String>,
}

/// OIDC callback。state 検証 → token 交換 → access token 検証 → セッション発行。
pub async fn callback(
    State(state): State<AppState>,
    jar: CookieJar,
    Query(query): Query<CallbackQuery>,
) -> Result<(CookieJar, Redirect), ApiError> {
    if let Some(err) = query.error {
        tracing::warn!(%err, "IdP がエラーを返した");
        return Err(ApiError::Unauthorized);
    }
    let code = query.code.ok_or(ApiError::Unauthorized)?;
    let returned_state = query.state.ok_or(ApiError::Unauthorized)?;

    // 相関 Cookie（state + PKCE verifier）を検証。state 不一致は CSRF/リプレイとして拒否。
    let flow = jar
        .get(FLOW_COOKIE)
        .and_then(|c| parse_flow(c.value()))
        .ok_or(ApiError::Unauthorized)?;
    if flow.state != returned_state {
        tracing::warn!("OIDC state 不一致（CSRF/リプレイの疑い）");
        return Err(ApiError::Unauthorized);
    }

    // code↔token 交換はサーバ側で実施（ブラウザにトークンを出さない）。
    let tokens =
        oidc::exchange_code(&state.http, &state.config.auth, &code, &flow.verifier).await?;

    // 受領した access token を JWKS で検証してクレームを得る。
    let verified = verify_access_token(&state, &tokens.access_token).await?;
    let principal = claims::principal_from_claims(verified);
    let tenant_id = resolve_tenant_id(&principal, &state.config.auth)?;

    // セッション本体を作成（トークンはサーバ側のみに保持）。
    let session_id = new_opaque_token();
    let csrf_token = new_opaque_token();
    let now = chrono::Utc::now().timestamp();
    let record = SessionRecord {
        principal,
        tenant_id: tenant_id.clone(),
        access_token: tokens.access_token,
        refresh_token: tokens.refresh_token,
        id_token: tokens.id_token,
        access_expires_at: now + tokens.expires_in,
        csrf_token: csrf_token.clone(),
    };
    let ttl_secs = state.config.session.ttl_secs;
    state
        .sessions
        .put(
            &tenant_id,
            &session_id,
            &record,
            Duration::from_secs(ttl_secs),
        )
        .await?;

    // Cookie: セッション(httpOnly) + CSRF(JS 読取可) を発行し、相関 Cookie を破棄。
    let secure = state.config.session.secure;
    let max_age = ttl_secs as i64;
    let jar = jar
        .add(build_cookie(
            &state.config.session.cookie_name,
            session_id,
            true,
            secure,
            max_age,
        ))
        .add(build_cookie(
            &state.config.session.csrf_cookie_name,
            csrf_token,
            false,
            secure,
            max_age,
        ))
        .add(removal_cookie(FLOW_COOKIE, secure));

    Ok((jar, Redirect::to("/")))
}
