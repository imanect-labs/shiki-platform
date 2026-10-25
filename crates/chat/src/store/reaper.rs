//! `ChatStore`: 強制失敗と孤児回収（fencing 無視の backstop 経路）。
//!
//! 主経路（[`runs`](super::runs) の claim/lease/fencing/finalize）が機能しなかった場合の保険。
//! 生成の最終試行失敗（jobq DLQ 行き）と sweeper が使う。

#[allow(clippy::wildcard_imports)]
use super::*;

use durable::{Key, KeyValue};
use uuid::Uuid;

use super::runs::{map_db, CHAT_GENERATION_QUEUE, EVENT_SPEC, RUN_KEY_COLUMNS, RUN_SPEC};
use crate::approver::APPROVAL_MAX_WAIT;
use crate::model::{StreamEvent, StreamEventKind};

impl ChatStore {
    /// run を強制 failed 化し、Error イベントを追記する（fencing 無視）。
    ///
    /// 生成の最終試行失敗（jobq DLQ 行き）と孤児回収 sweeper が使う。既に端末状態なら no-op。
    /// UI に失敗を明示するため Error イベントを 1 件足してから status を failed にする。
    ///
    /// `user_message` は**会話へそのまま出る**（SSE の Error イベント）ので定型文を渡す
    /// （[`ChatError::user_message`]）。上流の生エラー等の全文は `detail` に渡し、
    /// `generation_run.last_error`（API 非公開）と tracing にだけ残す。
    pub async fn force_fail_run(
        &self,
        run_id: Uuid,
        user_message: &str,
        detail: &str,
    ) -> Result<bool, ChatError> {
        let event = StreamEventKind::Error {
            message: user_message.to_string(),
        };
        let payload = serde_json::to_value(&event)
            .map_err(|e| ChatError::Internal(format!("event serialize: {e}")))?;
        // 端末でない run にだけ Error を追記（次 seq・fencing 無視の backstop）。
        let kv = [KeyValue::Uuid(run_id)];
        let seq = durable::append_event_unfenced(
            &self.db,
            &RUN_SPEC,
            &EVENT_SPEC,
            &Key::new(RUN_KEY_COLUMNS, &kv),
            event.tag(),
            &payload,
            // 承認待ちも端末ではない（承認待ちのまま失われた run を確定できるように）。
            &["queued", "running", "waiting_approval"],
        )
        .await
        .map_err(map_db)?;

        let Some(seq) = seq else {
            return Ok(false); // 既に端末状態
        };
        let thread_id: Option<Uuid> = sqlx::query_scalar(
            "UPDATE generation_run SET status = 'failed', last_error = $2, \
             lease_until = NULL, pending_approval = NULL, updated_at = now() \
             WHERE run_id = $1 RETURNING thread_id",
        )
        .bind(run_id)
        .bind(detail)
        .fetch_optional(&self.db)
        .await
        .map_err(map_db)?;
        tracing::error!(
            run_id = %run_id,
            thread_id = ?thread_id,
            detail,
            user_message,
            "chat run を failed 化"
        );

        let se = StreamEvent { seq, event };
        if let Ok(s) = serde_json::to_string(&se) {
            self.publish_event(run_id, &s).await;
        }
        Ok(true)
    }

    /// 孤児回収 sweeper（backstop）。主経路は jobq の再配信＋claim takeover で、ここは**ジョブが
    /// 失われた**場合の保険（ジョブが残っている run は再配信に任せて触らない）。戻り値は処理した run 数。
    ///
    /// - running: リースが大きく失効したものを failed 化する（Error イベントで UI にも反映）。
    /// - waiting_approval: 以前は対象外で、ジョブを失った承認待ちは永久に承認待ちのまま残った
    ///   （検証 P1）。最後に生きていた時刻（`lease_until`）から承認の期限（[`APPROVAL_MAX_WAIT`]）を
    ///   過ぎていれば failed 化し、期限内ならジョブを積み直す（ワーカーが取り直して承認を求め直す＝
    ///   再起動後の再配信と同じ経路）。
    pub async fn reap_orphaned_runs(&self, grace_secs: i64) -> Result<u64, ChatError> {
        let ids: Vec<Uuid> = sqlx::query_scalar(
            "SELECT r.run_id FROM generation_run r \
             WHERE r.status = 'running' AND r.lease_until IS NOT NULL \
               AND r.lease_until < now() - ($1 || ' seconds')::interval \
               AND NOT EXISTS (SELECT 1 FROM job_queue j WHERE j.queue = $2 \
                               AND j.payload->>'run_id' = r.run_id::text) \
             LIMIT 100",
        )
        .bind(grace_secs)
        .bind(CHAT_GENERATION_QUEUE)
        .fetch_all(&self.db)
        .await
        .map_err(map_db)?;
        let mut n = 0;
        for id in ids {
            if self
                .force_fail_run(
                    id,
                    crate::error::failure_message::INTERRUPTED,
                    "orphaned (lease expired)",
                )
                .await
                .unwrap_or(false)
            {
                n += 1;
            }
        }
        Ok(n + self.recover_orphaned_approvals(grace_secs).await?)
    }

