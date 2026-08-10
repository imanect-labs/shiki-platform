//! 実行履歴の保持期間 GC（engine.md §12.2・#444）。
//!
//! 起算は **run が terminal になった時刻**。実行中の run は保持期間を跨いでも対象外にする
//! （`run_timeout_sec` の上限 30 日 < 保持初期値 90 日なので実運用では衝突しないが、
//! 設定次第では起こり得るので条件として明示する）。
//!
//! 消す対象と経路:
//!
//! | 対象 | 経路 |
//! |---|---|
//! | `workflow_run` | 本モジュールが直接 DELETE |
//! | `step_execution` / `run_event` / `wait_subscription` | FK の `ON DELETE CASCADE` |
//! | `effect_journal` | run に FK で紐づかないので TTL で別途 DELETE（§7.3） |
//!
//! **削除は必ずバッチで行う。** 数百万行を 1 トランザクションで消すと、長時間のロックと巨大な
//! WAL を生んで本番を止める。1 バッチごとにコミットし、消えなくなるまで繰り返す。
//!
//! # spill blob（未実装・実装時の必須作業）
//!
//! engine.md §12.1 は 256KB 超の step 出力を ObjectStore（`workflow-io/{tenant}/{run_id}/{step_path}`）
//! へ逃がすと定めるが、**spill は現時点で未実装**（`grep -r spill crates/` が空）。したがって
//! 本 GC は blob を消さない。
//!
//! spill を実装する際は **本 GC への blob 削除の追加を同時に行うこと**。DB だけ消して blob が
//! 残ると、参照する行が無い＝発見手段の無い孤児が永久に溜まり続ける（後から突合して消すのは
//! 極めて高くつく）。engine.md §12.1 にも同じ注意を書いてある。

use std::time::Duration;

use serde::{Deserialize, Serialize};
use serde_json::json;
use sqlx::PgPool;

use super::{map_db, RunStore, RunStoreError};

/// 1 バッチで消す run の数。行ロックと WAL を短く保つための粒度。
const RUN_BATCH: i64 = 200;
/// 1 回の GC で回すバッチ数の上限（暴走時の歯止め・既定で 20 万 run/回）。
/// 使い切った場合は消し残しがある状態で正常終了し、次回の日次ジョブが続きを消す。
const MAX_BATCHES: usize = 1000;

/// GC の結果（ジョブのログ・テストの検証点）。
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct GcReport {
    /// 削除した run の数（step/event/wait は CASCADE で道連れ）。
    pub runs_deleted: u64,
    /// 削除した effect_journal の行数。
    pub journal_deleted: u64,
    /// バッチ上限に当たって消し残した場合 true（次回の日次ジョブが続きを消す）。
    pub truncated: bool,
}

/// 保持期間を過ぎた terminal run を消す 1 バッチ分の SQL。
///
/// `finished_at` の index レンジで先に絞ってから、テナントごとの保持期間で正確に判定する。
/// レンジ条件（全テナントの最小保持期間）を噛ませないと、消すものが無いときに terminal run を
/// 全件走査して 0 件を返すことになる——日次で回すジョブとしては最悪の形になる。
const PURGE_RUNS_SQL: &str = "\
    WITH victim AS ( \
        SELECT r.tenant_id, r.run_id \
          FROM workflow_run r \
          JOIN tenant t ON t.tenant_id = r.tenant_id \
         WHERE r.status IN ('succeeded', 'failed', 'cancelled') \
           AND r.finished_at IS NOT NULL \
           AND r.finished_at < now() - make_interval(days => \
                 (SELECT min(workflow_retention_days) FROM tenant)) \
           AND r.finished_at < now() - make_interval(days => t.workflow_retention_days) \
         ORDER BY r.finished_at \
         LIMIT $1 \
    ) \
    DELETE FROM workflow_run w \
     USING victim v \
     WHERE w.tenant_id = v.tenant_id AND w.run_id = v.run_id";

