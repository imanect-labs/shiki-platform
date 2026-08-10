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
///
/// 1 run に連なる `step_execution` / `run_event` は CASCADE で道連れになるので、実削除行数は
/// これより桁で大きくなり得る（IR は最大 200 ノード・map は最大 1000 要素まで展開する）。
/// 「親 200 件」でも極端な run が混ざれば 1 TX が重くなるため、小さめに取る。
const RUN_BATCH: i64 = 50;
/// 1 バッチで消す effect_journal の行数。run より 1 桁以上多く増えるので大きく取る
/// （こちらは CASCADE を持たない単純削除なので 1 行あたりのコストが軽い）。
const JOURNAL_BATCH: i64 = 2_000;
/// 1 系列あたりのバッチ数上限（暴走時の歯止め）。使い切った場合は消し残しがある状態で
/// 正常終了し、次回の日次ジョブが続きを消す。
const MAX_BATCHES: usize = 1_000;

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

/// 保持期間を過ぎた terminal run を 1 バッチ消す。`$1`=tenant_id・`$2`=保持日数・`$3`=件数。
///
/// **テナントごとに呼ぶ。** 全テナントを 1 クエリで舐めると、保持期間が最も短いテナントに
/// 引きずられて長期保持テナントの履歴まで毎日走査することになる（`(tenant_id, finished_at)`
/// の index レンジがテナント境界で切れないため）。
///
/// **候補は `FOR UPDATE SKIP LOCKED` で固定する。** ロックしないと、候補を読んでから DELETE する
/// までの間に `resume_failed` が同じ run を `running` / `finished_at = NULL` へ戻せてしまい、
/// 外側の条件は tenant/run_id しか見ないため**再開済みの run とその step/event を消す**
/// （Codex P1・#445）。ロックを取れなかった行は次回に回す。
const PURGE_RUNS_SQL: &str = "\
    WITH victim AS ( \
        SELECT r.tenant_id, r.run_id \
          FROM workflow_run r \
         WHERE r.tenant_id = $1 \
           AND r.status IN ('succeeded', 'failed', 'cancelled') \
           AND r.finished_at IS NOT NULL \
           AND r.finished_at < now() - make_interval(days => $2) \
         ORDER BY r.finished_at \
         LIMIT $3 \
         FOR UPDATE SKIP LOCKED \
    ) \
    DELETE FROM workflow_run w \
     USING victim v \
     WHERE w.tenant_id = v.tenant_id AND w.run_id = v.run_id";

/// effect_journal の TTL 削除（run 保持期間と同じ・§7.3）。`$1`=tenant_id・`$2`=保持日数・`$3`=件数。
///
/// **所有 run が残っている journal は消さない。** 保持期間を run の生存期間より短く設定できる以上
/// （`> 0` しか制約が無い）、`created_at` だけで消すと**実行中/待機中の run の副作用記録が先に消え**、
/// その step が再開したときに `EffectJournal::check` が `Proceed` を返して外部副作用を二重実行する
/// （PIT-31 違反・Codex P1・#445）。run 行の有無で判定すれば、run 側の削除条件（terminal かつ
/// 期限切れ）がそのまま journal の削除条件になる。
///
/// 冪等キーは `wf:{tenant_id}:{run_id}:{step_path}`（script の `#cN` は step_path 側に付く）なので、
/// 3 番目のフィールドが run_id。tenant_id は `: | # @` と空白を禁止済み（API 層の validate）なので
/// `split_part` の位置がずれることはない。`MATERIALIZED` で正規表現の絞り込みを先に確定させ、
/// 形式外のキーを `::uuid` キャストに流さない。
const PURGE_JOURNAL_SQL: &str = "\
    WITH candidate AS MATERIALIZED ( \
        SELECT j.tenant_id, j.idempotency_key, \
               split_part(j.idempotency_key, ':', 3) AS run_text \
          FROM effect_journal j \
         WHERE j.tenant_id = $1 \
           AND j.created_at < now() - make_interval(days => $2) \
           AND j.idempotency_key ~ \
               '^wf:[^:]+:[0-9a-fA-F]{8}-[0-9a-fA-F]{4}-[0-9a-fA-F]{4}-[0-9a-fA-F]{4}-[0-9a-fA-F]{12}:' \
         ORDER BY j.created_at \
         LIMIT $3 \
    ), victim AS ( \
        SELECT c.tenant_id, c.idempotency_key FROM candidate c \
         WHERE NOT EXISTS ( \
             SELECT 1 FROM workflow_run r \
              WHERE r.tenant_id = c.tenant_id AND r.run_id = c.run_text::uuid) \
    ) \
    DELETE FROM effect_journal e \
     USING victim v \
     WHERE e.tenant_id = v.tenant_id AND e.idempotency_key = v.idempotency_key";

