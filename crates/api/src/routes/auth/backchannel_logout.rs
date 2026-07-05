//! `POST /auth/backchannel-logout` — OIDC Back-Channel Logout の受け口（#91）。
//!
//! Keycloak がユーザーのセッション終了（ログアウト・**管理者による無効化/削除**）時に
//! `logout_token`（JWT）を form-encoded で POST する。これを検証し、対象セッションを
//! サーバ側で即時失効させることで、access token 寿命（最大 `accessTokenLifespan`）を待たずに
//! 退職者・侵害アカウントを遮断する。
//!
//! ブラウザ由来のリクエストではない（Cookie も CSRF も無い）ため public ルートに置くが、
//! logout_token の署名・iss・aud（RP=client_id）・logout イベント宣言・nonce 不在を検証する
//! ことで、通常の access/id token を提示しての誤用を弾く（[`verify_logout_token`]）。
//!
//! [`verify_logout_token`]: crate::middleware::auth::verify_logout_token

use std::time::Duration;

use axum::{extract::State, http::StatusCode, Form};
use serde::Deserialize;

use crate::{
    error::ApiError,
    middleware::auth::{verify_logout_token, LOGOUT_TOKEN_MAX_AGE_SECS},
    state::AppState,
};

#[derive(Debug, Deserialize)]
pub struct BackchannelLogoutForm {
    logout_token: String,
}

/// logout_token を検証し、対象セッションを失効させる。
///
/// - `sid` があれば当該 SSO セッションのみ失効（他デバイスは残す）。
/// - `sid` が無く `sub` のみなら当該ユーザーの全セッションを失効（無効化/削除シナリオ）。
///
/// OIDC BCL §2.8 に従い、成功は 200＋`Cache-Control: no-store`。検証失敗は 400。
pub async fn backchannel_logout(
    State(state): State<AppState>,
    Form(form): Form<BackchannelLogoutForm>,
) -> Result<(StatusCode, [(axum::http::HeaderName, &'static str); 1]), ApiError> {
    let claims = verify_logout_token(&state, &form.logout_token)
        .await
        .map_err(|e| {
            tracing::warn!(error = %e, "backchannel logout: logout_token 検証に失敗");
            ApiError::BadRequest("logout_token が不正です".into())
        })?;

    // リプレイ防止（OIDC BCL §2.6）: jti を鮮度窓＋スキュー分だけ記録し、既出なら再処理しない。
    // verify_logout_token が jti の存在を保証する。Keycloak の正当な再送（タイムアウト等）と
    // 攻撃的リプレイのどちらも「既に失効済み」で 200 を返す（べき等・失効漏れは起きない）。
    let jti = claims.jti.as_deref().unwrap_or_default();
    let replay_ttl = Duration::from_secs((LOGOUT_TOKEN_MAX_AGE_SECS * 2) as u64);
    if !state.sessions.register_jti(jti, replay_ttl).await? {
        tracing::info!(
            jti,
            "backchannel logout: リプレイ/再送を検知（処理済み・200 を返す）"
        );
        return Ok((
            StatusCode::OK,
            [(axum::http::header::CACHE_CONTROL, "no-store")],
        ));
    }

    // sid 優先（当該セッションのみ）。無ければ sub で全セッション失効。
    let revoked = match (&claims.sid, &claims.sub) {
        (Some(sid), _) => state.sessions.delete_by_sid(sid).await?,
        (None, Some(sub)) => state.sessions.delete_by_subject(sub).await?,
        // verify_logout_token が sub/sid のどちらかを保証するため到達しない。
        (None, None) => 0,
    };
    tracing::info!(
        sid = ?claims.sid,
        sub = ?claims.sub,
        revoked,
        "backchannel logout: セッションを失効"
    );

    Ok((
        StatusCode::OK,
        [(axum::http::header::CACHE_CONTROL, "no-store")],
    ))
}
