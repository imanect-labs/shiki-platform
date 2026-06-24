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
    middleware::claims,
    oidc,
    routes::auth::{session_tenant_scope, CSRF_HEADER},
    session::{SessionRecord, CSRF_COOKIE, SESSION_COOKIE},
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
        .get(SESSION_COOKIE)
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
        verify_csrf(&jar, &req, &record.csrf_token)?;
    }

    // access token が leeway 内なら refresh でローテーション更新する。
    let now = chrono::Utc::now().timestamp();
    if record.access_expires_at - now <= session_cfg.refresh_leeway_secs {
        match record.refresh_token.clone() {
            Some(refresh_token) => {
                record =
                    refresh_session(&state, &tenant_id, &session_id, record, &refresh_token, now)
                        .await?;
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

/// access token を refresh でローテーションし、更新済みセッションを返す。
///
/// 失効・競合・一過性障害・claim 追従を区別して扱う:
/// - 成功: 新トークンを保存。principal を新 access token のクレームから再導出して claim 変化に追従。
///   保存は **update_if_present**（logout 中の削除をまたいでも復活させない＝即時失効を守る）。
/// - 4xx(invalid_grant): refresh token 失効。ただし並行リクエストが先にローテーション済みの
///   可能性があるため再読込し、有効なら継続。無ければセッション破棄→401。
/// - transport/5xx(一過性): セッションは破棄しない。access がまだ有効なら継続、期限切れなら 503 相当。
async fn refresh_session(
    state: &AppState,
    tenant_id: &str,
    session_id: &str,
    mut record: SessionRecord,
    refresh_token: &str,
    now: i64,
) -> Result<SessionRecord, ApiError> {
    let session_cfg = &state.config.session;
    match oidc::refresh_tokens(&state.http, &state.config.auth, refresh_token).await {
        Ok(tokens) => {
            // 新 access token のクレームから principal を再導出（IdP の group/roles 変更等に追従）。
            // backchannel TLS で得た信頼済みトークンのため署名再検証はしない（claims.rs 参照）。
            // 万一クレームを取り出せなければ fail-closed（古い principal で継続させない）。
            let claims = claims::decode_claims_insecure(&tokens.access_token)?;
            record.principal = claims::principal_from_claims(claims);
            record.access_token = tokens.access_token;
            if tokens.refresh_token.is_some() {
                record.refresh_token = tokens.refresh_token;
            }
            if tokens.id_token.is_some() {
                record.id_token = tokens.id_token;
            }
            record.access_expires_at = now + tokens.expires_in;

            let updated = state
                .sessions
                .update_if_present(
                    tenant_id,
                    session_id,
                    &record,
                    Duration::from_secs(session_cfg.ttl_secs),
                )
                .await?;
            if !updated {
                // 飛行中に logout 等で削除された。復活させず失効として扱う。
                return Err(ApiError::Unauthorized);
            }
            Ok(record)
        }
        Err(err) if err.is_client_error() => {
            // invalid_grant。並行リクエストが先にローテーション済みなら再読込で継続。
            match state.sessions.get(tenant_id, session_id).await? {
                Some(fresh) if fresh.access_expires_at - now > session_cfg.refresh_leeway_secs => {
                    Ok(fresh)
                }
                _ => {
                    tracing::info!(error = %err, "refresh token 失効。セッションを破棄");
                    let _ = state.sessions.delete(tenant_id, session_id).await;
                    Err(ApiError::Unauthorized)
                }
            }
        }
        Err(err) => {
            // 一過性障害（transport/5xx）。セッションは破棄しない。
            if record.access_expires_at > now {
                // access はまだ有効。今回は更新を諦めて現行トークンで継続。
                tracing::warn!(error = %err, "refresh 一過性失敗。現行 access で継続");
                Ok(record)
            } else {
                tracing::warn!(error = %err, "refresh 一過性失敗かつ access 期限切れ");
                Err(ApiError::Internal(format!("token refresh 一時失敗: {err}")))
            }
        }
    }
}

/// 副作用のある HTTP メソッドか（CSRF 検証対象）。
fn is_state_changing(method: &Method) -> bool {
    matches!(
        *method,
        Method::POST | Method::PUT | Method::PATCH | Method::DELETE
    )
}

/// double-submit CSRF を検証する。ヘッダ == CSRF Cookie かつ session の値とも一致を要求。
fn verify_csrf(jar: &CookieJar, req: &Request, session_csrf: &str) -> Result<(), ApiError> {
    let header = req.headers().get(CSRF_HEADER).and_then(|v| v.to_str().ok());
    let cookie = jar.get(CSRF_COOKIE).map(|c| c.value());
    match (header, cookie) {
        (Some(h), Some(c)) if !h.is_empty() && h == c && h == session_csrf => Ok(()),
        _ => Err(ApiError::Forbidden),
    }
}