/// effect_journal の TTL 削除（run 保持期間と同じ・§7.3）。
const PURGE_JOURNAL_SQL: &str = "\
    WITH victim AS ( \
        SELECT j.tenant_id, j.idempotency_key \
          FROM effect_journal j \
          JOIN tenant t ON t.tenant_id = j.tenant_id \
         WHERE j.created_at < now() - make_interval(days => \
                 (SELECT min(workflow_retention_days) FROM tenant)) \
           AND j.created_at < now() - make_interval(days => t.workflow_retention_days) \
         ORDER BY j.created_at \
         LIMIT $1 \
    ) \
    DELETE FROM effect_journal e \
     USING victim v \
     WHERE e.tenant_id = v.tenant_id AND e.idempotency_key = v.idempotency_key";

impl RunStore {
    /// 保持期間を過ぎた実行履歴を消す（jobq ジョブの本体・engine.md §12.2）。
    ///
    /// バッチごとにコミットし、消えなくなるか [`MAX_BATCHES`] に達するまで繰り返す。
    /// 途中で失敗しても消えた分はコミット済みで、次回が続きから再開する（冪等）。
    pub async fn purge_expired_history(&self) -> Result<GcReport, RunStoreError> {
        let mut report = GcReport::default();
        for batch in 0..MAX_BATCHES {
            let runs = sqlx::query(PURGE_RUNS_SQL)
                .bind(RUN_BATCH)
                .execute(&self.db)
                .await
                .map_err(map_db)?
                .rows_affected();
            let journal = sqlx::query(PURGE_JOURNAL_SQL)
                .bind(RUN_BATCH)
                .execute(&self.db)
                .await
                .map_err(map_db)?
                .rows_affected();
            report.runs_deleted += runs;
            report.journal_deleted += journal;
            if runs == 0 && journal == 0 {
                return Ok(report);
            }
            if batch + 1 == MAX_BATCHES {
                report.truncated = true;
            }
        }
        Ok(report)
    }
}

/// 保持期間 GC の専用キュー。ワークフローのレーンとは別に持つ（§1.1 のレーン分離と同じ理由で、
/// 重いバッチ削除を step 実行のワーカープールに混ぜない）。
pub const WORKFLOW_GC_QUEUE: &str = "workflow_gc";

/// `maintenance_schedule` のキー。
const GC_JOB_NAME: &str = "workflow_history_gc";

/// 投入間隔（§12.2 の「日次」）。
const GC_INTERVAL: Duration = Duration::from_hours(24);

/// GC ジョブの可視タイムアウト。削除が長引いても別ワーカーに二重配信されないよう長めに取る
/// （二重実行しても冪等ではあるが、同じ行を消し合って無駄なロック競合を起こす）。
const GC_VISIBILITY_TIMEOUT: Duration = Duration::from_mins(30);

impl RunStore {
    /// 前回投入から [`GC_INTERVAL`] 経っていれば GC ジョブを 1 件積む（スケジューラ tick から呼ぶ）。
    ///
    /// 積んだら `true`。**判定と投入と台帳更新は同一 TX** で行い、`maintenance_schedule` の行を
    /// `FOR UPDATE` で押さえる。複数インスタンスが同時にリーダーだと誤認した場合でも
    /// （リース更新の隙間などで起こり得る）、二重投入しない。
    pub async fn enqueue_history_gc_if_due(&self) -> Result<bool, RunStoreError> {
        let mut tx = self.db.begin().await.map_err(map_db)?;
        // 初回は行を作る。作成時刻を last_enqueued_at にするので、初回投入は次の tick 以降になる
        // （起動直後にいきなり重い削除が走らない）。
        sqlx::query(
            "INSERT INTO maintenance_schedule (job_name) VALUES ($1) ON CONFLICT DO NOTHING",
        )
        .bind(GC_JOB_NAME)
        .execute(&mut *tx)
        .await
        .map_err(map_db)?;
        let due: Option<bool> = sqlx::query_scalar(
            "SELECT last_enqueued_at < now() - ($2 || ' seconds')::interval \
               FROM maintenance_schedule WHERE job_name = $1 FOR UPDATE",
        )
        .bind(GC_JOB_NAME)
        .bind(i64::try_from(GC_INTERVAL.as_secs()).unwrap_or(i64::MAX))
        .fetch_optional(&mut *tx)
        .await
        .map_err(map_db)?;
        if due != Some(true) {
            // **rollback してはいけない。** 上の INSERT ごと巻き戻るため台帳の行が永遠に作られず、
            // 毎 tick が「行を作る → 巻き戻す」を繰り返して GC が一度も期限到来しない
            // （結合テストで発見）。ここは投入していないだけで、行の作成は確定させる。
            tx.commit().await.map_err(map_db)?;
            return Ok(false);
        }
        // tenant_id は jobq の必須列だが、この GC は全テナント横断の 1 ジョブ。
        // 走査自体がテナントごとの保持期間を見るので、ここは運用ジョブであることを示す固定値。
        jobq::enqueue_on(
            &mut tx,
            jobq::NewJob {
                queue: WORKFLOW_GC_QUEUE,
                tenant_id: "-",
                payload: &json!({ "job": GC_JOB_NAME }),
                trace_id: None,
                max_attempts: 3,
            },
        )
        .await
        .map_err(|e| RunStoreError::Internal(format!("gc ジョブの投入に失敗: {e}")))?;
        sqlx::query(
            "UPDATE maintenance_schedule SET last_enqueued_at = now(), updated_at = now() \
              WHERE job_name = $1",
        )
        .bind(GC_JOB_NAME)
        .execute(&mut *tx)
        .await
        .map_err(map_db)?;
        tx.commit().await.map_err(map_db)?;
        Ok(true)
    }

