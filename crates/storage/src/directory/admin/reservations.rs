//! 外部へ階層を書く前に新規IDの所有権を確保する。claims同期によるorg変更もFKで防ぐ。
use super::{DirectoryAdminStore, RoleEdit};
use crate::{
    audit::{record_on, AuditAction, AuditEntry, Chain, Decision},
    db_tenant::begin_tenant_tx,
    error::StorageError,
};
use authz::AuthContext;
use uuid::Uuid;
impl DirectoryAdminStore {
    /// 新規IDを同一transactionで予約する。処理途中の定義は管理一覧へ公開しない。
    pub async fn reserve_new_roles(
        &self,
        ctx: &AuthContext,
        roles: &[RoleEdit],
        operation: Uuid,
    ) -> Result<(), StorageError> {
        let fresh: Vec<_> = roles
            .iter()
            .filter(|role| role.expected_version == 0)
            .collect();
        if fresh.is_empty() {
            return Ok(());
        }
        if fresh
            .iter()
            .any(|role| !super::is_managed_id(ctx, &role.id))
        {
            return Err(StorageError::Invalid("ロールIDが不正です".into()));
        }
        let mut tx = begin_tenant_tx(&self.db, ctx.tenant()).await?;
        let trace: Option<String> = sqlx::query_scalar("SELECT trace_id FROM directory_admin_operation WHERE tenant_id = $1 AND org = $2 AND operation_id = $3 AND state = 'running'").bind(ctx.tenant_id()).bind(ctx.org()).bind(operation).fetch_optional(&mut *tx).await?.ok_or(StorageError::Conflict)?;
        let inserted: Vec<String> = sqlx::query_scalar("INSERT INTO directory_role (tenant_id, org, role_id, display_name) SELECT $1, $2, r.id, r.label FROM jsonb_to_recordset($3) AS r(id text, label text) ON CONFLICT (tenant_id, role_id) DO NOTHING RETURNING role_id").bind(ctx.tenant_id()).bind(ctx.org()).bind(serde_json::to_value(&fresh).map_err(|e| StorageError::Invalid(e.to_string()))?).fetch_all(&mut *tx).await?;
        sqlx::query("INSERT INTO directory_managed_role (tenant_id, org, role_id, label, version) SELECT tenant_id, org, role_id, '処理中-' || gen_random_uuid()::text, 0 FROM directory_role WHERE tenant_id = $1 AND org = $2 AND role_id = ANY($3)").bind(ctx.tenant_id()).bind(ctx.org()).bind(&inserted).execute(&mut *tx).await?;
        let ids: Vec<_> = fresh.iter().map(|role| &role.id).collect();
        let reserved: i64 = sqlx::query_scalar("SELECT count(*) FROM directory_managed_role WHERE tenant_id = $1 AND org = $2 AND role_id = ANY($3) AND version = 0 AND keycloak_group_id IS NULL").bind(ctx.tenant_id()).bind(ctx.org()).bind(ids).fetch_one(&mut *tx).await?;
        if usize::try_from(reserved).ok() != Some(fresh.len()) {
            return Err(StorageError::Conflict);
        }
        if !inserted.is_empty() {
            record_on(
                &mut tx,
                ctx,
                AuditEntry {
                    action: AuditAction::OrgAdminDirectoryUpdate,
                    object_type: "directory_operation",
                    object_id: &operation.to_string(),
                    decision: Decision::Allow,
                    trace_id: trace.as_deref(),
                    metadata: serde_json::json!({"reserved_roles":inserted.len()}),
                },
                Chain::Yes,
            )
            .await?;
        }
        tx.commit().await?;
        Ok(())
    }
}
