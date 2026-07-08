//! `ChatStore`: 強制失敗と孤児回収（fencing 無視の backstop 経路）。
//!
//! 主経路（[`runs`](super::runs) の claim/lease/fencing/finalize）が機能しなかった場合の保険。
//! 生成の最終試行失敗（jobq DLQ 行き）と sweeper が使う。

#[allow(clippy::wildcard_imports)]
use super::*;

use durable::{Key, KeyValue};
use uuid::Uuid;

use super::runs::{map_db, EVENT_SPEC, RUN_KEY_COLUMNS, RUN_SPEC};
use crate::model::{StreamEvent, StreamEventKind};

impl ChatStore {
    /// run を強制 failed 化し、Error イベントを追記する（fencing 無視）。
    ///
    /// 生成の最終試行失敗（jobq DLQ 行き）と孤児回収 sweeper が使う。既に端末状態なら no-op。
    /// UI に失敗を明示するため Error イベントを 1 件足してから status を failed にする。
    pub async fn force_fail_run(&self, run_id: Uuid, message: &str) -> Result<bool, ChatError> {
        let event = StreamEventKind::Error {
            message: message.to_string(),
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
            &["queued", "running"],
        )
        .await
        .map_err(map_db)?;

        let Some(seq) = seq else {
            return Ok(false); // 既に端末状態
        };
        sqlx::query(
            "UPDATE generation_run SET status = 'failed', last_error = $2, \
             lease_until = NULL, updated_at = now() WHERE run_id = $1",
        )
        .bind(run_id)
        .bind(message)
        .execute(&self.db)
        .await
        .map_err(map_db)?;

        let se = StreamEvent { seq, event };
        if let Ok(s) = serde_json::to_string(&se) {
            self.publish_event(run_id, &s).await;
        }
        Ok(true)
    }

    /// 孤児回収 sweeper（backstop）: リースが大きく失効した running run を failed 化する。
    /// 主経路は jobq の再配信＋claim takeover。ここはジョブが失われた場合の保険。
    /// 各孤児へ Error イベントを追記して UI にも失敗を反映する。
    pub async fn reap_orphaned_runs(&self, grace_secs: i64) -> Result<u64, ChatError> {
        let ids: Vec<Uuid> = sqlx::query_scalar(
            "SELECT run_id FROM generation_run \
             WHERE status = 'running' AND lease_until IS NOT NULL \
               AND lease_until < now() - ($1 || ' seconds')::interval \
             LIMIT 100",
        )
        .bind(grace_secs)
        .fetch_all(&self.db)
        .await
        .map_err(map_db)?;
        let mut n = 0;
        for id in ids {
            if self
                .force_fail_run(id, "orphaned (lease expired)")
                .await
                .unwrap_or(false)
            {
                n += 1;
            }
        }
        Ok(n)
    }
}
