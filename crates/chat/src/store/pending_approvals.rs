//! 自分の承認を待っている run の一覧（ホームの「今日のこと」・#965）。
//!
//! 承認できるのは run の actor 本人だけ（[`ChatStore::submit_approval`] の confused-deputy 対策）
//! なので、一覧は **自分が actor の run** に限る。SQL で tenant / org / actor / 承認待ちに絞って
//! 1 本で引き（pre-filter）、返す前に thread への editor 権限を OpenFGA でまとめて確かめる
//! （post-filter・承認 API と同じ relation）。共有を外された thread の承認待ちは出さない。

#[allow(clippy::wildcard_imports)]
use super::*;

use authz::{AuthContext, CheckTuple, Consistency, Relation};
use chrono::{DateTime, Utc};
use uuid::Uuid;

use crate::model::PendingApproval;
use crate::ChatError;

/// 1 回に返す件数の上限（ホームに出すのは数件。全件取得はしない）。
const MAX_PENDING: i64 = 20;

type PendingRow = (
    Uuid,
    String,
    Uuid,
    String,
    String,
    Option<DateTime<Utc>>,
    DateTime<Utc>,
    Option<String>,
);

impl ChatStore {
    /// 自分の承認を待っている run を、承認を求めた時刻の新しい順に返す（最大 `limit` 件）。
    pub async fn list_pending_approvals(
        &self,
        ctx: &AuthContext,
        limit: i64,
    ) -> Result<Vec<PendingApproval>, ChatError> {
        let limit = limit.clamp(1, MAX_PENDING);
        // pending_approval は status を waiting_approval にする直前に書かれる（sink → approver）。
        // 両方そろった run だけを「承認待ち」とみなす。理由は承認要求イベントから引く（run には
        // 載っていない・ホームの 1 行に添えるだけなので先頭 200 文字まで）。ツールの入力は引かない。
        let rows: Vec<PendingRow> = sqlx::query_as(
            "SELECT r.thread_id, t.title, r.run_id, \
                    r.pending_approval->>'tool_call_id', r.pending_approval->>'tool_name', \
                    (r.pending_approval->>'requested_at')::timestamptz, r.updated_at, \
                    left(e.payload->>'reason', 200) \
             FROM generation_run r \
             JOIN thread t ON t.tenant_id = r.tenant_id AND t.id = r.thread_id \
                          AND t.deleted_at IS NULL \
             LEFT JOIN LATERAL ( \
                 SELECT ge.payload FROM generation_event ge \
                 WHERE ge.tenant_id = r.tenant_id AND ge.run_id = r.run_id \
                   AND ge.type = 'approval_requested' \
                   AND ge.payload->>'tool_call_id' = r.pending_approval->>'tool_call_id' \
                 ORDER BY ge.seq DESC LIMIT 1 \
             ) e ON true \
             WHERE r.tenant_id = $1 AND r.org = $2 AND r.actor = $3 \
               AND r.status = 'waiting_approval' AND r.pending_approval IS NOT NULL \
               AND r.pending_approval ? 'tool_call_id' AND r.pending_approval ? 'tool_name' \
             ORDER BY r.updated_at DESC, r.run_id DESC \
             LIMIT $4",
        )
        .bind(&ctx.tenant_id)
        .bind(&ctx.org)
        .bind(&ctx.principal.id)
        .bind(limit)
        .fetch_all(&self.db)
        .await
        .map_err(|e| ChatError::Internal(format!("db: {e}")))?;
        if rows.is_empty() {
            return Ok(Vec::new());
        }

        // post-filter: 承認 API と同じく thread の editor を要る（共有の剥奪は即時に効かせる）。
        let checks: Vec<CheckTuple> = rows
            .iter()
            .map(|r| CheckTuple {
                subject: ctx.subject(),
                relation: Relation::Editor,
                object: ctx.ns().thread(&r.0.to_string()),
            })
            .collect();
        let allowed = self
            .authz
            .batch_check(&checks, Consistency::HigherConsistency)
            .await
            .map_err(|e| ChatError::Internal(e.to_string()))?;

        Ok(rows
            .into_iter()
            .zip(allowed)
            .filter_map(|(r, ok)| ok.then_some(r))
            .map(
                |(
                    thread_id,
                    thread_title,
                    run_id,
                    tool_call_id,
                    tool_name,
                    requested,
                    updated,
                    reason,
                )| {
                    PendingApproval {
                        thread_id,
                        thread_title,
                        run_id,
                        tool_call_id,
                        tool_name,
                        reason,
                        // 要求時刻が読めない（旧形式）ときは run の更新時刻で代える。
                        requested_at: requested.unwrap_or(updated),
                    }
                },
            )
            .collect())
    }
}