    /// ジョブを失った承認待ちの run を、期限切れなら failed 化し、期限内ならジョブを積み直す。
    async fn recover_orphaned_approvals(&self, grace_secs: i64) -> Result<u64, ChatError> {
        let max_wait = i64::try_from(APPROVAL_MAX_WAIT.as_secs()).unwrap_or(i64::MAX);
        let rows: Vec<(Uuid, bool)> = sqlx::query_as(
            "SELECT r.run_id, r.lease_until < now() - ($2 || ' seconds')::interval \
             FROM generation_run r \
             WHERE r.status = 'waiting_approval' \
               AND (r.lease_until IS NULL OR r.lease_until < now() - ($1 || ' seconds')::interval) \
               AND NOT EXISTS (SELECT 1 FROM job_queue j WHERE j.queue = $3 \
                               AND j.payload->>'run_id' = r.run_id::text) \
             LIMIT 100",
        )
        .bind(grace_secs)
        .bind(max_wait)
        .bind(CHAT_GENERATION_QUEUE)
        .fetch_all(&self.db)
        .await
        .map_err(map_db)?;
        let mut n = 0;
        for (id, expired) in rows {
            let done = if expired {
                self.force_fail_run(
                    id,
                    crate::error::failure_message::APPROVAL_EXPIRED,
                    "orphaned while waiting for approval (approval deadline passed)",
                )
                .await
                .unwrap_or(false)
            } else {
                self.requeue_orphaned_approval(id, grace_secs)
                    .await
                    .unwrap_or_else(|e| {
                        tracing::warn!(run_id = %id, error = %e, "承認待ちの run のジョブの積み直しに失敗");
                        false
                    })
            };
            if done {
                n += 1;
            }
        }
        Ok(n)
    }

    /// 承認待ちの run にジョブを積み直す。run 行をロックして条件を確かめ直す（並行する sweeper
    /// どうしで二重に積まない・その間にワーカーが取り直した run には積まない）。
    async fn requeue_orphaned_approval(
        &self,
        run_id: Uuid,
        grace_secs: i64,
    ) -> Result<bool, ChatError> {
        let mut tx = self.db.begin().await.map_err(map_db)?;
        let row: Option<(String, Option<String>)> = sqlx::query_as(
            "SELECT r.tenant_id, r.trace_id FROM generation_run r \
             WHERE r.run_id = $1 AND r.status = 'waiting_approval' \
               AND (r.lease_until IS NULL OR r.lease_until < now() - ($2 || ' seconds')::interval) \
               AND NOT EXISTS (SELECT 1 FROM job_queue j WHERE j.queue = $3 \
                               AND j.payload->>'run_id' = r.run_id::text) \
             FOR UPDATE OF r SKIP LOCKED",
        )
        .bind(run_id)
        .bind(grace_secs)
        .bind(CHAT_GENERATION_QUEUE)
        .fetch_optional(&mut *tx)
        .await
        .map_err(map_db)?;
        let Some((tenant_id, trace_id)) = row else {
            return Ok(false);
        };
        jobq::enqueue_on(
            &mut tx,
            jobq::NewJob {
                queue: CHAT_GENERATION_QUEUE,
                tenant_id: &tenant_id,
                payload: &serde_json::json!({ "run_id": run_id }),
                trace_id: trace_id.as_deref(),
                max_attempts: 3,
            },
        )
        .await
        .map_err(|e| ChatError::Internal(format!("enqueue: {e}")))?;
        tx.commit().await.map_err(map_db)?;
        tracing::warn!(run_id = %run_id, "ジョブを失った承認待ちの run にジョブを積み直した");
        Ok(true)
    }
}
