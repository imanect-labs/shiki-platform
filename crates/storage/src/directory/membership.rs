//! 組織の member と部署の定期の照合（D-136）が読むディレクトリの行。
//!
//! 照合の相手は、ログインの同期が載せた人（`directory_user`）。テナントをまたいで（tenant_id, user_id）の順に
//! 少しずつ読む（主キーの索引の範囲読み・全件を一度に引かない）。部署は、ログインの同期が所属グループ由来と
//! 記録したロール（`directory_user_role.source = 'group'`）だけを照合の候補にする（属性 `roles` 由来のロールや
//! 手で書いたタプルを、所属グループに無いからと外さない）。

use std::collections::HashMap;

use super::DirectoryStore;
use crate::error::StorageError;

/// 1 回に読める人数の上限（照合の 1 ページ）。
const MAX_PAGE: usize = 1_000;

/// ディレクトリの 1 人（テナントと OIDC `sub`）。
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord, sqlx::FromRow)]
pub struct DirectoryEntry {
    pub tenant_id: String,
    pub user_id: String,
}

impl DirectoryStore {
    /// `after` より後ろの人を（tenant_id, user_id）の順に `limit` 人（1〜1,000 に収める）。
    pub async fn users_after(
        &self,
        after: Option<&DirectoryEntry>,
        limit: usize,
    ) -> Result<Vec<DirectoryEntry>, StorageError> {
        let limit = limit.clamp(1, MAX_PAGE);
        sqlx::query_as(
            "SELECT tenant_id, user_id FROM directory_user \
             WHERE ($1::text IS NULL OR (tenant_id, user_id) > ($1, $2)) \
             ORDER BY tenant_id, user_id LIMIT $3",
        )
        .bind(after.map(|e| e.tenant_id.as_str()))
        .bind(after.map(|e| e.user_id.as_str()))
        .bind(limit as i64)
        .fetch_all(&self.db)
        .await
        .map_err(StorageError::Db)
    }

    /// ログインの同期が所属グループ由来（部署）と記録したロールを、人ごとに（1 クエリ）。記録の無い人は入らない。
    pub async fn group_roles_of(
        &self,
        users: &[DirectoryEntry],
    ) -> Result<HashMap<DirectoryEntry, Vec<String>>, StorageError> {
        if users.is_empty() {
            return Ok(HashMap::new());
        }
        let tenants: Vec<&str> = users.iter().map(|u| u.tenant_id.as_str()).collect();
        let ids: Vec<&str> = users.iter().map(|u| u.user_id.as_str()).collect();
        let rows: Vec<(String, String, String)> = sqlx::query_as(
            "SELECT r.tenant_id, r.user_id, r.role_id FROM directory_user_role r \
             JOIN unnest($1::text[], $2::text[]) AS u(tenant_id, user_id) \
               ON r.tenant_id = u.tenant_id AND r.user_id = u.user_id \
             WHERE r.source = 'group' ORDER BY r.tenant_id, r.user_id, r.role_id",
        )
        .bind(&tenants)
        .bind(&ids)
        .fetch_all(&self.db)
        .await
        .map_err(StorageError::Db)?;
        let mut out: HashMap<DirectoryEntry, Vec<String>> = HashMap::new();
        for (tenant_id, user_id, role_id) in rows {
            out.entry(DirectoryEntry { tenant_id, user_id })
                .or_default()
                .push(role_id);
        }
        Ok(out)
    }

    /// 部署の写し（`directory_user_role`）から、照合で member を外したロールを消す（AI 活用分析の部署集計を合わせる）。
    pub async fn remove_user_roles(
        &self,
        tenant_id: &str,
        user_id: &str,
        role_ids: &[String],
    ) -> Result<u64, StorageError> {
        if role_ids.is_empty() {
            return Ok(0);
        }
        let done = sqlx::query(
            "DELETE FROM directory_user_role \
             WHERE tenant_id = $1 AND user_id = $2 AND role_id = ANY($3)",
        )
        .bind(tenant_id)
        .bind(user_id)
        .bind(role_ids)
        .execute(&self.db)
        .await
        .map_err(StorageError::Db)?;
        Ok(done.rows_affected())
    }
}
