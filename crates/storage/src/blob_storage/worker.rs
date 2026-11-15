use super::{BlobStorage, VersionStorageOptions, QUEUE};
use crate::StorageError;
use authz::TenantId;
use serde::Deserialize;
use std::time::Duration;
use uuid::Uuid;

#[derive(Deserialize)]
struct Payload {
    org: String,
    node_id: Uuid,
    #[serde(default)]
    after_version: i64,
}

impl BlobStorage {
    pub(super) async fn gc_scopes_due(&self) -> Result<Vec<(String, String)>, StorageError> {
        let mut tx =
            crate::db_tenant::begin_cross_tenant_tx(&self.pool, "blob_storage_gc_scan").await?;
        // tenant-sql: allow(横断保守 executor の組織単位・有界期限走査)
        let rows: Vec<(String, String)> = sqlx::query_as(
            "WITH due AS MATERIALIZED (SELECT tenant_id, org FROM blob_storage_scope \
             WHERE next_check_at < now() ORDER BY next_check_at LIMIT 16 FOR UPDATE SKIP LOCKED), \
             postponed AS (UPDATE blob_storage_scope s SET next_check_at = GREATEST( \
               clock_timestamp() + interval '1 minute', \
               COALESCE((SELECT max(expires_at) FROM blob_storage_hold h WHERE h.tenant_id = s.tenant_id AND h.org = s.org), now()), \
               CASE WHEN EXISTS (SELECT 1 FROM tenant t WHERE t.tenant_id = s.tenant_id AND t.status IN ('deleting', 'deleted')) \
                    THEN clock_timestamp() + interval '1 hour' ELSE now() END) \
             FROM due WHERE s.tenant_id = due.tenant_id AND s.org = due.org RETURNING s.tenant_id, s.org) \
             SELECT tenant_id, org FROM postponed p \
             WHERE NOT EXISTS (SELECT 1 FROM blob_storage_hold h WHERE h.tenant_id = p.tenant_id AND h.org = p.org AND h.expires_at > now()) \
             AND NOT EXISTS (SELECT 1 FROM tenant t WHERE t.tenant_id = p.tenant_id AND t.status IN ('deleting', 'deleted'))")
            .fetch_all(&mut *tx).await?;
        tx.commit().await?;
        Ok(rows)
    }

    async fn gc_due(&self) -> Result<(), StorageError> {
        let rows = self.gc_scopes_due().await?;
        for (tenant, org) in rows {
            let tenant =
                TenantId::parse(tenant).map_err(|e| StorageError::Integrity(e.to_string()))?;
            if let Err(e) = self.collect(&tenant, &org).await {
                tracing::warn!(error = %e, tenant = %tenant, org, "組織の版実体回収を再試行します");
            }
        }
        Ok(())
    }

    async fn work(&self, options: VersionStorageOptions) -> Result<bool, StorageError> {
        let mut tx =
            crate::db_tenant::begin_cross_tenant_tx(&self.pool, "blob_storage_claim").await?;
        let jobs = jobq::claim(&mut tx, QUEUE, Duration::from_mins(10), 1)
            .await
            .map_err(|e| StorageError::Integrity(e.to_string()))?;
        tx.commit().await?;
        let Some(job) = jobs.into_iter().next() else {
            return Ok(false);
        };
        let tenant = TenantId::parse(job.tenant_id.clone())
            .map_err(|e| StorageError::Integrity(e.to_string()))?;
        let result = async {
            let payload: Payload = serde_json::from_value(job.payload)
                .map_err(|e| StorageError::Integrity(e.to_string()))?;
            let batch = self
                .compact_node(
                    &tenant,
                    &payload.org,
                    payload.node_id,
                    payload.after_version,
                    options,
                )
                .await?;
            let mut tx = crate::db_tenant::begin_tenant_tx(&self.pool, &tenant).await?;
            if batch.checked == 16 {
                Self::enqueue_after(
                    &mut tx,
                    &tenant,
                    &payload.org,
                    payload.node_id,
                    batch.after_version,
                )
                .await?;
            }
            jobq::ack(&mut tx, job.id)
                .await
                .map_err(|e| StorageError::Integrity(e.to_string()))?;
            tx.commit().await?;
            Ok::<_, StorageError>(())
        }
        .await;
        if let Err(e) = result {
            tracing::warn!(error = %e, job_id = job.id, "旧版の最適化を再試行します");
            let mut tx = crate::db_tenant::begin_tenant_tx(&self.pool, &tenant).await?;
            jobq::fail(&mut tx, job.id, &e.to_string(), Duration::from_mins(1))
                .await
                .map_err(|e| StorageError::Integrity(e.to_string()))?;
            tx.commit().await?;
        }
        Ok(true)
    }
}

/// 書込は同じ txn で jobq へ投入。旧版の backfill と GC もこの worker で行う。
pub fn spawn_version_storage_worker(
    storage: BlobStorage,
    options: VersionStorageOptions,
) -> tokio::task::JoinHandle<()> {
    tokio::spawn(async move {
        if !options.enabled {
            return;
        }
        if let Err(e) = options.validate() {
            tracing::error!(error = %e, "旧版最適化の設定が不正です");
            return;
        }
        let mut backfill_after = tokio::time::Instant::now();
        let mut gc_after = tokio::time::Instant::now();
        loop {
            let outcome = storage.work(options).await;
            if tokio::time::Instant::now() >= gc_after {
                if let Err(e) = storage.gc_due().await {
                    tracing::warn!(error = %e, "版実体の回収に失敗しました");
                }
                gc_after = tokio::time::Instant::now() + Duration::from_mins(1);
            }
            if !matches!(outcome, Ok(true)) {
                if tokio::time::Instant::now() >= backfill_after {
                    let mut enqueued = false;
                    let delay = match storage.backfill(options.max_optimize_bytes).await {
                        Ok(0) => 3600,
                        Ok(_) => {
                            enqueued = true;
                            0
                        }
                        Err(e) => {
                            tracing::warn!(error = %e, "旧版移行の投入に失敗しました");
                            60
                        }
                    };
                    backfill_after = tokio::time::Instant::now() + Duration::from_secs(delay);
                    if enqueued {
                        continue;
                    }
                }
                if let Err(e) = outcome {
                    tracing::warn!(error = %e, "旧版最適化 worker が失敗しました");
                }
                tokio::time::sleep(Duration::from_mins(1)).await;
            }
        }
    })
}
