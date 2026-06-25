//! `GET /auth/session` — ログイン状態の確認（401 を出さずに真偽を返す）。

use axum::{extract::State, Json};
use axum_extra::extract::cookie::CookieJar;
use serde::Serialize;

use crate::{
    error::ApiError,
    session::{decode_session_cookie, SESSION_COOKIE},
    state::AppState,
};

#[derive(Debug, Serialize)]
pub struct SessionStatus {
    pub authenticated: bool,
}

/// セッション Cookie が有効なセッションを指すかを返す（UI の出し分け用）。
pub async fn session(
    State(state): State<AppState>,
    jar: CookieJar,
) -> Result<Json<SessionStatus>, ApiError> {
    // require_session と同じ「使えるか」を返す: access がまだ有効、または refresh で
    // 継続可能（refresh token 保持）なセッションのみ authenticated=true とする。
    // これをしないと、access 期限切れ＋refresh 無しの死にセッションを true と誤報告し、
    // 次の保護ルートで即 401 になる不整合を起こす。
    let now = chrono::Utc::now().timestamp();
    // テナントスコープ＋ session id は Cookie から復元する（multi テナント対応）。
    let resolved = jar.get(SESSION_COOKIE).and_then(|c| {
        decode_session_cookie(c.value()).map(|(s, t)| (s.to_string(), t.to_string()))
    });
    let authenticated = match resolved {
        Some((session_id, tenant_id)) => match state.sessions.get(&tenant_id, &session_id).await? {
            Some(record) => {
                record.tenant_id == tenant_id
                    && (record.access_expires_at > now || record.refresh_token.is_some())
            }
            None => false,
        },
        None => false,
    };

    Ok(Json(SessionStatus { authenticated }))
}
