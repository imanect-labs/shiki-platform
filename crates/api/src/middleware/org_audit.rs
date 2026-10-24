//! 監査閲覧者ゲート（監査ログの API・`/org-audit/*`・#634）。
//!
//! セッション middleware の**後**に走り、`organization:<tenant>|<org>#audit_viewer` を OpenFGA で
//! 確かめる。ルート表のポリシー（[`AccessPolicy::OrgAuditor`]）で一律に掛けるので、ハンドラは
//! 判定を持たない（チェックを散らさない・architecture-invariants）。
//!
//! audit_viewer は admin と独立で、互いに含意しない（管理者でない監査担当者を置ける）。
//! 閲覧権にも連鎖しない（PIT-65）: ここで通るのは監査ログの行（ID・判定）だけで、ファイルや
//! 会話の中身は従来どおり各チョークポイントの ReBAC が判定する。
//!
//! 拒否は監査に Deny で残す（[`super::org_admin`] と同じく、昇格の試みを追えるように）。
//!
//! ブラウザからは同一オリジン（またはアドレスバーへの直接入力）の要求だけを通す。セッション
//! Cookie は `SameSite=Lax` なので、外部サイトのリンクから書き出しへ遷移させると本人の操作として
//! 「書き出した」記録が残ってしまう（読まれはしないが、監査の「誰が」が信用できなくなる）。
//! `Sec-Fetch-Site` を送らないクライアント（CLI 等）は Cookie を持たない限り通らないので見ない。
//!
//! [`AccessPolicy::OrgAuditor`]: crate::server::AccessPolicy::OrgAuditor

use authz::{AuthContext, Consistency, Relation};
use axum::{
    extract::{Request, State},
    http::HeaderMap,
    middleware::Next,
    response::Response,
};
use serde_json::json;
use storage::audit::AuditAction;
use storage::audit::{AuditEntry, Decision};

use super::org_admin::context_of;
use crate::{error::ApiError, extract::TraceId, state::AppState};

/// 監査閲覧者でなければ 403 にする axum middleware。
pub async fn require_org_auditor(
    State(state): State<AppState>,
    req: Request,
    next: Next,
) -> Result<Response, ApiError> {
    if !same_site_request(req.headers()) {
        return Err(ApiError::Forbidden);
    }
    let ctx = context_of(&req)?;
    if !is_org_auditor(&state, &ctx, Consistency::HigherConsistency).await? {
        let trace = req.extensions().get::<TraceId>().map(|t| t.0.clone());
        let object_id = format!("{}|{}", ctx.tenant_id, ctx.org);
        // 監査の失敗で応答は変えない（拒否は既に決まっている・欠落はログで顕在化する）。
        if let Err(e) = state
            .audit
            .record(
                &ctx,
                AuditEntry {
                    action: AuditAction::OrgAuditAccess,
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
            tracing::error!(error = %e, "org audit deny audit failed");
        }
        return Err(ApiError::Forbidden);
    }
    Ok(next.run(req).await)
}

/// 外部サイト発の要求でないか（`Sec-Fetch-Site` が無い・`same-origin`・`none` なら通す）。
fn same_site_request(headers: &HeaderMap) -> bool {
    match headers.get("sec-fetch-site").and_then(|v| v.to_str().ok()) {
        None => true,
        Some(v) => matches!(v, "same-origin" | "none"),
    }
}

/// 自組織の監査閲覧者か。
///
/// ゲートは剥奪直後に通らないよう `HigherConsistency` で引く。メニューの表示可否（`/me`）は
/// `MinimizeLatency` でよい（実際の閲覧はゲートが再判定する）。
pub(crate) async fn is_org_auditor(
    state: &AppState,
    ctx: &AuthContext,
    consistency: Consistency,
) -> Result<bool, ApiError> {
    Ok(state
        .authz
        .check(
            &ctx.subject(),
            Relation::AuditViewer,
            &ctx.ns().organization(&ctx.org),
            consistency,
        )
        .await?)
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::http::HeaderValue;

    fn headers(v: Option<&'static str>) -> HeaderMap {
        let mut h = HeaderMap::new();
        if let Some(v) = v {
            h.insert("sec-fetch-site", HeaderValue::from_static(v));
        }
        h
    }

    #[test]
    fn only_same_origin_browser_requests_pass() {
        assert!(same_site_request(&headers(None)));
        assert!(same_site_request(&headers(Some("same-origin"))));
        assert!(same_site_request(&headers(Some("none"))));
        assert!(!same_site_request(&headers(Some("cross-site"))));
        assert!(!same_site_request(&headers(Some("same-site"))));
    }
}
