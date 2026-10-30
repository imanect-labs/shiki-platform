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
/// editor の確認にかける候補の数。確認で落ちた分だけ古い承認待ちが繰り上がるよう、`limit` より
/// 多めに引く（1 ユーザーの承認待ちは保留の上限で数件に収まる・batch_check 1 往復の 50 件）。
const CANDIDATES: i64 = 50;

#[derive(sqlx::FromRow)]
struct PendingRow {
    thread_id: Uuid,
    thread_title: String,
    origin_note_id: Option<Uuid>,
    origin_note_name: Option<String>,
    run_id: Uuid,
    tool_call_id: String,
    tool_name: String,
    requested_at: DateTime<Utc>,
}

impl ChatStore {
    /// 自分の承認を待っている run を、承認を求めた時刻の新しい順に返す（最大 `limit` 件）。
    pub async fn list_pending_approvals(
        &self,
        ctx: &AuthContext,
        limit: i64,
    ) -> Result<Vec<PendingApproval>, ChatError> {
        let limit = limit.clamp(1, MAX_PENDING);
        // pending_approval は status を waiting_approval にする直前に書かれる（sink → approver）。
        // 両方そろった run だけを「承認待ち」とみなす。並びは承認を求めた時刻（run の updated_at は
        // 待機中もハートビートで進むので使わない）。読めない旧形式は run の作成時刻で代える。
        // ツールの入力・理由は引かない（ホームでは出さない。中身は会話の承認カードで確かめる）。
        let rows: Vec<PendingRow> = sqlx::query_as(
            "SELECT r.thread_id, t.title AS thread_title, t.origin_note_id, t.origin_note_name, \
                    r.run_id, r.pending_approval->>'tool_call_id' AS tool_call_id, \
                    r.pending_approval->>'tool_name' AS tool_name, \
                    COALESCE((r.pending_approval->>'requested_at')::timestamptz, r.created_at) \
                      AS requested_at \
             FROM generation_run r \
             JOIN thread t ON t.tenant_id = r.tenant_id AND t.id = r.thread_id \
                          AND t.deleted_at IS NULL \
             WHERE r.tenant_id = $1 AND r.org = $2 AND r.actor = $3 \
               AND r.status = 'waiting_approval' AND r.pending_approval IS NOT NULL \
               AND r.pending_approval ? 'tool_call_id' AND r.pending_approval ? 'tool_name' \
             ORDER BY requested_at DESC, r.run_id DESC \
             LIMIT $4",
        )
        .bind(&ctx.tenant_id)
        .bind(&ctx.org)
        .bind(&ctx.principal.id)
        .bind(CANDIDATES)
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
                object: ctx.ns().thread(&r.thread_id.to_string()),
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
            .take(usize::try_from(limit).unwrap_or(1))
            .map(|r| PendingApproval {
                thread_id: r.thread_id,
                thread_title: r.thread_title,
                origin_note_id: r.origin_note_id,
                origin_note_name: r.origin_note_name,
                run_id: r.run_id,
                tool_call_id: r.tool_call_id,
                tool_name: r.tool_name,
                requested_at: r.requested_at,
            })
            .collect())
    }
}