    /// GC キューを 1 件消費する（ワーカーのループから呼ぶ）。処理した場合 `Some(report)`。
    ///
    /// 削除は冪等（消えたものは消えたまま）なので、失敗時は jobq のバックオフ再配信に任せる。
    pub async fn consume_history_gc_once(&self) -> Result<Option<GcReport>, RunStoreError> {
        let mut conn = self.db.acquire().await.map_err(map_db)?;
        let mut jobs = jobq::claim(&mut conn, WORKFLOW_GC_QUEUE, GC_VISIBILITY_TIMEOUT, 1)
            .await
            .map_err(|e| RunStoreError::Internal(format!("gc ジョブの claim に失敗: {e}")))?;
        let Some(job) = jobs.pop() else {
            return Ok(None);
        };
        match self.purge_expired_history().await {
            Ok(report) => {
                jobq::ack(&mut conn, job.id)
                    .await
                    .map_err(|e| RunStoreError::Internal(format!("gc ジョブの ack に失敗: {e}")))?;
                Ok(Some(report))
            }
            Err(e) => {
                // 失敗は握り潰さず jobq へ返す（バックオフ再配信・上限超過で DLQ）。
                let outcome = jobq::fail(
                    &mut conn,
                    job.id,
                    &e.to_string(),
                    jobq::backoff_for(job.attempts),
                )
                .await;
                if let Err(fe) = outcome {
                    tracing::error!(error = %fe, "gc ジョブの失敗記録に失敗（vt 経過で再配信）");
                }
                Err(e)
            }
        }
    }
}

/// GC ワーカーのループ（`start` でタスクを起こす）。
///
/// step 実行のワーカープールとは別タスクにする。重いバッチ削除で step の claim を止めないため。
#[derive(Clone)]
pub struct HistoryGcWorker {
    store: RunStore,
    poll: Duration,
}

impl HistoryGcWorker {
    /// 既定のポーリング間隔（1 分）。日次ジョブなので短くする意味が無い。
    pub fn new(db: PgPool) -> Self {
        HistoryGcWorker {
            store: RunStore::new(db),
            poll: Duration::from_mins(1),
        }
    }

    /// ポーリングループを起動する（プロセス生存中は走り続ける・detach）。
    pub fn spawn(self) -> tokio::task::JoinHandle<()> {
        tokio::spawn(async move {
            loop {
                match self.store.consume_history_gc_once().await {
                    Ok(Some(report)) => tracing::info!(
                        runs_deleted = report.runs_deleted,
                        journal_deleted = report.journal_deleted,
                        truncated = report.truncated,
                        "実行履歴 GC を実行しました"
                    ),
                    Ok(None) => {}
                    Err(e) => tracing::warn!(error = %e, "実行履歴 GC でエラー"),
                }
                tokio::time::sleep(self.poll).await;
            }
        })
    }
}