impl RunStore {
    /// 保持期間を過ぎた実行履歴を消す（jobq ジョブの本体・engine.md §12.2）。
    ///
    /// **内部運用経路であり `AuthContext` を持たない全テナント横断処理。** 呼ぶのは
    /// `spawn_workflow_runtime` が起動する [`HistoryGcWorker`] だけで、公開 API から呼んではいけない。
    ///
    /// テナントごとに、バッチごとにコミットしながら消す。途中で失敗しても消えた分はコミット済みで、
    /// 次回が続きから再開する（冪等）。
    /// `tenant_scope` を渡すとそのテナントだけを対象にする（テスト分離。他の `RunStore` の
    /// 走査系と同じ規約）。ワーカーは `None`（全テナント横断）で呼ぶ。
    pub async fn purge_expired_history(
        &self,
        tenant_scope: Option<&str>,
    ) -> Result<GcReport, RunStoreError> {
        let tenants: Vec<(String, i32)> = sqlx::query_as(
            "SELECT tenant_id, workflow_retention_days FROM tenant \
              WHERE status <> 'deleted' AND (($1::text IS NULL) OR tenant_id = $1)",
        )
        .bind(tenant_scope)
        .fetch_all(&self.db)
        .await
        .map_err(map_db)?;

        let mut report = GcReport::default();
        for (tenant_id, days) in tenants {
            // run を先に消す。journal はその結果（run 行の消滅）を見て消せるようになる。
            let (runs, runs_left) = self
                .purge_in_batches(PURGE_RUNS_SQL, &tenant_id, days, RUN_BATCH)
                .await?;
            let (journal, journal_left) = self
                .purge_in_batches(PURGE_JOURNAL_SQL, &tenant_id, days, JOURNAL_BATCH)
                .await?;
            report.runs_deleted += runs;
            report.journal_deleted += journal;
            report.truncated |= runs_left || journal_left;
        }
        Ok(report)
    }

    /// 1 テナント分を「消えなくなるまで」バッチ削除する。戻り値は (削除数, 消し残しの有無)。
    ///
    /// 削除行数が `limit` 未満になった時点で打ち切る。ちょうど消し切ったときに余分な空振りクエリを
    /// 出さず、`truncated` も誤検知しない。
    async fn purge_in_batches(
        &self,
        sql: &str,
        tenant_id: &str,
        retention_days: i32,
        limit: i64,
    ) -> Result<(u64, bool), RunStoreError> {
        let mut total = 0u64;
        for _ in 0..MAX_BATCHES {
            let deleted = sqlx::query(sql)
                .bind(tenant_id)
                .bind(retention_days)
                .bind(limit)
                .execute(&self.db)
                .await
                .map_err(map_db)?
                .rows_affected();
            total += deleted;
            if deleted < u64::try_from(limit).unwrap_or(u64::MAX) {
                return Ok((total, false));
            }
        }
        Ok((total, true))
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
        // **削除の前にコネクションを返す。** 保持したまま purge を呼ぶと、purge が self.db から
        // 別のコネクションを取りに行くため、プールが 1 本の構成で永久待機する。長時間 GC の間
        // 1 本を無用に占有しないためでもある（Codex P2・#445）。
        drop(conn);
        let outcome = self.purge_expired_history(None).await;
        let mut conn = self.db.acquire().await.map_err(map_db)?;
        match outcome {
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
