//! 組織管理者ゲート（管理コンソールの API・`/org-admin/*`・#553）。
//!
//! セッション middleware の**後**に走り、`organization:<tenant>|<org>#admin` を OpenFGA で
//! 確かめる。ルート表のポリシー（[`AccessPolicy::OrgAdmin`]）で一律に掛けるので、ハンドラは
//! 管理者チェックを持たない（チェックを散らさない・architecture-invariants）。
//!
//! admin は**閲覧権に連鎖しない**（PIT-65）。ここで通るのは管理 API だけで、中身の読み取りは
//! 従来どおり各チョークポイントの ReBAC が判定する。
//!
//! [`AccessPolicy::OrgAdmin`]: crate::server::AccessPolicy::OrgAdmin

use authz::{AuthContext, Consistency, Principal, Relation};
use axum::{
    extract::{Request, State},
    middleware::Next,
    response::Response,
};

use crate::{
    error::ApiError,
    extract::{resolve_org, TenantId},
    state::AppState,
};

/// 管理者でなければ 403 にする axum middleware。
pub async fn require_org_admin(
    State(state): State<AppState>,
    req: Request,
    next: Next,
) -> Result<Response, ApiError> {
    let ctx = context_of(&req)?;
    if !is_org_admin(&state, &ctx).await? {
        return Err(ApiError::Forbidden);
    }
    Ok(next.run(req).await)
}

/// リクエストの認証主体から `AuthContext` を組む（セッション middleware が載せた値を読む）。
fn context_of(req: &Request) -> Result<AuthContext, ApiError> {
    let principal = req
        .extensions()
        .get::<Principal>()
        .cloned()
        .ok_or(ApiError::Unauthorized)?;
    let tenant_id = req
        .extensions()
        .get::<TenantId>()
        .map(|t| t.0.clone())
        .ok_or(ApiError::Unauthorized)?;
    let org = resolve_org(&principal);
    Ok(AuthContext::new(principal, org, tenant_id))
}

/// 自組織の管理者か。剥奪直後に管理操作が通らないよう、強い整合性で引く。
pub(crate) async fn is_org_admin(state: &AppState, ctx: &AuthContext) -> Result<bool, ApiError> {
    Ok(state
        .authz
        .check(
            &ctx.subject(),
            Relation::Admin,
            &ctx.ns().organization(&ctx.org),
            Consistency::HigherConsistency,
        )
        .await?)
}
