use authz::TenantId;

use super::BlobStorage;
use crate::{db_tenant, StorageError};

static MAINTENANCE_LIMIT: tokio::sync::Semaphore = tokio::sync::Semaphore::const_new(1);

pub(super) async fn maintenance_slot() -> Result<tokio::sync::SemaphorePermit<'static>, StorageError>
{
    MAINTENANCE_LIMIT
        .acquire()
        .await
        .map_err(|e| StorageError::Invalid(e.to_string()))
}

/// データ面の全サブシステムの撤去を、バックアップ/保守書込と排他する。
pub struct TenantPurgeGuard {
    tenant: TenantId,
    _tx: db_tenant::TenantTx,
    _slot: tokio::sync::SemaphorePermit<'static>,
}

impl TenantPurgeGuard {
    pub fn tenant(&self) -> &TenantId {
        &self.tenant
    }
}

impl BlobStorage {
    pub(crate) async fn begin_tenant_purge(
        &self,
        tenant: &TenantId,
    ) -> Result<TenantPurgeGuard, StorageError> {
        if self.pool.options().get_max_connections() < 2 {
            return Err(StorageError::Invalid(
                "テナント撤去にはDB接続が2以上必要です".into(),
            ));
        }
        let slot = maintenance_slot().await?;
        let mut tx = db_tenant::begin_tenant_tx(&self.pool, tenant).await?;
        sqlx::query("SELECT pg_advisory_xact_lock(hashtextextended($1, 118488))")
            .bind(tenant.as_str())
            .execute(&mut *tx)
            .await?;
        let held: bool = sqlx::query_scalar("SELECT EXISTS (SELECT 1 FROM blob_storage_hold WHERE tenant_id = $1 AND expires_at > now())")
            .bind(tenant.as_str()).fetch_one(&mut *tx).await?;
        if held {
            return Err(StorageError::Invalid(
                "バックアップ保護中のためテナント撤去を再試行してください".into(),
            ));
        }
        Ok(TenantPurgeGuard {
            tenant: tenant.clone(),
            _tx: tx,
            _slot: slot,
        })
    }
}
