//! 実行履歴の保持期間 GC（engine.md §12.2・#444）。
//!
//! 起算は **run が terminal になった時刻**。実行中の run は保持期間を跨いでも対象外にする。
//! 保持日数は 1 日まで縮められる（`run_timeout_sec` の上限 30 日より短くできる）ので、
//! 「既定 90 日なら衝突しない」に頼らず条件として明示的に効かせる。journal も所有 run 行の
//! 不在を条件にするため、衝突しても実行中 run の副作用記録は先に消えない（PIT-31）。
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
    /// 失敗して飛ばしたテナント数。1 件でもあればジョブ全体を Err にして jobq に再試行させる。
    pub failed_tenants: u64,
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

/// effect_journal の TTL 削除（run 保持期間と同じ・§7.3）。
/// `$1`=tenant_id・`$2`=保持日数・`$3`=件数・`$4`=走査再開位置（カーソル）。
///
/// **所有 run が残っている journal は消さない。** 保持期間は run の生存期間より短く設定できる以上
/// （`> 0` しか制約が無い）、`created_at` だけで消すと**実行中/待機中の run の副作用記録が先に消え**、
/// その step が再開したときに `EffectJournal::check` が `Proceed` を返して外部副作用を二重実行する
/// （PIT-31 違反・Codex P1・#445）。run 行の有無で判定すれば、run 側の削除条件（terminal かつ
/// 期限切れ）がそのまま journal の削除条件になる。
///
/// **所有 run の条件は `LIMIT` より前に置く。** 後段で絞ると、先頭 N 件がすべて生存 run のもの
/// だった場合に削除 0 件となり、呼び出し側が「消し切った」と判断して**その先にある削除可能な
/// journal へ二度と到達しない**（毎日同じ N 件で止まる・CodeRabbit 指摘）。前に置けば index 順に
/// 流しながら「消せるもの」だけを N 件集めて止まる。
///
/// **`$4` のカーソルで走査を再開する。** 削除条件が「期限切れ かつ 所有 run が不在」なので、
/// 生存 run に属する期限切れ journal は**消せないまま index の先頭に残り続ける**。カーソルが無いと
/// 毎バッチその先頭群を頭から舐め直すため、1 GC の総走査が O(消せない行数 × バッチ数) に膨らむ
/// （実測: 生存 run 25 万・journal 100 万行で **1 バッチ 10.5 秒 / 400 万バッファ**）。前バッチで
/// 到達した `created_at` から再開すれば 1 GC = 1 パス（O(行数)）になる。
///
/// 境界は `>=` にして同一 `created_at` の同着群を取りこぼさない。既に消した行はテーブルから
/// 消えているので、再走査で当たるのは「消せない同着行」だけ＝有界。初回は `$4 = NULL` で、
/// `coalesce` で `-infinity` に落とす（`$4 IS NULL OR ...` と書くと index レンジに落ちないため。
/// chrono の `MIN_UTC` は Postgres の timestamptz 範囲外なので番兵値としては使えない）。
///
/// 冪等キーは `wf:{tenant_id}:{run_id}:{step_path}`（script の `#cN` は step_path 側に付く）なので、
/// 3 番目のフィールドが run_id。tenant_id は `: | # @` と空白を禁止済み（API 層の validate）なので
/// `split_part` の位置がずれることはない。`CASE` は**キャスト安全のためのガード**で、形式外のキーを
/// `::uuid` に流さない（実際の絞り込みは外側の正規表現が行う）。
const PURGE_JOURNAL_SQL: &str = "\
    WITH victim AS ( \
        SELECT j.tenant_id, j.idempotency_key \
          FROM effect_journal j \
         WHERE j.tenant_id = $1 \
           AND j.created_at < now() - make_interval(days => $2) \
           AND j.created_at >= coalesce($4::timestamptz, '-infinity'::timestamptz) \
           AND j.idempotency_key ~ \
               '^wf:[^:]+:[0-9a-fA-F]{8}-[0-9a-fA-F]{4}-[0-9a-fA-F]{4}-[0-9a-fA-F]{4}-[0-9a-fA-F]{12}:' \
           AND NOT EXISTS ( \
               SELECT 1 FROM workflow_run r \
                WHERE r.tenant_id = j.tenant_id \
                  AND r.run_id = (CASE WHEN j.idempotency_key ~ \
                        '^wf:[^:]+:[0-9a-fA-F]{8}-[0-9a-fA-F]{4}-[0-9a-fA-F]{4}-[0-9a-fA-F]{4}-[0-9a-fA-F]{12}:' \
                      THEN split_part(j.idempotency_key, ':', 3) END)::uuid) \
         ORDER BY j.created_at \
         LIMIT $3 \
    ) \
    DELETE FROM effect_journal e \
     USING victim v \
     WHERE e.tenant_id = v.tenant_id AND e.idempotency_key = v.idempotency_key \
     RETURNING e.created_at";

