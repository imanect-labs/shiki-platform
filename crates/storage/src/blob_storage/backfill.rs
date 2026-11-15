use std::collections::BTreeMap;

use authz::TenantId;
use uuid::Uuid;

use super::{codec, BlobStorage};
use crate::StorageError;

impl BlobStorage {
    /// current/proposal を除く前に64件へ制限し、対象外は確認済みにする。
    pub(super) async fn backfill(&self, max_bytes: usize) -> Result<usize, StorageError> {
        let mut scan =
            crate::db_tenant::begin_cross_tenant_tx(&self.pool, "blob_storage_backfill").await?;
        // tenant-sql: allow(横断保守 executor の有界 backfill)
        let rows: Vec<(String, String, String)> = sqlx::query_as("SELECT tenant_id, org, sha256 FROM blob b \
            WHERE storage_checked_at IS NULL AND size_bytes <= $1 \
            AND NOT EXISTS (SELECT 1 FROM tenant t WHERE t.tenant_id = b.tenant_id AND t.status IN ('deleting', 'deleted')) \
            ORDER BY size_bytes, created_at, tenant_id, org, sha256 LIMIT 64")
            .bind(max_bytes.min(codec::MAX_BYTES) as i64).fetch_all(&mut *scan).await?;
        scan.commit().await?;
        let mut checked = 0;
        let mut groups: BTreeMap<(String, String), Vec<String>> = BTreeMap::new();
        for (tenant, org, sha) in rows {
            groups.entry((tenant, org)).or_default().push(sha);
        }
        for ((tenant, org), hashes) in groups {
            let tenant =
                TenantId::parse(tenant).map_err(|e| StorageError::Integrity(e.to_string()))?;
            let mut tx = match self.maintenance_guard(&tenant).await {
                Ok(tx) => tx,
                Err(StorageError::NotFound) => continue,
                Err(e) => return Err(e),
            };
            // 同じblobの最適化・他workerのbackfillと順序を確定してから現在版を確認する。
            let locked: Vec<String> = sqlx::query_scalar("SELECT sha256 FROM blob WHERE tenant_id = $1 AND org = $2 \
                AND sha256 = ANY($3) AND storage_checked_at IS NULL ORDER BY sha256 FOR UPDATE SKIP LOCKED")
                .bind(tenant.as_str()).bind(&org).bind(&hashes).fetch_all(&mut *tx).await?;
            let nodes: Vec<Uuid> = sqlx::query_scalar("SELECT DISTINCT v.node_id FROM blob b \
                CROSS JOIN LATERAL (SELECT node_id FROM node_version v WHERE v.tenant_id = $1 AND v.org = $2 \
                  AND v.blob_sha256 = b.sha256 AND NOT v.is_proposal LIMIT 1) v \
                WHERE b.tenant_id = $1 AND b.org = $2 AND b.sha256 = ANY($3) \
                AND NOT EXISTS (SELECT 1 FROM node n WHERE n.tenant_id = $1 AND n.org = $2 AND n.blob_sha256 = b.sha256)")
                .bind(tenant.as_str()).bind(&org).bind(&locked).fetch_all(&mut *tx).await?;
            // enqueue と確認済みの印は同じ Tx。失敗した仕事はjobqの再試行/DLQへ任せる。
            sqlx::query("UPDATE blob b SET storage_checked_at = clock_timestamp() WHERE b.tenant_id = $1 AND b.org = $2 \
                AND b.sha256 = ANY($3)")
                .bind(tenant.as_str()).bind(&org).bind(&locked).execute(&mut *tx).await?;
            for node in nodes {
                Self::enqueue(&mut tx, &tenant, &org, node).await?;
            }
            tx.commit().await?;
            checked += locked.len();
        }
        Ok(checked)
    }
}
