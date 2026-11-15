use super::{DirectoryAdminStore, DirectoryChangeSet};
use crate::{
    db_tenant::{begin_cross_tenant_tx, begin_tenant_tx},
    error::StorageError,
};
use authz::AuthContext;
use serde_json::Value;
use sqlx::Connection;
use uuid::Uuid;

/// DBセッション内の組織ロック。生の接続をAPIへ公開しない。dropで解放する。
pub struct DirectoryLease {
    _connection: sqlx::PgConnection,
}

#[derive(sqlx::FromRow)]
pub struct PendingDirectoryOperation {
    pub tenant_id: String,
    pub org: String,
    pub actor_id: String,
    pub id: Uuid,
}

impl DirectoryAdminStore {
    pub async fn lease(&self, ctx: &AuthContext) -> Result<Option<DirectoryLease>, StorageError> {
        // 長い外部同期のロックで通常クエリ用poolを占有しない。ロック専用接続はdropで閉じる。
        // この接続ではtenantデータを読まない。全データ操作はbegin_tenant_txを通す。
        let options = self.db.connect_options();
        let mut connection = tokio::time::timeout(
            std::time::Duration::from_secs(10),
            sqlx::PgConnection::connect_with(&options),
        )
        .await
        .map_err(|_| StorageError::Locked)??;
        let tenant_key = format!("directory-admin-tenant:{}", ctx.tenant_id());
        let tenant_locked: bool =
            sqlx::query_scalar("SELECT pg_try_advisory_lock_shared(hashtextextended($1, 0))")
                .bind(tenant_key)
                .fetch_one(&mut connection)
                .await?;
        if !tenant_locked {
            return Ok(None);
        }
        let key = format!("directory-admin:{}:{}", ctx.tenant_id(), ctx.org());
        let locked: bool =
            sqlx::query_scalar("SELECT pg_try_advisory_lock(hashtextextended($1, 0))")
                .bind(key)
                .fetch_one(&mut connection)
                .await?;
        Ok(locked.then_some(DirectoryLease {
            _connection: connection,
        }))
    }

    /// テナント撤去はすべての組織の同期操作と排他にする。ロック中の操作があれば撤去を開始しない。
    pub async fn tenant_deletion_lease(
        &self,
        tenant: &authz::TenantId,
    ) -> Result<Option<DirectoryLease>, StorageError> {
        let options = self.db.connect_options();
        let mut connection = tokio::time::timeout(
            std::time::Duration::from_secs(10),
            sqlx::PgConnection::connect_with(&options),
        )
        .await
        .map_err(|_| StorageError::Locked)??;
        let locked: bool =
            sqlx::query_scalar("SELECT pg_try_advisory_lock(hashtextextended($1, 0))")
                .bind(format!("directory-admin-tenant:{tenant}"))
                .fetch_one(&mut connection)
                .await?;
        Ok(locked.then_some(DirectoryLease {
            _connection: connection,
        }))
    }
    /// 運用ワーカーのテナント横断入口。詳細の操作は返したtenant/orgのAuthContextで行う。
    pub async fn pending_operations(&self) -> Result<Vec<PendingDirectoryOperation>, StorageError> {
        let mut tx = begin_cross_tenant_tx(&self.db, "directory-admin-worker").await?;
        // tenant-sql: allow(全テナントの永続化済み管理ジョブを発見する運用ワーカー。処理は組織ロック＋再認可)
        let pending = sqlx::query_as(
            "SELECT tenant_id, org, actor_id, operation_id AS id FROM directory_admin_operation \
             WHERE state = 'pending' OR (state = 'running' AND updated_at < now() - interval '2 minutes') \
             ORDER BY created_at LIMIT 20",
        ).fetch_all(&mut *tx).await?;
        tx.commit().await?;
        Ok(pending)
    }

    pub async fn start_operation(
        &self,
        ctx: &AuthContext,
        id: Uuid,
    ) -> Result<Option<(DirectoryChangeSet, usize)>, StorageError> {
        let mut tx = begin_tenant_tx(&self.db, ctx.tenant()).await?;
        let row: Option<(Value, i32)> = sqlx::query_as(
            "UPDATE directory_admin_operation SET state = 'running', attempts = attempts + 1, updated_at = now() \
             WHERE tenant_id = $1 AND org = $2 AND operation_id = $3 AND state IN ('pending', 'running') \
             RETURNING request, processed",
        ).bind(ctx.tenant_id()).bind(ctx.org()).bind(id).fetch_optional(&mut *tx).await?;
        tx.commit().await?;
        row.map(|(request, processed)| {
            let request = serde_json::from_value(request)
                .map_err(|e| StorageError::Invalid(e.to_string()))?;
            Ok((
                request,
                usize::try_from(processed)
                    .map_err(|_| StorageError::Invalid("進行状態が不正です".into()))?,
            ))
        })
        .transpose()
    }

    pub async fn fail_operation(
        &self,
        ctx: &AuthContext,
        id: Uuid,
        code: &str,
    ) -> Result<(), StorageError> {
        let mut tx = begin_tenant_tx(&self.db, ctx.tenant()).await?;
        sqlx::query("UPDATE directory_admin_operation SET state = 'failed', error_code = $4, updated_at = now() WHERE tenant_id = $1 AND org = $2 AND operation_id = $3 AND state <> 'succeeded'")
            .bind(ctx.tenant_id()).bind(ctx.org()).bind(id).bind(code).execute(&mut *tx).await?;
        tx.commit().await?;
        Ok(())
    }
}
