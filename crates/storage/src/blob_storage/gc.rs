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
        let mut removed = 0;
        for sha in blobs {
            let current: bool = sqlx::query_scalar(
                "SELECT EXISTS (SELECT 1 FROM node \
                WHERE tenant_id = $1 AND org = $2 AND blob_sha256 = $3)",
            )
            .bind(tenant.as_str())
            .bind(org)
            .bind(&sha)
            .fetch_one(&mut *tx)
            .await?;
            if !current {
                self.store
                    .delete(&blob_object_key(tenant.as_str(), org, &sha))
                    .await?;
                removed += 1;
            }
            // false も再点検する。materialize 後の親 Tx rollback による raw も取り残さない。
            sqlx::query("UPDATE blob SET raw_available = $4, \
                raw_expires_at = CASE WHEN $4 THEN clock_timestamp() + interval '1 day' ELSE clock_timestamp() END, \
                raw_audit_at = clock_timestamp() + interval '1 day' \
                WHERE tenant_id = $1 AND org = $2 AND sha256 = $3")
                .bind(tenant.as_str()).bind(org).bind(&sha).bind(current).execute(&mut *tx).await?;
        }
        let objects: Vec<String> = sqlx::query_scalar("SELECT sha256 FROM blob_storage_object o \
            WHERE o.tenant_id = $1 AND o.org = $2 AND o.protected_until < now() AND o.refcount = 0 \
              AND NOT EXISTS (SELECT 1 FROM blob_storage_ref r WHERE r.tenant_id = $1 AND r.org = $2 AND r.object_sha256 = o.sha256) \
            ORDER BY protected_until LIMIT 16 FOR UPDATE SKIP LOCKED")
            .bind(tenant.as_str()).bind(org).fetch_all(&mut *tx).await?;
        for sha in objects {
            // FOR UPDATE の待機後の新しい snapshot で再確認する。FK の KEY SHARE と競合する。
            let unused: bool = sqlx::query_scalar(
                "SELECT NOT EXISTS (SELECT 1 FROM blob_storage_ref \
                WHERE tenant_id = $1 AND org = $2 AND object_sha256 = $3)",
            )
            .bind(tenant.as_str())
            .bind(org)
            .bind(&sha)
            .fetch_one(&mut *tx)
            .await?;
            if unused {
                self.store
                    .delete(&Self::object_key(tenant, org, &sha)?)
                    .await?;
                // tombstone を残し、期限を超えた遅延 PUT も次周で回収する。
                sqlx::query(
                    "UPDATE blob_storage_object SET collected_at = clock_timestamp(), \
                    protected_until = clock_timestamp() + interval '1 day' \
                    WHERE tenant_id = $1 AND org = $2 AND sha256 = $3",
                )
                .bind(tenant.as_str())
                .bind(org)
                .bind(&sha)
                .execute(&mut *tx)
                .await?;
                removed += 1;
            }
        }
        sqlx::query("DELETE FROM blob_storage_hold WHERE tenant_id = $1 AND org = $2 AND expires_at < now()")
            .bind(tenant.as_str()).bind(org).execute(&mut *tx).await?;
        tx.commit().await?;
        Ok(removed)
    }
}
