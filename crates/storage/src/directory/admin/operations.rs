use authz::AuthContext;
use sha2::{Digest, Sha256};
use uuid::Uuid;

use super::{DirectoryAdminStore, DirectoryChangeSet, DirectoryOperation};
use crate::{
    audit::{record_on, AuditAction, AuditEntry, Chain, Decision},
    db_tenant::begin_tenant_tx,
    error::StorageError,
};

pub fn request_hash(request: &DirectoryChangeSet) -> Result<String, StorageError> {
    Ok(hex::encode(Sha256::digest(
        serde_json::to_vec(request).map_err(|e| StorageError::Invalid(e.to_string()))?,
    )))
}
impl DirectoryAdminStore {
    pub async fn existing_request(
        &self,
        ctx: &AuthContext,
        key: Uuid,
        hash: &str,
    ) -> Result<Option<DirectoryOperation>, StorageError> {
        let mut tx = begin_tenant_tx(&self.db, ctx.tenant()).await?;
        let found: Option<(Uuid, String)> = sqlx::query_as("SELECT operation_id, request_hash FROM directory_admin_operation WHERE tenant_id = $1 AND org = $2 AND idempotency_key = $3").bind(ctx.tenant_id()).bind(ctx.org()).bind(key).fetch_optional(&mut *tx).await?;
        tx.commit().await?;
        if let Some((id, stored)) = found {
            if stored != hash {
                return Err(StorageError::Conflict);
            }
            return Ok(Some(self.operation(ctx, id).await?));
        }
        Ok(None)
    }
    /// 監査と操作キューを同一トランザクションで永続化する。
    pub async fn enqueue(
        &self,
        ctx: &AuthContext,
        changes: &DirectoryChangeSet,
        hash: &str,
        trace: Option<&str>,
    ) -> Result<DirectoryOperation, StorageError> {
        let json =
            serde_json::to_value(changes).map_err(|e| StorageError::Invalid(e.to_string()))?;
        let mut tx = begin_tenant_tx(&self.db, ctx.tenant()).await?;
        let active: bool = sqlx::query_scalar("SELECT EXISTS (SELECT 1 FROM directory_admin_operation WHERE tenant_id = $1 AND org = $2 AND state <> 'succeeded')").bind(ctx.tenant_id()).bind(ctx.org()).fetch_one(&mut *tx).await?;
        if active {
            return Err(StorageError::Conflict);
        }
        let id = Uuid::new_v4();
        sqlx::query(
            "INSERT INTO directory_admin_operation \
             (tenant_id, org, operation_id, actor_id, trace_id, idempotency_key, request_hash, request, total) \
             VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9) ON CONFLICT (tenant_id, org, idempotency_key) DO NOTHING",
        ).bind(ctx.tenant_id()).bind(ctx.org()).bind(id).bind(&ctx.principal().id)
            .bind(trace).bind(changes.idempotency_key).bind(hash).bind(json).bind(i32::try_from((changes.roles.len() + changes.mappings.len()).max(1)).map_err(|_| StorageError::Invalid("変更件数が多すぎます".into()))?)
            .execute(&mut *tx).await?;
        let stored: (Uuid, String) = sqlx::query_as(
            "SELECT operation_id, request_hash FROM directory_admin_operation \
             WHERE tenant_id = $1 AND org = $2 AND idempotency_key = $3",
        )
        .bind(ctx.tenant_id())
        .bind(ctx.org())
        .bind(changes.idempotency_key)
        .fetch_one(&mut *tx)
        .await?;
        if stored.1 != hash {
            return Err(StorageError::Conflict);
        }
        if stored.0 == id {
            record_on(&mut tx, ctx, AuditEntry {
                action: AuditAction::OrgAdminDirectoryUpdate, object_type: "directory_operation",
                object_id: &id.to_string(), decision: Decision::Allow, trace_id: trace,
                metadata: serde_json::json!({ "roles": changes.roles.len(), "mappings": changes.mappings.len() }),
            }, Chain::Yes).await?;
        }
        tx.commit().await?;
        self.operation(ctx, stored.0).await
    }

