use super::{AdGroup, AdGroupPage, DirectoryAdminStore, AD_GROUP_ROOT};
use crate::{db_tenant::begin_tenant_tx, error::StorageError};
use authz::AuthContext;

impl DirectoryAdminStore {
    pub async fn groups(
        &self,
        ctx: &AuthContext,
        query: &str,
        cursor: Option<&str>,
        limit: usize,
        ou: &[String],
    ) -> Result<AdGroupPage, StorageError> {
        let limit = limit.clamp(1, 100);
        let pattern = format!("%{}%", crate::directory::escape_like(query.trim()));
        let mut tx = begin_tenant_tx(&self.db, ctx.tenant()).await?;
        let total = sqlx::query_scalar("SELECT count(*) FROM directory_ad_group WHERE tenant_id = $1 AND org = $2 AND active AND (name ILIKE $3 ESCAPE '\\' OR array_to_string(ou_path, '/') ILIKE $3 ESCAPE '\\') AND ($4 <> '' OR ou_path = $5)")
            .bind(ctx.tenant_id()).bind(ctx.org()).bind(&pattern).bind(query.trim()).bind(ou).fetch_one(&mut *tx).await?;
        let mut items: Vec<AdGroup> = sqlx::query_as(
            "SELECT g.group_id AS id, g.name, g.claim_path, g.distinguished_name, g.ou_path, g.parent_ids, g.mapping_version, \
             ARRAY(SELECT m.role_id FROM directory_ad_mapping m WHERE m.tenant_id = $1 AND m.org = $2 AND m.group_id = g.group_id ORDER BY m.role_id) AS role_ids \
             FROM directory_ad_group g WHERE g.tenant_id = $1 AND g.org = $2 AND g.active \
             AND (g.name ILIKE $3 ESCAPE '\\' OR array_to_string(g.ou_path, '/') ILIKE $3 ESCAPE '\\') \
             AND ($6 <> '' OR g.ou_path = $7) AND ($4::text IS NULL OR g.group_id > $4) ORDER BY g.group_id LIMIT $5",
        ).bind(ctx.tenant_id()).bind(ctx.org()).bind(pattern).bind(cursor).bind(i64::try_from(limit + 1).unwrap_or(101)).bind(query.trim()).bind(ou).fetch_all(&mut *tx).await?;
        tx.commit().await?;
        let next_cursor = if items.len() > limit {
            items.truncate(limit);
            items.last().map(|g| g.id.clone())
        } else {
            None
        };
        Ok(AdGroupPage {
            items,
            next_cursor,
            total,
        })
    }

    /// サーバで自動作成計画を組むためのスナップショット。ブラウザへの全件応答には使わない。
    pub async fn groups_for_plan(
        &self,
        ctx: &AuthContext,
        ids: Option<&[String]>,
        ancestors: bool,
    ) -> Result<Vec<AdGroup>, StorageError> {
        let mut tx = begin_tenant_tx(&self.db, ctx.tenant()).await?;
        let oversized: bool = sqlx::query_scalar("SELECT EXISTS (SELECT 1 FROM directory_ad_mapping m JOIN directory_ad_group g ON g.tenant_id = $1 AND g.org = $2 AND g.group_id = m.group_id WHERE m.tenant_id = $1 AND m.org = $2 AND g.active AND ($3::text[] IS NULL OR m.group_id = ANY($3)) OFFSET 20000)").bind(ctx.tenant_id()).bind(ctx.org()).bind(ids).fetch_one(&mut *tx).await?;
        if oversized {
            return Err(StorageError::Invalid("対応表が多いため、対象グループを絞って実行してください（確認案のロールID合計は20,000件まで）".into()));
        }
        let groups = sqlx::query_as(
            "WITH RECURSIVE wanted AS (SELECT group_id, parent_ids FROM directory_ad_group WHERE tenant_id = $1 AND org = $2 AND active AND ($3::text[] IS NULL OR group_id = ANY($3)) \
             UNION SELECT p.group_id, p.parent_ids FROM directory_ad_group p JOIN wanted w ON p.group_id = ANY(w.parent_ids) WHERE p.tenant_id = $1 AND p.org = $2 AND p.active AND $4) \
             SELECT g.group_id AS id, g.name, g.claim_path, g.distinguished_name, g.ou_path, g.parent_ids, g.mapping_version, \
             CASE WHEN $3::text[] IS NULL OR g.group_id = ANY($3) THEN ARRAY(SELECT m.role_id FROM directory_ad_mapping m WHERE m.tenant_id = $1 AND m.org = $2 AND m.group_id = g.group_id ORDER BY m.role_id) ELSE '{}'::text[] END AS role_ids \
             FROM directory_ad_group g JOIN wanted w ON w.group_id = g.group_id WHERE g.tenant_id = $1 AND g.org = $2 AND g.active ORDER BY g.group_id LIMIT 10001",
        ).bind(ctx.tenant_id()).bind(ctx.org()).bind(ids).bind(ancestors).fetch_all(&mut *tx).await?;
        tx.commit().await?;
        Ok(groups)
    }

    pub async fn complete_group_snapshot(
        &self,
        ctx: &AuthContext,
        ids: &[String],
    ) -> Result<(), StorageError> {
        let mut tx = begin_tenant_tx(&self.db, ctx.tenant()).await?;
        sqlx::query("UPDATE directory_ad_group SET active = false WHERE tenant_id = $1 AND org = $2 AND active AND NOT(group_id = ANY($3))")
            .bind(ctx.tenant_id()).bind(ctx.org()).bind(ids).execute(&mut *tx).await?;
        tx.commit().await?;
        Ok(())
    }
}

