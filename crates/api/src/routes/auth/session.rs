//! `GET /auth/session` — ログイン状態の確認（401 を出さずに真偽を返す）。

use axum::{extract::State, Json};
use axum_extra::extract::cookie::CookieJar;
use serde::Serialize;

use super::session_tenant_scope;
use crate::{error::ApiError, state::AppState};

#[derive(Debug, Serialize)]
pub struct SessionStatus {
    pub authenticated: bool,
}

/// セッション Cookie が有効なセッションを指すかを返す（UI の出し分け用）。
pub async fn session(
    State(state): State<AppState>,
    jar: CookieJar,
) -> Result<Json<SessionStatus>, ApiError> {
    let session_cfg = &state.config.session;
    let tenant_id = session_tenant_scope(&state.config.auth)?;

    let authenticated = match jar.get(&session_cfg.cookie_name) {
        Some(cookie) => state
            .sessions
            .get(&tenant_id, cookie.value())
            .await?
            .is_some(),
        None => false,
    };

    Ok(Json(SessionStatus { authenticated }))
}
