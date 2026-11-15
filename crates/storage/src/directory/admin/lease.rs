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
    pub(super) connection: tokio::sync::Mutex<sqlx::PgConnection>,
}

impl DirectoryLease {
    /// 外部書込みの直前にもロック用セッションの生存を確認する。
    pub async fn verify(&self) -> Result<(), StorageError> {
        tokio::time::timeout(std::time::Duration::from_secs(5), async {
            self.connection.lock().await.ping().await
        })
        .await
        .map_err(|_| StorageError::Locked)??;
        Ok(())
    }

    /// 接続が失われたら進行中のfutureを破棄する。外部で受付済みの処理は取り消せない。
    pub async fn while_live<F: std::future::Future>(
        &self,
        work: F,
    ) -> Result<F::Output, StorageError> {
        self.verify().await?;
        let monitor = async {
            let mut timer = tokio::time::interval(std::time::Duration::from_secs(1));
            loop {
                timer.tick().await;
                if let Err(error) = self.verify().await {
                    return error;
                }
            }
        };
        tokio::select! {
            biased;
            lost = monitor => Err(lost),
            result = work => Ok(result),
        }
    }
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
        // 設定変更の開始記録だけは同じセッションでtenant文脈付きtransactionを開く。
        let name: String = format!("shiki-dir-{}", super::role_id_prefix(ctx))
            .chars()
            .take(63)
            .collect();
        let options = self
            .db
            .connect_options()
            .as_ref()
            .clone()
            .application_name(&name);
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
            connection: tokio::sync::Mutex::new(connection),
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
        if !locked {
            return Ok(None);
        }
        // 外部で受付済みの遅延書込みは、撤去しても取り消せない。確認済み復旧まで撤去を拒否する。
        let mut tx = begin_tenant_tx(&self.db, tenant).await?;
        let uncertain: bool = sqlx::query_scalar("SELECT EXISTS (SELECT 1 FROM directory_connection_guard WHERE tenant_id = $1 AND NOT verified) OR EXISTS (SELECT 1 FROM directory_admin_operation WHERE tenant_id = $1 AND (state = 'running' OR error_code = 'execution_interrupted'))")
            .bind(tenant.as_str()).fetch_one(&mut *tx).await?;
        tx.commit().await?;
        Ok((!uncertain).then_some(DirectoryLease {
            connection: tokio::sync::Mutex::new(connection),
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
        Ok(self
            .start_owned_operation(ctx, id)
            .await?
            .map(|(request, processed, _)| (request, processed)))
    }

    pub async fn start_owned_operation(
        &self,
        ctx: &AuthContext,
        id: Uuid,
    ) -> Result<Option<(DirectoryChangeSet, usize, i32)>, StorageError> {
        let mut tx = begin_tenant_tx(&self.db, ctx.tenant()).await?;
        let row: Option<(Value, i32, i32)> = sqlx::query_as(
            "UPDATE directory_admin_operation SET state = 'running', attempts = attempts + 1, updated_at = now() \
             WHERE tenant_id = $1 AND org = $2 AND operation_id = $3 AND state = 'pending' \
             RETURNING request, processed, attempts",
        ).bind(ctx.tenant_id()).bind(ctx.org()).bind(id).fetch_optional(&mut *tx).await?;
        tx.commit().await?;
        row.map(|(request, processed, attempt)| {
            let request = serde_json::from_value(request)
                .map_err(|e| StorageError::Invalid(e.to_string()))?;
            Ok((
                request,
                usize::try_from(processed)
                    .map_err(|_| StorageError::Invalid("進行状態が不正です".into()))?,
                attempt,
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
        sqlx::query("UPDATE directory_admin_operation SET state = 'failed', error_code = $4, updated_at = now() WHERE tenant_id = $1 AND org = $2 AND operation_id = $3 AND state IN ('pending', 'running')")
            .bind(ctx.tenant_id()).bind(ctx.org()).bind(id).bind(code).execute(&mut *tx).await?;
        tx.commit().await?;
        Ok(())
    }

    /// 旧実行の失敗通知は、後から開始した実行や確認待ちの状態を上書きしない。
    pub async fn fail_owned_operation(
        &self,
        ctx: &AuthContext,
        id: Uuid,
        attempt: i32,
        code: &str,
    ) -> Result<(), StorageError> {
        let mut tx = begin_tenant_tx(&self.db, ctx.tenant()).await?;
        sqlx::query("UPDATE directory_admin_operation SET state = 'failed', error_code = $4, updated_at = now() WHERE tenant_id = $1 AND org = $2 AND operation_id = $3 AND attempts = $5 AND state = 'running'")
            .bind(ctx.tenant_id()).bind(ctx.org()).bind(id).bind(code).bind(attempt).execute(&mut *tx).await?;
        tx.commit().await?;
        Ok(())
    }
}
