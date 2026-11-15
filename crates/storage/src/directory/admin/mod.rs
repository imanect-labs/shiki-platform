//! 組織管理画面の永続化。IdP所属の正本はclaims、管理ロールの所属は対応表から導く。
use authz::AuthContext;
use serde::{Deserialize, Serialize};
use sqlx::PgPool;
use utoipa::ToSchema;

use crate::{db_tenant::begin_tenant_tx, error::StorageError};

mod catalog;
mod configuration;
mod groups;
mod lease;
mod members;
mod paths;
mod validate;
pub use lease::{DirectoryLease, PendingDirectoryOperation};
mod operations;
mod reservations;
pub use operations::request_hash;
mod types;
pub use types::*;

/// 管理ロールの安定IDと編集可能な表示・階層。
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema, sqlx::FromRow)]
pub struct ManagedRole {
    pub id: String,
    pub label: String,
    pub parent_id: Option<String>,
    pub version: i64,
    #[sqlx(default)]
    pub path_ids: Vec<String>,
    #[sqlx(default)]
    pub path_labels: Vec<String>,
    #[sqlx(default)]
    pub child_count: i64,
}

/// 一ページ分だけ返す。カーソルは最後のID。
#[derive(Debug, Serialize, ToSchema)]
pub struct ManagedRolePage {
    pub items: Vec<ManagedRole>,
    pub next_cursor: Option<String>,
    pub total: i64,
}

/// 認証済み組織の管理ストア。生のpoolをAPIへ渡さない。
#[derive(Clone)]
pub struct DirectoryAdminStore {
    pub(super) db: PgPool,
}

impl super::DirectoryStore {
    #[must_use]
    pub fn admin(&self) -> DirectoryAdminStore {
        DirectoryAdminStore {
            db: self.db.clone(),
        }
    }
}

impl DirectoryAdminStore {
    /// queryまたは親の直下でDB側検索する。最大100件。
    pub async fn roles(
        &self,
        ctx: &AuthContext,
        query: &str,
        parent: Option<&str>,
        cursor: Option<&str>,
        limit: usize,
    ) -> Result<ManagedRolePage, StorageError> {
        let limit = limit.clamp(1, 100);
        let pattern = format!("%{}%", super::escape_like(query.trim()));
        let mut tx = begin_tenant_tx(&self.db, ctx.tenant()).await?;
        let total: i64 = sqlx::query_scalar(
            "SELECT count(*) FROM directory_role d LEFT JOIN directory_managed_role m \
             ON m.tenant_id = d.tenant_id AND m.org = d.org AND m.role_id = d.role_id \
             WHERE d.tenant_id = $1 AND d.org = $2 AND (m.role_id IS NULL OR m.version > 0) AND (($3 <> '' AND (coalesce(m.label, d.display_name) ILIKE $4 ESCAPE '\\' OR d.role_id ILIKE $4 ESCAPE '\\')) OR ($3 = '' AND m.parent_id IS NOT DISTINCT FROM $5))",
        ).bind(ctx.tenant_id()).bind(ctx.org()).bind(query.trim()).bind(&pattern).bind(parent).fetch_one(&mut *tx).await?;
        let mut items: Vec<ManagedRole> = sqlx::query_as(
            "SELECT d.role_id AS id, coalesce(m.label, d.display_name) AS label, m.parent_id, coalesce(m.version, 0) AS version FROM directory_role d \
             LEFT JOIN directory_managed_role m ON m.tenant_id = d.tenant_id AND m.org = d.org AND m.role_id = d.role_id \
             WHERE d.tenant_id = $1 AND d.org = $2 AND (m.role_id IS NULL OR m.version > 0) \
             AND (($3 <> '' AND (coalesce(m.label, d.display_name) ILIKE $4 ESCAPE '\\' OR d.role_id ILIKE $4 ESCAPE '\\')) \
                  OR ($3 = '' AND m.parent_id IS NOT DISTINCT FROM $5)) \
             AND ($6::text IS NULL OR d.role_id > $6) ORDER BY d.role_id LIMIT $7",
        ).bind(ctx.tenant_id()).bind(ctx.org()).bind(query.trim()).bind(pattern)
            .bind(parent).bind(cursor).bind(i64::try_from(limit + 1).unwrap_or(101))
            .fetch_all(&mut *tx).await?;
        tx.commit().await?;
        let next_cursor = if items.len() > limit {
            items.truncate(limit);
            items.last().map(|role| role.id.clone())
        } else {
            None
        };
        self.enrich(ctx, &mut items).await?;
        Ok(ManagedRolePage {
            items,
            next_cursor,
            total,
        })
    }

    /// 検証済みgroups claimのうち、自組織に登録された対応だけを解決する。
    pub async fn mapped_roles(
        &self,
        ctx: &AuthContext,
        claim_paths: &[String],
    ) -> Result<Vec<String>, StorageError> {
        let mut tx = begin_tenant_tx(&self.db, ctx.tenant()).await?;
        let ids = sqlx::query_scalar(
            "SELECT DISTINCT m.role_id FROM directory_ad_mapping m \
             JOIN directory_ad_group g ON g.tenant_id = m.tenant_id AND g.org = m.org AND g.group_id = m.group_id \
             WHERE m.tenant_id = $1 AND m.org = $2 AND g.active AND g.claim_path = ANY($3) ORDER BY m.role_id",
        ).bind(ctx.tenant_id()).bind(ctx.org()).bind(claim_paths).fetch_all(&mut *tx).await?;
        tx.commit().await?;
        Ok(ids)
    }
}

/// ロールIDはtenant名前空間内なので、さらに組織ごとの新規ID領域を分ける。
#[must_use]
pub fn role_id_prefix(ctx: &AuthContext) -> String {
    use sha2::{Digest, Sha256};
    let hash = Sha256::digest(format!("{}\0{}", ctx.tenant_id(), ctx.org()).as_bytes());
    format!("managed-{}-", hex::encode(hash))
}

/// UI/CSVのID区切りと衝突しない、組織所有の管理ID。
pub fn is_managed_id(ctx: &AuthContext, id: &str) -> bool {
    id.strip_prefix(&role_id_prefix(ctx)).is_some_and(|suffix| {
        !suffix.is_empty()
            && suffix
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_')
    })
}