    pub async fn operations(
        &self,
        ctx: &AuthContext,
    ) -> Result<Vec<DirectoryOperation>, StorageError> {
        let mut tx = begin_tenant_tx(&self.db, ctx.tenant()).await?;
        let operations = sqlx::query_as("SELECT operation_id AS id, state, error_code, attempts, processed, total FROM directory_admin_operation WHERE tenant_id = $1 AND org = $2 ORDER BY created_at DESC LIMIT 20").bind(ctx.tenant_id()).bind(ctx.org()).fetch_all(&mut *tx).await?;
        tx.commit().await?;
        Ok(operations)
    }
    pub async fn operation(
        &self,
        ctx: &AuthContext,
        id: Uuid,
    ) -> Result<DirectoryOperation, StorageError> {
        let mut tx = begin_tenant_tx(&self.db, ctx.tenant()).await?;
        let operation = sqlx::query_as(
            "SELECT operation_id AS id, state, error_code, attempts, processed, total FROM directory_admin_operation \
             WHERE tenant_id = $1 AND org = $2 AND operation_id = $3",
        ).bind(ctx.tenant_id()).bind(ctx.org()).bind(id).fetch_optional(&mut *tx).await?
            .ok_or(StorageError::NotFound)?;
        tx.commit().await?;
        Ok(operation)
    }
}

/// 各変更と同一transaction内で進捗を進める。最後の変更と成功状態も原子的。
pub(super) async fn advance(
    connection: &mut sqlx::PgConnection,
    ctx: &AuthContext,
    id: Uuid,
    step: i32,
    kind: &str,
    object: &str,
) -> Result<(), StorageError> {
    let trace: Option<Option<String>> = sqlx::query_scalar(
        "UPDATE directory_admin_operation SET processed = processed + 1, updated_at = now(), \
         state = CASE WHEN processed + 1 = total THEN 'succeeded' ELSE 'running' END \
         WHERE tenant_id = $1 AND org = $2 AND operation_id = $3 AND processed = $4 AND state = 'running' RETURNING trace_id",
    ).bind(ctx.tenant_id()).bind(ctx.org()).bind(id).bind(step).fetch_optional(&mut *connection).await?;
    let trace = trace.ok_or(StorageError::Conflict)?;
    record_on(
        connection,
        ctx,
        AuditEntry {
            action: AuditAction::OrgAdminDirectoryApply,
            object_type: kind,
            object_id: object,
            decision: Decision::Allow,
            trace_id: trace.as_deref(),
            metadata: serde_json::json!({ "operation_id": id, "step": step }),
        },
        Chain::Yes,
    )
    .await
}

impl DirectoryAdminStore {
    pub async fn save_mapping(
        &self,
        ctx: &AuthContext,
        mapping: &super::GroupRoleMapping,
        operation: Uuid,
        step: i32,
    ) -> Result<(), StorageError> {
        let mut tx = begin_tenant_tx(&self.db, ctx.tenant()).await?;
        let updated = sqlx::query(
            "UPDATE directory_ad_group SET mapping_version = mapping_version + 1 WHERE tenant_id = $1 AND org = $2 AND group_id = $3 AND mapping_version = $4",
        ).bind(ctx.tenant_id()).bind(ctx.org()).bind(&mapping.group_id).bind(mapping.expected_version).execute(&mut *tx).await?;
        if updated.rows_affected() != 1 {
            return Err(StorageError::Conflict);
        }
        sqlx::query(
            "DELETE FROM directory_ad_mapping WHERE tenant_id = $1 AND org = $2 AND group_id = $3",
        )
        .bind(ctx.tenant_id())
        .bind(ctx.org())
        .bind(&mapping.group_id)
        .execute(&mut *tx)
        .await?;
        let added = sqlx::query("INSERT INTO directory_ad_mapping (tenant_id, org, group_id, role_id) SELECT tenant_id, org, $3, role_id FROM directory_role WHERE tenant_id = $1 AND org = $2 AND role_id = ANY($4)").bind(ctx.tenant_id()).bind(ctx.org()).bind(&mapping.group_id).bind(&mapping.role_ids).execute(&mut *tx).await?;
        if added.rows_affected() != mapping.role_ids.len() as u64 {
            return Err(StorageError::NotFound);
        }
        advance(
            &mut tx,
            ctx,
            operation,
            step,
            "directory_group",
            &mapping.group_id,
        )
        .await?;
        tx.commit().await?;
        Ok(())
    }

