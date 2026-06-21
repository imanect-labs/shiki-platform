//! セッションミドルウェア（Cookie → セッション → `Principal`）。Bearer 版を置換する。
//!
//! - 不透明セッション Cookie からストアのセッションを引き、`Principal` + `tenant_id` を
//!   extension に載せる（`claims.rs` / `extract/*` / OpenFGA 経路は再利用）。
//! - access token が期限前ならサーバ側で refresh ローテーション（downstream の
//!   token-exchange が 401 にならないように）。refresh も失効ならセッション破棄→401。
//! - 状態変更系メソッドには double-submit CSRF を検証する。

use std::time::Duration;

use axum::{
    extract::{Request, State},
    http::Method,
    middleware::Next,
    response::Response,
};
use axum_extra::extract::cookie::CookieJar;

use crate::{
    error::ApiError,
    extract::TenantId,
    oidc,
    routes::auth::{session_tenant_scope, CSRF_HEADER},
    state::AppState,
};

/// 保護ルートに適用する axum middleware。セッション確立で `Principal` を載せる。
pub async fn require_session(
    State(state): State<AppState>,
    mut req: Request,
    next: Next,
) -> Result<Response, ApiError> {
    let jar = CookieJar::from_headers(req.headers());
    let session_cfg = &state.config.session;

    let session_id = jar
        .get(&session_cfg.cookie_name)
        .map(|c| c.value().to_string())
        .ok_or(ApiError::Unauthorized)?;
    let tenant_id = session_tenant_scope(&state.config.auth)?;

    let mut record = state
        .sessions
        .get(&tenant_id, &session_id)
        .await?
        .ok_or(ApiError::Unauthorized)?;

    // 状態変更系は double-submit CSRF を検証（ヘッダ == CSRF Cookie == session.csrf_token）。
    if is_state_changing(req.method()) {
        verify_csrf(
            &jar,
            &req,
            session_cfg.csrf_cookie_name.as_str(),
            &record.csrf_token,
        )?;
    }

    // access token が leeway 内なら refresh でローテーション更新する。
    let now = chrono::Utc::now().timestamp();
    if record.access_expires_at - now <= session_cfg.refresh_leeway_secs {
        match record.refresh_token.clone() {
            Some(refresh_token) => {
                match oidc::refresh_tokens(&state.http, &state.config.auth, &refresh_token).await {
                    Ok(tokens) => {
                        record.access_token = tokens.access_token;
                        if tokens.refresh_token.is_some() {
                            record.refresh_token = tokens.refresh_token;
                        }
                        if tokens.id_token.is_some() {
                            record.id_token = tokens.id_token;
                        }
                        record.access_expires_at = now + tokens.expires_in;
                        state
                            .sessions
                            .put(
                                &tenant_id,
                                &session_id,
                                &record,
                                Duration::from_secs(session_cfg.ttl_secs),
                            )
                            .await?;
                    }
                    Err(err) => {
                        // refresh も失効 → セッション破棄して再ログインへ。
                        tracing::info!(error = %err, "refresh 失敗。セッションを破棄");
                        let _ = state.sessions.delete(&tenant_id, &session_id).await;
                        return Err(ApiError::Unauthorized);
                    }
                }
            }
            None => {
                // refresh token が無く access も期限切れなら失効扱い。
                if record.access_expires_at <= now {
                    let _ = state.sessions.delete(&tenant_id, &session_id).await;
                    return Err(ApiError::Unauthorized);
                }
            }
        }
    }

    // principal + tenant_id を extension に載せる（AuthContext extractor が取り出す）。
    req.extensions_mut().insert(record.principal);
    req.extensions_mut().insert(TenantId(tenant_id));
    Ok(next.run(req).await)
}

/// 副作用のある HTTP メソッドか（CSRF 検証対象）。
fn is_state_changing(method: &Method) -> bool {
    matches!(
        *method,
        Method::POST | Method::PUT | Method::PATCH | Method::DELETE
    )
}

/// double-submit CSRF を検証する。ヘッダ == CSRF Cookie かつ session の値とも一致を要求。
fn verify_csrf(
    jar: &CookieJar,
    req: &Request,
    csrf_cookie_name: &str,
    session_csrf: &str,
) -> Result<(), ApiError> {
    let header = req.headers().get(CSRF_HEADER).and_then(|v| v.to_str().ok());
    let cookie = jar.get(csrf_cookie_name).map(|c| c.value());
    match (header, cookie) {
        (Some(h), Some(c)) if !h.is_empty() && h == c && h == session_csrf => Ok(()),
        _ => Err(ApiError::Forbidden),
    }
}
