//! 監査ログのハッシュチェーンの検証（`GET /org-audit/verify`・#828）。
//!
//! 監査閲覧者だけが使える（ルート表の `AccessPolicy::OrgAuditor`）。範囲は `AuthContext` の
//! tenant_id ＋ org のチェーン。1 回で確かめる行数に上限を置き、続きは `after_id` で確かめる
//! （画面が末尾まで繰り返す）。検証したことも監査に残し、残せなければ結果を返さない。

use axum::{
    extract::{Query, State},
    Json,
};
use serde::Deserialize;
use serde_json::json;
use storage::audit::{AuditAction, AuditEntry, Decision};
use storage::audit_chain::ChainReport;
use utoipa::IntoParams;

use crate::{
    error::ApiError,
    extract::{AuthContextExt, TraceIdExt},
    state::AppState,
};

/// 1 回の呼び出しで確かめる行数の上限（30 秒の要求タイムアウトに収める）。
pub const VERIFY_MAX_ROWS: u64 = 50_000;

/// 検証の位置。
#[derive(Debug, Default, Deserialize, IntoParams)]
#[into_params(parameter_in = Query)]
pub struct VerifyQuery {
    /// 前の呼び出しの `last_id`（続きから確かめる）。省略時はチェーンの先頭から。
    pub after_id: Option<i64>,
}

/// 監査ログのハッシュチェーンを確かめる（実データを変える操作の行だけ）。
#[utoipa::path(
    get, path = "/org-audit/verify", params(VerifyQuery),
    responses(
        (status = 200, description = "検証の結果（complete=false なら last_id から続きを確かめる）", body = ChainReport),
        (status = 403, description = "監査閲覧者ではない"),
        (status = 404, description = "after_id がこのチェーンの行ではない"),
    ),
    security(("session" = [])),
)]
pub async fn verify_chain(
    State(state): State<AppState>,
    AuthContextExt(ctx): AuthContextExt,
    trace: TraceIdExt,
    Query(q): Query<VerifyQuery>,
) -> Result<Json<ChainReport>, ApiError> {
    let report = state
        .audit
        .chain_verifier()
        .verify(&ctx, q.after_id, VERIFY_MAX_ROWS)
        .await
        .map_err(|e| match e {
            storage::StorageError::NotFound => ApiError::NotFound,
            e => ApiError::Internal(format!("audit verify: {e}")),
        })?;
    let object_id = format!("{}|{}", ctx.tenant_id, ctx.org);
    state
        .audit
        .record(
            &ctx,
            AuditEntry {
                action: AuditAction::OrgAuditVerify,
                object_type: "organization",
                object_id: &object_id,
                decision: Decision::Allow,
                trace_id: trace.as_deref(),
                metadata: json!({
                    "after_id": q.after_id,
                    "checked": report.checked,
                    "last_id": report.last_id,
                    "complete": report.complete,
                    "broken": report.broken.as_ref().map(|b| json!({"id": b.id, "reason": b.reason})),
                }),
            },
        )
        .await
        .map_err(|e| {
            tracing::error!(error = %e, "監査ログの検証の記録に失敗（結果は返さない）");
            ApiError::Internal("監査ログに記録できなかったため表示を中止しました".into())
        })?;
    Ok(Json(report))
}
