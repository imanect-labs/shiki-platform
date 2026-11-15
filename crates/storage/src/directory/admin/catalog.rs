use super::{AdGroup, DirectoryAdminStore, GroupRoleMapping, ManagedRole, RoleEdit};
use crate::{db_tenant::begin_tenant_tx, error::StorageError};
use authz::AuthContext;

impl DirectoryAdminStore {
    pub async fn role(
        &self,
        ctx: &AuthContext,
        id: &str,
    ) -> Result<Option<ManagedRole>, StorageError> {
        let mut tx = begin_tenant_tx(&self.db, ctx.tenant()).await?;
        let role = sqlx::query_as(
            "SELECT d.role_id AS id, coalesce(m.label, d.display_name) AS label, m.parent_id, \
             coalesce(m.version, 0) AS version FROM directory_role d LEFT JOIN directory_managed_role m \
             ON m.tenant_id = d.tenant_id AND m.org = d.org AND m.role_id = d.role_id \
             WHERE d.tenant_id = $1 AND d.org = $2 AND d.role_id = $3",
        ).bind(ctx.tenant_id()).bind(ctx.org()).bind(id).fetch_optional(&mut *tx).await?;
        tx.commit().await?;
        Ok(role)
    }

    pub async fn group(
        &self,
        ctx: &AuthContext,
        id: &str,
    ) -> Result<Option<AdGroup>, StorageError> {
        let mut tx = begin_tenant_tx(&self.db, ctx.tenant()).await?;
        let group = sqlx::query_as(
            "SELECT group_id AS id, name, claim_path, distinguished_name, mapping_version, ou_path, parent_ids FROM directory_ad_group \
             WHERE tenant_id = $1 AND org = $2 AND group_id = $3 AND active",
        )
        .bind(ctx.tenant_id())
        .bind(ctx.org())
        .bind(id)
        .fetch_optional(&mut *tx)
        .await?;
        tx.commit().await?;
        Ok(group)
    }

    /// 外部のグループをキャッシュする。呼出側はKeycloakのテナント修飾rootで検証する。
    pub async fn cache_group(
        &self,
        ctx: &AuthContext,
        group: &AdGroup,
    ) -> Result<(), StorageError> {
        let expected = format!("/tenants/{}/{}/", ctx.tenant_id(), ctx.org());
        if !group.claim_path.starts_with(&expected) {
            return Err(StorageError::Forbidden);
        }
        let mut tx = begin_tenant_tx(&self.db, ctx.tenant()).await?;
        sqlx::query(
            "INSERT INTO directory_ad_group (tenant_id, org, group_id, name, claim_path, distinguished_name, ou_path, parent_ids) \
             VALUES ($1, $2, $3, $4, $5, $6, $7, $8) ON CONFLICT (tenant_id, org, group_id) DO UPDATE \
             SET mapping_version = directory_ad_group.mapping_version + CASE WHEN (directory_ad_group.name, directory_ad_group.claim_path, directory_ad_group.distinguished_name, directory_ad_group.ou_path, directory_ad_group.parent_ids, directory_ad_group.active) IS DISTINCT FROM (excluded.name, excluded.claim_path, excluded.distinguished_name, excluded.ou_path, excluded.parent_ids, true) THEN 1 ELSE 0 END, name = excluded.name, claim_path = excluded.claim_path, distinguished_name = excluded.distinguished_name, active = true, ou_path = excluded.ou_path, \
                 parent_ids = excluded.parent_ids, updated_at = now()",
        ).bind(ctx.tenant_id()).bind(ctx.org()).bind(&group.id).bind(&group.name)
            .bind(&group.claim_path).bind(&group.distinguished_name).bind(&group.ou_path).bind(&group.parent_ids)
            .execute(&mut *tx).await?;
        tx.commit().await?;
        Ok(())
    }

    pub async fn mapping(
        &self,
        ctx: &AuthContext,
        group_id: &str,
    ) -> Result<GroupRoleMapping, StorageError> {
        let mut tx = begin_tenant_tx(&self.db, ctx.tenant()).await?;
        let version = sqlx::query_scalar("SELECT mapping_version FROM directory_ad_group WHERE tenant_id = $1 AND org = $2 AND group_id = $3").bind(ctx.tenant_id()).bind(ctx.org()).bind(group_id).fetch_optional(&mut *tx).await?.ok_or(StorageError::NotFound)?;
        let role_ids = sqlx::query_scalar(
            "SELECT role_id FROM directory_ad_mapping WHERE tenant_id = $1 AND org = $2 AND group_id = $3 ORDER BY role_id",
        ).bind(ctx.tenant_id()).bind(ctx.org()).bind(group_id).fetch_all(&mut *tx).await?;
        tx.commit().await?;
        Ok(GroupRoleMapping {
            expected_version: version,
            group_id: group_id.into(),
            role_ids,
        })
    }

    /// ロールの表示射影。認可タプルの適用後にだけ確定する。
    pub async fn save_role(
        &self,
        ctx: &AuthContext,
        edit: &RoleEdit,
        keycloak_id: &str,
        operation: uuid::Uuid,
        step: i32,
    ) -> Result<(), StorageError> {
        let mut tx = begin_tenant_tx(&self.db, ctx.tenant()).await?;
        // 現在の版をロックし、他の組織の同じロールIDは更新しない。
        let exists: Option<String> = sqlx::query_scalar(
            "SELECT org FROM directory_role WHERE tenant_id = $1 AND role_id = $2 FOR UPDATE",
        )
        .bind(ctx.tenant_id())
        .bind(&edit.id)
        .fetch_optional(&mut *tx)
        .await?;
        if exists.as_deref().is_some_and(|org| org != ctx.org()) {
            return Err(StorageError::NotFound);
        }
        sqlx::query(
            "INSERT INTO directory_role (tenant_id, org, role_id, display_name) VALUES ($1, $2, $3, $4) \
             ON CONFLICT (tenant_id, role_id) DO UPDATE SET display_name = excluded.display_name, updated_at = now() \
             WHERE directory_role.org = excluded.org",
        ).bind(ctx.tenant_id()).bind(ctx.org()).bind(&edit.id).bind(&edit.label).execute(&mut *tx).await?;
        let current: Option<i64> = sqlx::query_scalar(
            "SELECT version FROM directory_managed_role WHERE tenant_id = $1 AND org = $2 AND role_id = $3 FOR UPDATE",
        ).bind(ctx.tenant_id()).bind(ctx.org()).bind(&edit.id).fetch_optional(&mut *tx).await?;
        if current != Some(edit.expected_version) {
            return Err(StorageError::Conflict);
        }
        sqlx::query(
            "INSERT INTO directory_managed_role (tenant_id, org, role_id, label, parent_id, keycloak_group_id, version) \
             VALUES ($1, $2, $3, $4, $5, $6, $7) ON CONFLICT (tenant_id, org, role_id) DO UPDATE \
             SET label = excluded.label, parent_id = excluded.parent_id, keycloak_group_id = excluded.keycloak_group_id, \
                 version = excluded.version, updated_at = now()",
        ).bind(ctx.tenant_id()).bind(ctx.org()).bind(&edit.id).bind(&edit.label)
            .bind(&edit.parent_id).bind(keycloak_id).bind(edit.expected_version + 1)
            .execute(&mut *tx).await?;
        super::operations::advance(&mut tx, ctx, operation, step, "role", &edit.id).await?;
        tx.commit().await?;
        Ok(())
    }
}
