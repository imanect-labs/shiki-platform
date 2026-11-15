use super::{DirectoryAdminStore, ManagedRole};
use crate::{db_tenant::begin_tenant_tx, error::StorageError};
use authz::AuthContext;

#[derive(sqlx::FromRow)]
struct RolePath {
    id: String,
    ids: Vec<String>,
    labels: Vec<String>,
    child_count: i64,
}

impl DirectoryAdminStore {
    /// 一ページ分の祖先と子数だけを一往復で取得する。
    pub(super) async fn enrich(
        &self,
        ctx: &AuthContext,
        roles: &mut [ManagedRole],
    ) -> Result<(), StorageError> {
        if roles.is_empty() {
            return Ok(());
        }
        let ids: Vec<_> = roles.iter().map(|r| r.id.clone()).collect();
        let mut tx = begin_tenant_tx(&self.db, ctx.tenant()).await?;
        let paths: Vec<RolePath> = sqlx::query_as(
            "WITH RECURSIVE paths AS ( \
               SELECT d.role_id AS origin, d.role_id AS id, coalesce(m.label, d.display_name) AS label, m.parent_id, \
                      0 AS depth, ARRAY[d.role_id] AS visited \
               FROM directory_role d LEFT JOIN directory_managed_role m \
                 ON m.tenant_id = d.tenant_id AND m.org = d.org AND m.role_id = d.role_id \
               WHERE d.tenant_id = $1 AND d.org = $2 AND d.role_id = ANY($3) \
               UNION ALL SELECT p.origin, m.role_id, m.label, m.parent_id, p.depth + 1, p.visited || m.role_id \
               FROM paths p JOIN directory_managed_role m ON m.role_id = p.parent_id \
               WHERE m.tenant_id = $1 AND m.org = $2 AND p.depth < 128 AND NOT m.role_id = ANY(p.visited) \
             ) SELECT origin AS id, array_agg(id ORDER BY depth DESC) AS ids, array_agg(label ORDER BY depth DESC) AS labels, \
               (SELECT count(*) FROM directory_managed_role c WHERE c.tenant_id = $1 AND c.org = $2 AND c.parent_id = paths.origin) AS child_count \
             FROM paths GROUP BY origin",
        ).bind(ctx.tenant_id()).bind(ctx.org()).bind(ids).fetch_all(&mut *tx).await?;
        tx.commit().await?;
        let by_id: std::collections::HashMap<_, _> =
            paths.into_iter().map(|p| (p.id.clone(), p)).collect();
        for role in roles {
            if let Some(path) = by_id.get(&role.id) {
                role.path_ids.clone_from(&path.ids);
                role.path_labels.clone_from(&path.labels);
                role.child_count = path.child_count;
            }
        }
        Ok(())
    }

    /// 選択済み候補は明示IDを最大100件で取得する。他組織のIDを返さない。
    pub async fn roles_by_ids(
        &self,
        ctx: &AuthContext,
        ids: &[String],
    ) -> Result<Vec<ManagedRole>, StorageError> {
        if ids.len() > 100 {
            return Err(StorageError::Invalid(
                "一度に取得できるロールは100件までです".into(),
            ));
        }
        let mut tx = begin_tenant_tx(&self.db, ctx.tenant()).await?;
        let mut roles = sqlx::query_as(
            "SELECT d.role_id AS id, coalesce(m.label, d.display_name) AS label, m.parent_id, coalesce(m.version, 0) AS version \
             FROM directory_role d LEFT JOIN directory_managed_role m ON m.tenant_id = d.tenant_id AND m.org = d.org AND m.role_id = d.role_id \
             WHERE d.tenant_id = $1 AND d.org = $2 AND (m.role_id IS NULL OR m.version > 0) AND d.role_id = ANY($3) ORDER BY d.role_id",
        ).bind(ctx.tenant_id()).bind(ctx.org()).bind(ids).fetch_all(&mut *tx).await?;
        tx.commit().await?;
        self.enrich(ctx, &mut roles).await?;
        Ok(roles)
    }
}

impl DirectoryAdminStore {
    /// 自動作成の候補IDだけをまとめて照合する。ブラウザ一覧からは呼ばない。
    pub async fn role_definitions(
        &self,
        ctx: &AuthContext,
        ids: &[String],
    ) -> Result<Vec<ManagedRole>, StorageError> {
        if ids.len() > 20_000 {
            return Err(StorageError::Invalid("照合するロールが多すぎます".into()));
        }
        let mut tx = begin_tenant_tx(&self.db, ctx.tenant()).await?;
        let roles = sqlx::query_as("SELECT d.role_id AS id, coalesce(m.label, d.display_name) AS label, m.parent_id, coalesce(m.version, 0) AS version FROM directory_role d LEFT JOIN directory_managed_role m ON m.tenant_id = d.tenant_id AND m.org = d.org AND m.role_id = d.role_id WHERE d.tenant_id = $1 AND d.org = $2 AND d.role_id = ANY($3)").bind(ctx.tenant_id()).bind(ctx.org()).bind(ids).fetch_all(&mut *tx).await?;
        tx.commit().await?;
        Ok(roles)
    }
}

impl DirectoryAdminStore {
    pub async fn roles_for_preview(
        &self,
        ctx: &AuthContext,
        ids: &[String],
    ) -> Result<Vec<ManagedRole>, StorageError> {
        let mut roles = self.role_definitions(ctx, ids).await?;
        self.enrich(ctx, &mut roles).await?;
        Ok(roles)
    }
}
