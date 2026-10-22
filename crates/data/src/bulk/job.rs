//! 一括取込のジョブ消費と常駐ワーカー（jobq・[`super::BULK_IMPORT_QUEUE`]）。
//!
//! 1 ジョブで最大 [`CHUNKS_PER_JOB`] チャンクを進め、残りは続きのジョブとして**キューの末尾へ**積み直す
//! （ack と同じ Tx）。複数テナントの取込が同時に走っても、jobq の id 順の取り出しで交互に進む
//! （1 テナントの大きな取込がワーカーを占有しない）。チャンクは各々 1 Tx で、ロックの保持は 1 チャンク分。

use std::time::Duration;

use serde_json::Value;
use uuid::Uuid;

use super::{enqueue_import, BULK_CHUNK, BULK_IMPORT_QUEUE};
use crate::store::DataStore;
use crate::{map_db, DataError};

/// 1 ジョブで進めるチャンク数。
const CHUNKS_PER_JOB: usize = 20;
/// 可視タイムアウト（1 ジョブの想定所要より十分長く）。
const BULK_VT: Duration = Duration::from_mins(15);
/// 終わった取込の作業領域・エラー行を残す日数。
const RETAIN_DAYS: i32 = 7;

fn job_ids(payload: &Value) -> Option<(Uuid, Uuid)> {
    let get = |k: &str| {
        payload
            .get(k)
            .and_then(Value::as_str)
            .and_then(|s| Uuid::parse_str(s).ok())
    };
    Some((get("table_id")?, get("import_id")?))
}

impl DataStore {
    /// 一括取込のジョブを 1 件消費する。処理した場合 `true`。
    pub async fn consume_bulk_job_once(&self) -> Result<bool, DataError> {
        for (_, pool) in self.pools.all() {
            let mut conn = pool.acquire().await.map_err(map_db)?;
            let mut jobs = jobq::claim(&mut conn, BULK_IMPORT_QUEUE, BULK_VT, 1)
                .await
                .map_err(|e| DataError::Internal(format!("jobq: {e}")))?;
            drop(conn);
            let Some(job) = jobs.pop() else { continue };
            let outcome = self.run_bulk_job(&job).await;
            let mut tx = pool.begin().await.map_err(map_db)?;
            match outcome {
                Ok(again) => {
                    if again {
                        if let Some((table_id, import_id)) = job_ids(&job.payload) {
                            enqueue_import(
                                &mut tx,
                                &job.tenant_id,
                                table_id,
                                import_id,
                                job.trace_id.as_deref(),
                            )
                            .await?;
                        }
                    }
                    jobq::ack(&mut tx, job.id)
                        .await
                        .map_err(|e| DataError::Internal(format!("jobq: {e}")))?;
                    tx.commit().await.map_err(map_db)?;
                }
                Err(e) => {
                    let failed = jobq::fail(
                        &mut tx,
                        job.id,
                        &e.to_string(),
                        jobq::backoff_for(job.attempts),
                    )
                    .await;
                    if failed.is_ok() {
                        tx.commit().await.map_err(map_db)?;
                    }
                    return Err(e);
                }
            }
            return Ok(true);
        }
        Ok(false)
    }

    /// ジョブ 1 件を処理する。取込が終わっていなければ `true`（続きのジョブを積む）。
    async fn run_bulk_job(&self, job: &jobq::ClaimedJob) -> Result<bool, DataError> {
        let Some((table_id, import_id)) = job_ids(&job.payload) else {
            return Err(DataError::Internal(
                "一括取込ジョブの payload が不正です".into(),
            ));
        };
        for _ in 0..CHUNKS_PER_JOB {
            match self
                .bulk_import_step(&job.tenant_id, table_id, import_id, BULK_CHUNK)
                .await
            {
                Ok(step) if step.done => return Ok(false),
                Ok(_) => {}
                // 取込・テーブルが purge された（続ける対象が無い）。
                Err(DataError::NotFound) => return Ok(false),
                Err(e) => return Err(e),
            }
        }
        Ok(true)
    }

