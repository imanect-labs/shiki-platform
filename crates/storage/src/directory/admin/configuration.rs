//! 外部設定の不確定な書込みを、確認済みの復旧まで隔離する。秘密値は保存しない。
use super::{DirectoryAdminStore, DirectoryLease};
use crate::{db_tenant::begin_tenant_tx, error::StorageError};
use authz::AuthContext;
use sqlx::Connection;
use uuid::Uuid;

impl DirectoryAdminStore {
    pub async fn connection_recovery_required(
        &self,
        ctx: &AuthContext,
    ) -> Result<bool, StorageError> {
        let mut tx = begin_tenant_tx(&self.db, ctx.tenant()).await?;
        let required = sqlx::query_scalar("SELECT EXISTS (SELECT 1 FROM directory_connection_guard WHERE tenant_id = $1 AND org = $2 AND NOT verified)")
            .bind(ctx.tenant_id()).bind(ctx.org()).fetch_one(&mut *tx).await?;
        tx.commit().await?;
        Ok(required)
    }

    /// 呼出側は組織leaseを保持する。世代IDが古い完了通知による上書きを防ぐ。
    pub async fn begin_connection_change(
        &self,
        ctx: &AuthContext,
        lease: &DirectoryLease,
        confirmed: bool,
        trace: Option<&str>,
    ) -> Result<Uuid, StorageError> {
        let attempt = Uuid::new_v4();
        // 開始記録とロックの生存を同じDBセッションで保証する。
        let mut connection = lease.connection.lock().await;
        let mut tx = connection.begin().await?;
        crate::db_tenant::set_tenant(&mut tx, ctx.tenant()).await?;
        let updated = sqlx::query("INSERT INTO directory_connection_guard (tenant_id, org, attempt_id) VALUES ($1, $2, $3) ON CONFLICT (tenant_id, org) DO UPDATE SET attempt_id = EXCLUDED.attempt_id, verified = false, updated_at = now() WHERE directory_connection_guard.verified OR $4")
            .bind(ctx.tenant_id()).bind(ctx.org()).bind(attempt).bind(confirmed).execute(&mut *tx).await?;
        if updated.rows_affected() != 1 {
            return Err(StorageError::Conflict);
        }
        crate::audit::record_on(&mut tx, ctx, crate::audit::AuditEntry {
            action: crate::audit::AuditAction::OrgAdminDirectoryUpdate, object_type: "directory_connection", object_id: "ad",
            decision: crate::audit::Decision::Allow, trace_id: trace,
            metadata: serde_json::json!({"outcome": "started", "recovery_confirmed": confirmed, "attempt_id": attempt}),
        }, crate::audit::Chain::Yes).await?;
        tx.commit().await?;
        Ok(attempt)
    }

    pub async fn finish_connection_change(
        &self,
        ctx: &AuthContext,
        attempt: Uuid,
    ) -> Result<(), StorageError> {
        let mut tx = begin_tenant_tx(&self.db, ctx.tenant()).await?;
        let updated = sqlx::query("UPDATE directory_connection_guard SET verified = true, updated_at = now() WHERE tenant_id = $1 AND org = $2 AND attempt_id = $3")
            .bind(ctx.tenant_id()).bind(ctx.org()).bind(attempt).execute(&mut *tx).await?;
        if updated.rows_affected() != 1 {
            return Err(StorageError::Conflict);
        }
        tx.commit().await?;
        Ok(())
    }
}