impl RunStore {
    /// **1 テナント分**の保持期間 GC を実行する（外部から呼べる唯一の入口）。
    ///
    /// テナント横断版（[`RunStore::purge_expired_history`]）は `pub(crate)` に閉じてあり、
    /// アンビエントに全テナントを消せる公開経路を作らない。破壊的操作を公開するなら、対象
    /// テナントが引数として必ず現れる形にする（AGENTS.md「tenant_id が落ちる経路を作らない」）。
    pub async fn purge_tenant_history(&self, tenant_id: &str) -> Result<GcReport, RunStoreError> {
        self.purge_expired_history(Some(tenant_id)).await
    }

    /// 保持期間を過ぎた実行履歴を消す（jobq ジョブの本体・engine.md §12.2）。
    ///
    /// **内部運用経路であり `AuthContext` を持たない全テナント横断処理。** 呼ぶのは
    /// [`HistoryGcWorker`] だけなので `pub(crate)` に閉じる（#445 のレビュー指摘の残り穴を、
    /// コメントではなく可視性で担保する）。テナント単位の入口が要るなら
    /// [`RunStore::purge_tenant_history`] を使う。
    ///
    /// テナントごとに、バッチごとにコミットしながら消す。途中で失敗しても消えた分はコミット済みで、
    /// 次回が続きから再開する（冪等）。
    pub(crate) async fn purge_expired_history(
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

        // **1 テナントの失敗で他テナントを巻き添えにしない。** ここを `?` で抜けると、先頭テナントの
        // 行ロック待ちや statement_timeout ひとつで**その日の GC が以降 1 件も走らない**。GC は
        // `workflow.enabled` から独立した唯一の保持機構なので、blast radius をテナント内に閉じる。
        // 失敗件数は最後に Err へ畳んで jobq の再試行に載せる（黙って成功にしない）。
        let mut report = GcReport::default();
        let mut last_error = None;
        for (tenant_id, days) in tenants {
            match self.purge_one_tenant(&tenant_id, days).await {
                Ok((runs, journal, left)) => {
                    report.runs_deleted += runs;
                    report.journal_deleted += journal;
                    report.truncated |= left;
                }
                Err(e) => {
                    tracing::error!(tenant = %tenant_id, error = %e, "テナントの履歴 GC に失敗（他テナントは継続）");
                    report.failed_tenants += 1;
                    last_error = Some(e);
                }
            }
        }
        if let Some(e) = last_error {
            return Err(RunStoreError::Internal(format!(
                "{} テナントの GC に失敗（最後の理由: {e}）",
                report.failed_tenants
            )));
        }
        Ok(report)
    }