    pub async fn retry_operation(
        &self,
        ctx: &AuthContext,
        id: Uuid,
        trace: Option<&str>,
    ) -> Result<DirectoryOperation, StorageError> {
        let mut tx = begin_tenant_tx(&self.db, ctx.tenant()).await?;
        let updated = sqlx::query(
            "UPDATE directory_admin_operation SET state = 'pending', actor_id = $4, trace_id = $5, error_code = NULL, updated_at = now() \
             WHERE tenant_id = $1 AND org = $2 AND operation_id = $3 AND state = 'failed'",
        ).bind(ctx.tenant_id()).bind(ctx.org()).bind(id).bind(&ctx.principal().id).bind(trace).execute(&mut *tx).await?;
        if updated.rows_affected() != 1 {
            return Err(StorageError::Conflict);
        }
        record_on(
            &mut tx,
            ctx,
            AuditEntry {
                action: AuditAction::OrgAdminDirectoryUpdate,
                object_type: "directory_operation",
                object_id: &id.to_string(),
                decision: Decision::Allow,
                trace_id: trace,
                metadata: serde_json::json!({ "retry": true }),
            },
            Chain::Yes,
        )
        .await?;
        tx.commit().await?;
        self.operation(ctx, id).await
    }
}

impl DirectoryAdminStore {
    /// 接続のスコープ・階層設定の変更時は、外部書込み前に旧スナップショットを無効化する。
    pub async fn configuration_event(
        &self,
        ctx: &AuthContext,
        outcome: &str,
        trace: Option<&str>,
    ) -> Result<(), StorageError> {
        let mut tx = begin_tenant_tx(&self.db, ctx.tenant()).await?;
        if outcome == "snapshot_invalidated" {
            sqlx::query("UPDATE directory_ad_group SET active = false, mapping_version = mapping_version + 1 WHERE tenant_id = $1 AND org = $2 AND active").bind(ctx.tenant_id()).bind(ctx.org()).execute(&mut *tx).await?;
        }
        record_on(
            &mut tx,
            ctx,
            AuditEntry {
                action: AuditAction::OrgAdminDirectoryUpdate,
                object_type: "directory_connection",
                object_id: "ad",
                decision: Decision::Allow,
                trace_id: trace,
                metadata: serde_json::json!({"outcome":outcome}),
            },
            Chain::Yes,
        )
        .await?;
        tx.commit().await?;
        Ok(())
    }
}

impl DirectoryAdminStore {
    pub async fn has_unfinished_mappings(&self, ctx: &AuthContext) -> Result<bool, StorageError> {
        let mut tx = begin_tenant_tx(&self.db, ctx.tenant()).await?;
        let unfinished = sqlx::query_scalar("SELECT EXISTS (SELECT 1 FROM directory_admin_operation WHERE tenant_id = $1 AND org = $2 AND state <> 'succeeded' AND jsonb_array_length(request->'mappings') > 0)").bind(ctx.tenant_id()).bind(ctx.org()).fetch_one(&mut *tx).await?;
        tx.commit().await?;
        Ok(unfinished)
    }

    pub async fn has_active_operation(&self, ctx: &AuthContext) -> Result<bool, StorageError> {
        let mut tx = begin_tenant_tx(&self.db, ctx.tenant()).await?;
        let active = sqlx::query_scalar("SELECT EXISTS (SELECT 1 FROM directory_admin_operation WHERE tenant_id = $1 AND org = $2 AND state IN ('pending','running'))").bind(ctx.tenant_id()).bind(ctx.org()).fetch_one(&mut *tx).await?;
        tx.commit().await?;
        Ok(active)
    }
}
