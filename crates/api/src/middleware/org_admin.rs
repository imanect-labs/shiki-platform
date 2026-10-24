//! 組織管理者ゲート（管理コンソールの API・`/org-admin/*`・#553）。
//!
//! セッション middleware の**後**に走り、`organization:<tenant>|<org>#admin` を OpenFGA で
//! 確かめる。ルート表のポリシー（[`AccessPolicy::OrgAdmin`]）で一律に掛けるので、ハンドラは
//! 管理者チェックを持たない（チェックを散らさない・architecture-invariants）。
//!
//! admin は**閲覧権に連鎖しない**（PIT-65）。ここで通るのは管理 API だけで、中身の読み取りは
//! 従来どおり各チョークポイントの ReBAC が判定する。
//!
//! 拒否は監査に Deny で残す（`StorageService::require` と同じく、昇格の試みを追えるように）。
//!
//! [`AccessPolicy::OrgAdmin`]: crate::server::AccessPolicy::OrgAdmin

use authz::{AuthContext, Consistency, Principal, Relation};
use axum::{
    extract::{Request, State},
    middleware::Next,
    response::Response,
};
use serde_json::json;
use storage::audit::{AuditEntry, Decision};

use crate::{
    error::ApiError,
    extract::{OrgId, TenantId, TraceId},
    state::AppState,
};

/// 管理者でなければ 403 にする axum middleware。
pub async fn require_org_admin(
    State(state): State<AppState>,
    req: Request,
    next: Next,
) -> Result<Response, ApiError> {
    let ctx = context_of(&req)?;
    if !is_org_admin(&state, &ctx, Consistency::HigherConsistency).await? {
        let trace = req.extensions().get::<TraceId>().map(|t| t.0.clone());
        let object_id = format!("{}|{}", ctx.tenant_id, ctx.org);
        // 監査の失敗で応答は変えない（拒否は既に決まっている・欠落はログで顕在化する）。
        if let Err(e) = state
            .audit
            .record(
                &ctx,
                AuditEntry {
                    action: "org_admin.access",
                    object_type: "organization",
                    object_id: &object_id,
                    decision: Decision::Deny,
                    trace_id: trace.as_deref(),
                    metadata: json!({
                        "method": req.method().as_str(),
                        "path": req.uri().path(),
                    }),
                },
            )
            .await
        {
            tracing::error!(error = %e, "org admin deny audit failed");
        }
        return Err(ApiError::Forbidden);
    }
    Ok(next.run(req).await)
}

/// リクエストの認証主体から `AuthContext` を組む（セッション middleware が載せた値を読む）。
pub(crate) fn context_of(req: &Request) -> Result<AuthContext, ApiError> {
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
    let org = req
        .extensions()
        .get::<OrgId>()
        .map(|o| o.0.clone())
        .ok_or(ApiError::Unauthorized)?;
    Ok(AuthContext::new(principal, org, tenant_id))
}

/// 自組織の管理者か。
///
/// 管理操作のゲートは剥奪直後に通らないよう `HigherConsistency` で引く。メニューの表示可否
/// （`/me`）のような表示だけの用途は `MinimizeLatency` でよい（実際の操作はゲートが再判定する）。
pub(crate) async fn is_org_admin(
    state: &AppState,
    ctx: &AuthContext,
    consistency: Consistency,
) -> Result<bool, ApiError> {
    Ok(state
        .authz
        .check(
            &ctx.subject(),
            Relation::Admin,
            &ctx.ns().organization(&ctx.org),
            consistency,
        )
        .await?)
}
