use super::{DirectoryAdminStore, DirectoryMember, DirectoryMemberPage};
use crate::{db_tenant::begin_tenant_tx, error::StorageError};
use authz::AuthContext;
impl DirectoryAdminStore {
    pub async fn members(
        &self,
        ctx: &AuthContext,
        query: &str,
        cursor: Option<&str>,
    ) -> Result<DirectoryMemberPage, StorageError> {
        let mut tx = begin_tenant_tx(&self.db, ctx.tenant()).await?;
        let pattern = format!("%{}%", crate::directory::escape_like(query.trim()));
        let mut items: Vec<DirectoryMember> = sqlx::query_as("SELECT u.user_id AS id, u.email, u.display_name, \
            ARRAY(SELECT coalesce(d.display_name, r.role_id) FROM directory_user_role r LEFT JOIN directory_role d ON d.tenant_id = $1 AND d.org = $2 AND d.role_id = r.role_id WHERE r.tenant_id = $1 AND r.org = $2 AND r.user_id = u.user_id ORDER BY r.role_id LIMIT 20) AS role_names, \
            (SELECT count(*) FROM directory_user_role r WHERE r.tenant_id = $1 AND r.org = $2 AND r.user_id = u.user_id) AS role_count \
            FROM directory_user u WHERE u.tenant_id = $1 AND u.org = $2 AND (u.display_name ILIKE $3 ESCAPE '\\' OR u.email ILIKE $3 ESCAPE '\\') AND ($4::text IS NULL OR u.user_id > $4) ORDER BY u.user_id LIMIT 41").bind(ctx.tenant_id()).bind(ctx.org()).bind(pattern).bind(cursor).fetch_all(&mut *tx).await?;
        tx.commit().await?;
        let next_cursor = if items.len() > 40 {
            items.truncate(40);
            items.last().map(|m| m.id.clone())
        } else {
            None
        };
        Ok(DirectoryMemberPage { items, next_cursor })
    }
}
