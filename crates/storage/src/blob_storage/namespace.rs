//! retenant の copy → DB rename → delete を、最適化/GC/backup と排他する保守 capability。
use authz::TenantId;

use super::BlobStorage;
use crate::{db_tenant, StorageError};

pub struct NamespaceMove {
    storage: BlobStorage,
    from: TenantId,
    to: TenantId,
    _guard: db_tenant::CrossTenantTx,
    _slot: tokio::sync::SemaphorePermit<'static>,
}

impl BlobStorage {
    /// raw/FGA の移送を始める前に取得し、旧キー削除まで保持する。
    pub async fn begin_namespace_move(
        &self,
        from: TenantId,
        to: TenantId,
    ) -> Result<NamespaceMove, StorageError> {
        if from == to {
            return Err(StorageError::Invalid("移行元と移行先が同じです".into()));
        }
        if self.pool.options().get_max_connections() < 2 {
            return Err(StorageError::Invalid(
                "テナント移行にはDB接続が2以上必要です".into(),
            ));
        }
        let slot = super::purge::maintenance_slot().await?;
        let mut guard =
            db_tenant::begin_cross_tenant_tx(&self.pool, "blob_storage_namespace_move").await?;
        let mut ids = vec![from.as_str(), to.as_str()];
        ids.sort_unstable();
        for tenant in &ids {
            sqlx::query("SELECT pg_advisory_xact_lock(hashtextextended($1, 118488))")
                .bind(*tenant)
                .execute(&mut *guard)
                .await?;
        }
        let held: bool = sqlx::query_scalar("SELECT EXISTS (SELECT 1 FROM blob_storage_hold WHERE tenant_id = ANY($1) AND expires_at > now())")
            .bind(&ids).fetch_one(&mut *guard).await?;
        if held {
            return Err(StorageError::Invalid(
                "バックアップ保護中のテナントは移行できません".into(),
            ));
        }
        Ok(NamespaceMove {
            storage: self.clone(),
            from,
            to,
            _guard: guard,
            _slot: slot,
        })
    }

    async fn namespace_page(
        &self,
        tenant: &TenantId,
        after: Option<&(String, String)>,
        live: bool,
    ) -> Result<Vec<(String, String)>, StorageError> {
        let mut tx = db_tenant::begin_tenant_tx(&self.pool, tenant).await?;
        let rows = sqlx::query_as(
            "SELECT org, sha256 FROM blob_storage_object WHERE tenant_id = $1 \
            AND ($2::text IS NULL OR (org, sha256) > ($2, $3)) AND (NOT $4 OR refcount > 0) \
            ORDER BY org, sha256 LIMIT 1000",
        )
        .bind(tenant.as_str())
        .bind(after.map(|v| v.0.as_str()))
        .bind(after.map(|v| v.1.as_str()))
        .bind(live)
        .fetch_all(&mut *tx)
        .await?;
        tx.commit().await?;
        Ok(rows)
    }
}

impl NamespaceMove {
    /// 現在 manifest が必要とする物理実体だけをコピーする。DB はまだ変更しない。
    pub async fn copy_objects(&self) -> Result<usize, StorageError> {
        let mut after = None;
        let mut copied = 0;
        loop {
            let rows = self
                .storage
                .namespace_page(&self.from, after.as_ref(), true)
                .await?;
            if rows.is_empty() {
                break;
            }
            for (org, sha) in &rows {
                let old = BlobStorage::object_key(&self.from, org, sha)?;
                let new = BlobStorage::object_key(&self.to, org, sha)?;
                if !self.storage.store.exists(&new).await? {
                    self.storage.store.copy(&old, &new).await?;
                }
                if self.storage.store.read_and_hash(&new).await?.0 != *sha {
                    return Err(StorageError::Integrity(
                        "移行先の物理実体ハッシュが一致しません".into(),
                    ));
                }
                copied += 1;
            }
            after = rows.last().cloned();
        }
        Ok(copied)
    }

    /// DB rename の commit 後だけ呼べる。tombstone に対応する旧キーも再試行時に回収する。
    pub async fn cleanup_old_objects(&self) -> Result<usize, StorageError> {
        let mut tx = db_tenant::begin_tenant_tx(&self.storage.pool, &self.from).await?;
        let still_old: bool = sqlx::query_scalar(
            "SELECT EXISTS (SELECT 1 FROM blob WHERE tenant_id = $1) \
            OR EXISTS (SELECT 1 FROM blob_storage_object WHERE tenant_id = $1)",
        )
        .bind(self.from.as_str())
        .fetch_one(&mut *tx)
        .await?;
        tx.commit().await?;
        if still_old {
            return Err(StorageError::Invalid(
                "DB移行のcommit前に旧実体を削除できません".into(),
            ));
        }
        let mut after = None;
        let mut removed = 0;
        loop {
            let rows = self
                .storage
                .namespace_page(&self.to, after.as_ref(), false)
                .await?;
            if rows.is_empty() {
                break;
            }
            let keys = rows
                .iter()
                .map(|(org, sha)| BlobStorage::object_key(&self.from, org, sha))
                .collect::<Result<Vec<_>, _>>()?;
            self.storage.store.delete_batch(&keys).await?;
            removed += keys.len();
            after = rows.last().cloned();
        }
        Ok(removed)
    }
}