impl DirectoryAdminStore {
    /// 取得失敗時に既存キャッシュを半分だけ更新しない。所属解決は常に完全なsnapshotを見る。
    pub async fn replace_groups(
        &self,
        ctx: &AuthContext,
        groups: &[AdGroup],
        operation: Option<uuid::Uuid>,
    ) -> Result<(), StorageError> {
        let prefix = format!(
            "/tenants/{}/{}/{AD_GROUP_ROOT}/",
            ctx.tenant_id(),
            ctx.org()
        );
        if groups.len() > 10_000 || groups.iter().any(|g| !g.claim_path.starts_with(&prefix)) {
            return Err(StorageError::Forbidden);
        }
        let mut tx = begin_tenant_tx(&self.db, ctx.tenant()).await?;
        let ids: Vec<_> = groups.iter().map(|g| &g.id).collect();
        let paths: Vec<_> = groups.iter().map(|g| &g.claim_path).collect();
        // 同じ名前で再作成された別IDへ、旧グループの対応を引き継がない。
        sqlx::query("DELETE FROM directory_ad_mapping m USING directory_ad_group g WHERE m.tenant_id = $1 AND m.org = $2 AND g.tenant_id = $1 AND g.org = $2 AND m.group_id = g.group_id AND NOT(g.group_id = ANY($3)) AND g.claim_path = ANY($4)").bind(ctx.tenant_id()).bind(ctx.org()).bind(&ids).bind(&paths).execute(&mut *tx).await?;
        sqlx::query("DELETE FROM directory_ad_group WHERE tenant_id = $1 AND org = $2 AND NOT(group_id = ANY($3)) AND claim_path = ANY($4)").bind(ctx.tenant_id()).bind(ctx.org()).bind(&ids).bind(&paths).execute(&mut *tx).await?;
        let snapshot =
            serde_json::to_value(groups).map_err(|e| StorageError::Invalid(e.to_string()))?;
        sqlx::query("INSERT INTO directory_ad_group (tenant_id, org, group_id, name, claim_path, distinguished_name, ou_path, parent_ids) \
            SELECT $1, $2, id, name, claim_path, distinguished_name, ou_path, parent_ids FROM jsonb_to_recordset($3) AS g(id text, name text, claim_path text, distinguished_name text, ou_path text[], parent_ids text[]) \
            ON CONFLICT (tenant_id, org, group_id) DO UPDATE SET name = excluded.name, claim_path = excluded.claim_path, distinguished_name = excluded.distinguished_name, ou_path = excluded.ou_path, parent_ids = excluded.parent_ids, active = true, updated_at = now(), \
            mapping_version = directory_ad_group.mapping_version + CASE WHEN (directory_ad_group.name, directory_ad_group.claim_path, directory_ad_group.distinguished_name, directory_ad_group.ou_path, directory_ad_group.parent_ids, directory_ad_group.active) IS DISTINCT FROM (excluded.name, excluded.claim_path, excluded.distinguished_name, excluded.ou_path, excluded.parent_ids, true) THEN 1 ELSE 0 END")
            .bind(ctx.tenant_id()).bind(ctx.org()).bind(snapshot).execute(&mut *tx).await?;
        sqlx::query("UPDATE directory_ad_group SET active = false, mapping_version = mapping_version + 1 WHERE tenant_id = $1 AND org = $2 AND active AND NOT(group_id = ANY($3))").bind(ctx.tenant_id()).bind(ctx.org()).bind(ids).execute(&mut *tx).await?;
        if let Some(id) = operation {
            super::operations::advance(&mut tx, ctx, id, 0, "directory_snapshot", "ad").await?;
        }
        tx.commit().await?;
        Ok(())
    }
}

impl DirectoryAdminStore {
    pub async fn folders(
        &self,
        ctx: &AuthContext,
        path: &[String],
        cursor: Option<&str>,
    ) -> Result<super::AdFolderPage, StorageError> {
        if path.len() > 128 {
            return Err(StorageError::Invalid("OU階層が深すぎます".into()));
        }
        let depth = i32::try_from(path.len())
            .map_err(|_| StorageError::Invalid("OU階層が不正です".into()))?;
        let mut tx = begin_tenant_tx(&self.db, ctx.tenant()).await?;
        let mut items: Vec<super::AdFolder> = sqlx::query_as("SELECT ou_path[$3 + 1] AS name, count(*) AS group_count FROM directory_ad_group WHERE tenant_id = $1 AND org = $2 AND active AND cardinality(ou_path) > $3 AND ($3 = 0 OR ou_path[1:$3] = $4) AND ($5::text IS NULL OR ou_path[$3 + 1] > $5) GROUP BY ou_path[$3 + 1] ORDER BY name LIMIT 101").bind(ctx.tenant_id()).bind(ctx.org()).bind(depth).bind(path).bind(cursor).fetch_all(&mut *tx).await?;
        tx.commit().await?;
        let next_cursor = if items.len() > 100 {
            items.truncate(100);
            items.last().map(|f| f.name.clone())
        } else {
            None
        };
        Ok(super::AdFolderPage { items, next_cursor })
    }
}