    /// 1 テナント分を消す。戻り値は (run 削除数, journal 削除数, 消し残しの有無)。
    async fn purge_one_tenant(
        &self,
        tenant_id: &str,
        days: i32,
    ) -> Result<(u64, u64, bool), RunStoreError> {
        // run を先に消す。journal はその結果（run 行の消滅）を見て消せるようになる。
        let (runs, runs_left) = self
            .purge_in_batches(PURGE_RUNS_SQL, tenant_id, days, RUN_BATCH)
            .await?;
        let (journal, journal_left) = self.purge_journal_in_batches(tenant_id, days).await?;
        Ok((runs, journal, runs_left || journal_left))
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

    /// journal を「消えなくなるまで」カーソル付きでバッチ削除する。戻り値は (削除数, 消し残しの有無)。
    ///
    /// run 側と違い**カーソルを持つ**。journal の削除条件は「期限切れ かつ 所有 run が不在」で、
    /// 生存 run に属する期限切れ行は消せないまま index の先頭に残るため、毎バッチ先頭から
    /// 舐め直すと 1 GC の総走査が O(消せない行数 × バッチ数) になる（`PURGE_JOURNAL_SQL` 参照）。
    async fn purge_journal_in_batches(
        &self,
        tenant_id: &str,
        retention_days: i32,
    ) -> Result<(u64, bool), RunStoreError> {
        let mut total = 0u64;
        // 走査開始位置。初回は None（SQL 側で `-infinity` に落ちる）。
        let mut cursor: Option<chrono::DateTime<chrono::Utc>> = None;
        for _ in 0..MAX_BATCHES {
            let deleted: Vec<(chrono::DateTime<chrono::Utc>,)> = sqlx::query_as(PURGE_JOURNAL_SQL)
                .bind(tenant_id)
                .bind(retention_days)
                .bind(JOURNAL_BATCH)
                .bind(cursor)
                .fetch_all(&self.db)
                .await
                .map_err(map_db)?;
            total += deleted.len() as u64;
            if deleted.len() < usize::try_from(JOURNAL_BATCH).unwrap_or(usize::MAX) {
                return Ok((total, false));
            }
            // 次バッチはこのバッチで到達した位置から。`>=` なので同着群は取りこぼさない。
            cursor = deleted.iter().map(|r| r.0).max().or(cursor);
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
    /// 前回投入から [`GC_INTERVAL`] 経っていれば GC ジョブを 1 件積む（[`HistoryGcWorker`] が呼ぶ）。
    ///
    /// 積んだら `true`。**判定と投入と台帳更新は同一 TX** で行い、`maintenance_schedule` の行を
    /// `FOR UPDATE` で押さえる。全レプリカが無条件に呼んでも二重投入しない（＝スケジューラの
    /// 単一リーダーである必要がない。だからワークフローランタイムの外で回せる・#448）。
    pub(crate) async fn enqueue_history_gc_if_due(&self) -> Result<bool, RunStoreError> {
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
    pub(crate) async fn consume_history_gc_once(&self) -> Result<Option<GcReport>, RunStoreError> {
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

/// 実行履歴 GC の保守ランタイム（投入と消費の両方を持つ・`spawn` でループを起こす）。
///
/// **ワークフローランタイム（`spawn_workflow_runtime`）の外で起動する。** 保持期間はプライバシー/
/// コンプライアンス側の義務であり、「新規 run を受け付けるか」（`workflow.enabled`）とは独立した
/// 責務である。ランタイムの中に置くと、過去にワークフローを使っていたデプロイが機能を無効化した
/// 瞬間に保持義務まで止まり、既存履歴が期限を過ぎても永久に残る（#448）。
///
/// 日次投入もこのループが持つ。判定は `maintenance_schedule` の `FOR UPDATE` ＋ 24h チェックで
/// 多重起動安全なので、スケジューラの単一リーダーである必要がない。
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
            // GC は resume 経路を持たない（`resume_failed` を呼ばない）ので既定でよい。
            store: RunStore::new(db, crate::DEFAULT_LEASE_SECS),
            poll: Duration::from_mins(1),
        }
    }

    /// 期限が来ていれば GC ジョブを 1 件積む。積んだら `true`。
    ///
    /// 全レプリカが無条件に呼んでよい（同一 TX ＋ `FOR UPDATE` で 1 件に畳まれる）。
    pub async fn enqueue_if_due(&self) -> Result<bool, RunStoreError> {
        self.store.enqueue_history_gc_if_due().await
    }

    /// 積まれた GC ジョブを 1 件消費する。処理した場合 `Some(report)`。
    pub async fn run_once(&self) -> Result<Option<GcReport>, RunStoreError> {
        self.store.consume_history_gc_once().await
    }

    /// ポーリングループを起動する（プロセス生存中は走り続ける・detach）。
    pub fn spawn(self) -> tokio::task::JoinHandle<()> {
        tokio::spawn(async move {
            loop {
                match self.enqueue_if_due().await {
                    Ok(true) => tracing::info!("実行履歴 GC ジョブを投入しました"),
                    Ok(false) => {}
                    Err(e) => tracing::warn!(error = %e, "実行履歴 GC の投入でエラー"),
                }
                match self.run_once().await {
                    Ok(Some(report)) => tracing::info!(
                        runs_deleted = report.runs_deleted,
                        journal_deleted = report.journal_deleted,
                        truncated = report.truncated,
                        failed_tenants = report.failed_tenants,
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
