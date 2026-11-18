use super::BlobStorage;
use crate::{content_address::blob_object_key, StorageError};
use authz::TenantId;

impl BlobStorage {
    /// raw はキャッシュだけを回収し、論理版・提案・元の内容ハッシュは消さない。
    /// 削除後に DB commit が失敗しても、次回は保存表現から再生成できる。
    pub async fn collect(&self, tenant: &TenantId, org: &str) -> Result<usize, StorageError> {
        let mut tx = self.maintenance_guard(tenant).await?;
        Self::lock_gc(&mut tx, tenant, org).await?;
        let held: bool = sqlx::query_scalar(
            "SELECT EXISTS (SELECT 1 FROM blob_storage_hold \
            WHERE tenant_id = $1 AND org = $2 AND expires_at > now())",
        )
        .bind(tenant.as_str())
        .bind(org)
        .fetch_one(&mut *tx)
        .await?;
        if held {
            return Ok(0);
        }
        let mut blobs: Vec<String> = sqlx::query_scalar(
            "SELECT sha256 FROM blob \
            WHERE tenant_id = $1 AND org = $2 AND storage_manifest IS NOT NULL AND raw_available \
              AND raw_expires_at < now() ORDER BY raw_expires_at LIMIT 64 FOR UPDATE SKIP LOCKED",
        )
        .bind(tenant.as_str())
        .bind(org)
        .fetch_all(&mut *tx)
        .await?;
        let audit: Vec<String> = sqlx::query_scalar(
            "SELECT sha256 FROM blob
            WHERE tenant_id = $1 AND org = $2 AND storage_manifest IS NOT NULL AND NOT raw_available
              AND raw_audit_at < now() ORDER BY raw_audit_at LIMIT 16 FOR UPDATE SKIP LOCKED",
        )
        .bind(tenant.as_str())
        .bind(org)
        .fetch_all(&mut *tx)
        .await?;
        blobs.extend(audit);
        let current: Vec<String> = sqlx::query_scalar("SELECT DISTINCT blob_sha256 FROM node WHERE tenant_id = $1 AND org = $2 AND blob_sha256 = ANY($3)")
            .bind(tenant.as_str()).bind(org).bind(&blobs).fetch_all(&mut *tx).await?;
        let keys: Vec<String> = blobs
            .iter()
            .filter(|sha| !current.contains(sha))
            .map(|sha| blob_object_key(tenant.as_str(), org, sha))
            .collect();
        self.store.delete_batch(&keys).await?;
        let mut removed = keys.len();
        // falseも再点検。materialize後の親Tx rollbackによるrawを取り残さない。
        sqlx::query("UPDATE blob SET raw_available = (sha256 = ANY($4)), \
            raw_expires_at = CASE WHEN sha256 = ANY($4) THEN clock_timestamp() + interval '1 day' ELSE clock_timestamp() END, \
            raw_audit_at = clock_timestamp() + interval '1 day' \
            WHERE tenant_id = $1 AND org = $2 AND sha256 = ANY($3)")
            .bind(tenant.as_str()).bind(org).bind(&blobs).bind(&current).execute(&mut *tx).await?;
        let objects: Vec<String> = sqlx::query_scalar("SELECT sha256 FROM blob_storage_object o \
            WHERE o.tenant_id = $1 AND o.org = $2 AND o.protected_until < now() AND o.refcount = 0 \
              AND NOT EXISTS (SELECT 1 FROM blob_storage_ref r WHERE r.tenant_id = $1 AND r.org = $2 AND r.object_sha256 = o.sha256) \
            ORDER BY protected_until LIMIT 16 FOR UPDATE SKIP LOCKED")
            .bind(tenant.as_str()).bind(org).fetch_all(&mut *tx).await?;
        // 行ロック取得後の新snapshotで、公開された参照をまとめて再確認する。
        let unused: Vec<String> = sqlx::query_scalar("SELECT sha256 FROM blob_storage_object o WHERE tenant_id = $1 AND org = $2 \
            AND sha256 = ANY($3) AND NOT EXISTS (SELECT 1 FROM blob_storage_ref r WHERE r.tenant_id = $1 AND r.org = $2 AND r.object_sha256 = o.sha256)")
            .bind(tenant.as_str()).bind(org).bind(&objects).fetch_all(&mut *tx).await?;
        let keys = unused
            .iter()
            .map(|sha| Self::object_key(tenant, org, sha))
            .collect::<Result<Vec<_>, _>>()?;
        self.store.delete_batch(&keys).await?;
        removed += keys.len();
        // tombstoneを残し、期限を超えた遅延PUTも次周で回収する。
        sqlx::query(
            "UPDATE blob_storage_object SET collected_at = clock_timestamp(), \
            protected_until = clock_timestamp() + interval '1 day' \
            WHERE tenant_id = $1 AND org = $2 AND sha256 = ANY($3)",
        )
        .bind(tenant.as_str())
        .bind(org)
        .bind(&unused)
        .execute(&mut *tx)
        .await?;
        sqlx::query("DELETE FROM blob_storage_hold WHERE tenant_id = $1 AND org = $2 AND expires_at < now()")
            .bind(tenant.as_str()).bind(org).execute(&mut *tx).await?;
        sqlx::query("UPDATE blob_storage_scope SET next_check_at = GREATEST(clock_timestamp() + interval '1 minute', \
            COALESCE((SELECT min(deadline) FROM ( \
              (SELECT raw_expires_at AS deadline FROM blob WHERE tenant_id = $1 AND org = $2 AND storage_manifest IS NOT NULL AND raw_available ORDER BY raw_expires_at LIMIT 1) \
              UNION ALL (SELECT raw_audit_at FROM blob WHERE tenant_id = $1 AND org = $2 AND storage_manifest IS NOT NULL AND NOT raw_available ORDER BY raw_audit_at LIMIT 1) \
              UNION ALL (SELECT protected_until FROM blob_storage_object WHERE tenant_id = $1 AND org = $2 AND refcount = 0 ORDER BY protected_until LIMIT 1)) deadlines), \
              clock_timestamp() + interval '1 day')) WHERE tenant_id = $1 AND org = $2")
            .bind(tenant.as_str()).bind(org).execute(&mut *tx).await?;
        tx.commit().await?;
        Ok(removed)
    }
}