    /// ジョブを失った未完了の取込を積み直し、終わって日の経った取込の作業領域を消す。
    pub async fn enqueue_bulk_imports_if_needed(&self) -> Result<u64, DataError> {
        let mut requeued = 0;
        for (_, pool) in self.pools.all() {
            let mut tx = pool.begin().await.map_err(map_db)?;
            if !jobq::has_pending(&mut tx, BULK_IMPORT_QUEUE)
                .await
                .map_err(|e| DataError::Internal(format!("jobq: {e}")))?
            {
                // shard-safety: allow(cross-tenant) 保守スイープ（未完了の取込を拾う・プールごと）。
                let open: Vec<(i32, Uuid, Uuid, Option<String>)> = sqlx::query_as(
                    "SELECT tenant_int, id, table_id, trace_id FROM data_bulk_import \
                     WHERE status IN ('validating', 'applying', 'indexing') LIMIT 100",
                )
                .fetch_all(&mut *tx)
                .await
                .map_err(map_db)?;
                for (tenant_int, import_id, table_id, trace_id) in open {
                    let tenant: Option<String> = sqlx::query_scalar(
                        "SELECT tenant_id FROM data_table WHERE tenant_int = $1 AND id = $2",
                    )
                    .bind(tenant_int)
                    .bind(table_id)
                    .fetch_optional(&mut *tx)
                    .await
                    .map_err(map_db)?;
                    if let Some(tenant_id) = tenant {
                        enqueue_import(
                            &mut tx,
                            &tenant_id,
                            table_id,
                            import_id,
                            trace_id.as_deref(),
                        )
                        .await?;
                        requeued += 1;
                    }
                }
            }
            // shard-safety: allow(cross-tenant) 保守スイープ（保持期間を過ぎた取込の片付け）。
            let old: Vec<(i32, Uuid)> = sqlx::query_as(
                "DELETE FROM data_bulk_import WHERE status IN ('done', 'failed') \
                   AND finished_at < now() - make_interval(days => $1) \
                 RETURNING tenant_int, id",
            )
            .bind(RETAIN_DAYS)
            .fetch_all(&mut *tx)
            .await
            .map_err(map_db)?;
            for (tenant_int, id) in old {
                sqlx::query(
                    "DELETE FROM data_bulk_staging WHERE tenant_int = $1 AND import_id = $2",
                )
                .bind(tenant_int)
                .bind(id)
                .execute(&mut *tx)
                .await
                .map_err(map_db)?;
                sqlx::query("DELETE FROM data_bulk_key WHERE tenant_int = $1 AND import_id = $2")
                    .bind(tenant_int)
                    .bind(id)
                    .execute(&mut *tx)
                    .await
                    .map_err(map_db)?;
            }
            tx.commit().await.map_err(map_db)?;
        }
        Ok(requeued)
    }
}

/// 一括取込の常駐ワーカー（ジョブ消費と、ジョブを失った取込の積み直し）。
#[derive(Clone)]
pub struct BulkImportWorker {
    store: DataStore,
    poll: Duration,
}

impl BulkImportWorker {
    pub fn new(store: DataStore) -> Self {
        BulkImportWorker {
            store,
            poll: Duration::from_secs(2),
        }
    }

    /// バックグラウンドで回す。
    pub fn spawn(self) -> tokio::task::JoinHandle<()> {
        tokio::spawn(async move { self.run().await })
    }

    async fn run(self) {
        let mut sweep_at = tokio::time::Instant::now();
        loop {
            if tokio::time::Instant::now() >= sweep_at {
                if let Err(e) = self.store.enqueue_bulk_imports_if_needed().await {
                    tracing::warn!(error = %e, "一括取込のスイープに失敗");
                }
                sweep_at = tokio::time::Instant::now() + Duration::from_mins(1);
            }
            loop {
                match self.store.consume_bulk_job_once().await {
                    Ok(true) => {}
                    Ok(false) => break,
                    Err(e) => {
                        tracing::warn!(error = %e, "一括取込ジョブに失敗（jobq がバックオフ再配信）");
                        break;
                    }
                }
            }
            tokio::time::sleep(self.poll).await;
        }
    }
}
