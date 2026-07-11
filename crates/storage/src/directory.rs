//! ユーザーディレクトリ（共有ダイアログの相手検索）。Task 1.10 / #20。
//!
//! 設計上の不変条件:
//! - **テナント分離の pre-filter**: 検索は呼び出し元 [`AuthContext`] の `tenant_id`（＋ `org`）で
//!   必ず絞る。別テナントのユーザーは結果に出さない（SaaS 隔離境界＝`tenant_id`）。
//! - **全件取得の禁止**: keyset カーソル（`email, user_id`）＋ limit クランプで無限スクロール。
//! - これは検索可能なプロフィール（email / 表示名）の最小射影。正本のユーザー provisioning
//!   （Keycloak 同期・部署/ロール）は SAAS.2 / #76。dev では `dev_seed` が投入する。

use authz::AuthContext;
use sqlx::PgPool;

use crate::error::StorageError;

/// 1 ページの既定件数（呼び出し側が未指定のとき）。
pub const DEFAULT_SEARCH_LIMIT: usize = 20;
/// 1 ページの最大件数（全件取得を防ぐ上限）。
const MAX_SEARCH_LIMIT: usize = 50;

/// 検索結果の 1 ユーザー（共有相手候補）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DirectoryUser {
    /// OIDC `sub`（共有 tuple の `user:<id>` に使う）。
    pub id: String,
    pub email: String,
    pub display_name: String,
}

/// 検索の 1 ページ（テナント＋org スコープ済み）。
#[derive(Debug)]
pub struct DirectoryPage {
    pub items: Vec<DirectoryUser>,
    /// 続きがあれば次回 `cursor` に渡す値（末尾なら `None`）。
    pub next_cursor: Option<String>,
}

#[derive(sqlx::FromRow)]
struct DirectoryRow {
    user_id: String,
    email: String,
    display_name: String,
}

/// 検索結果の 1 ロール/部署（共有相手候補）。#76。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DirectoryRole {
    /// role/group id（共有 tuple の `role:<tenant>|<id>#member` に使う）。
    pub id: String,
    pub display_name: String,
}

/// ロール検索の 1 ページ（テナント＋org スコープ済み）。
#[derive(Debug)]
pub struct DirectoryRolePage {
    pub items: Vec<DirectoryRole>,
    pub next_cursor: Option<String>,
}

#[derive(sqlx::FromRow)]
struct DirectoryRoleRow {
    role_id: String,
    display_name: String,
}

/// ユーザーディレクトリのリポジトリ（Postgres backing）。
pub struct DirectoryStore {
    db: PgPool,
}

impl DirectoryStore {
    pub fn new(db: PgPool) -> Self {
        Self { db }
    }

    /// dev seed 用の冪等 upsert（テナント＋ユーザー単位）。
    pub async fn upsert_user(
        &self,
        user_id: &str,
        tenant_id: &str,
        org: &str,
        email: &str,
        display_name: &str,
    ) -> Result<(), StorageError> {
        sqlx::query(
            "INSERT INTO directory_user (user_id, tenant_id, org, email, display_name) \
             VALUES ($1, $2, $3, $4, $5) \
             ON CONFLICT (tenant_id, user_id) DO UPDATE \
               SET org = excluded.org, email = excluded.email, \
                   display_name = excluded.display_name, updated_at = now()",
        )
        .bind(user_id)
        .bind(tenant_id)
        .bind(org)
        .bind(email)
        .bind(display_name)
        .execute(&self.db)
        .await
        .map_err(StorageError::Db)?;
        Ok(())
    }

    /// テナント（＋ org）スコープのユーザー検索。自分自身は除外。
    ///
    /// `query` は email / 表示名の部分一致（ILIKE）。空文字は同テナントの先頭ページを返す
    /// （初期表示の候補一覧に使える）。keyset `(email, user_id)` 昇順でページングする。
    pub async fn search(
        &self,
        ctx: &AuthContext,
        query: &str,
        cursor: Option<&str>,
        limit: usize,
    ) -> Result<DirectoryPage, StorageError> {
        let limit = limit.clamp(1, MAX_SEARCH_LIMIT);
        let pattern = format!("%{}%", escape_like(query.trim()));
        let (after_email, after_id) = match cursor {
            Some(c) => {
                let (email, id) = decode_cursor(c)?;
                (Some(email), Some(id))
            }
            None => (None, None),
        };

        // limit+1 件引いて「続きがあるか」を判定する（余分な COUNT を避ける）。
        let rows: Vec<DirectoryRow> = sqlx::query_as(
            "SELECT user_id, email, display_name FROM directory_user \
             WHERE tenant_id = $1 AND org = $2 AND user_id <> $3 \
               AND (email ILIKE $4 ESCAPE '\\' OR display_name ILIKE $4 ESCAPE '\\') \
               AND ($5::text IS NULL OR (email, user_id) > ($5, $6)) \
             ORDER BY email, user_id LIMIT $7",
        )
        .bind(&ctx.tenant_id)
        .bind(&ctx.org)
        .bind(&ctx.principal.id)
        .bind(&pattern)
        .bind(after_email.as_deref())
        .bind(after_id.as_deref())
        .bind(limit as i64 + 1)
        .fetch_all(&self.db)
        .await
        .map_err(StorageError::Db)?;

        let has_more = rows.len() > limit;
        let items: Vec<DirectoryUser> = rows
            .into_iter()
            .take(limit)
            .map(|r| DirectoryUser {
                id: r.user_id,
                email: r.email,
                display_name: r.display_name,
            })
            .collect();
        let next_cursor = if has_more {
            items.last().map(|u| encode_cursor(&u.email, &u.id))
        } else {
            None
        };
        Ok(DirectoryPage { items, next_cursor })
    }

    /// ユーザー id 群 → 表示名の一括解決（更新者/作成者/版 author の表出用・Task 11P.10）。
    ///
    /// テナント（＋ org）で必ず絞る（別テナントの表示名を漏らさない）。ディレクトリに
    /// 無い id（AI エージェント主体など）は結果に含めない＝呼び出し側でフォールバックする。
    /// N+1 を避けるため `= ANY($ids)` の 1 クエリで引く。
    pub async fn resolve_display_names(
        &self,
        ctx: &AuthContext,
        ids: &[String],
    ) -> Result<std::collections::HashMap<String, String>, StorageError> {
        if ids.is_empty() {
            return Ok(std::collections::HashMap::new());
        }
        let rows: Vec<(String, String)> = sqlx::query_as(
            "SELECT user_id, display_name FROM directory_user \
             WHERE tenant_id = $1 AND org = $2 AND user_id = ANY($3)",
        )
        .bind(&ctx.tenant_id)
        .bind(&ctx.org)
        .bind(ids)
        .fetch_all(&self.db)
        .await
        .map_err(StorageError::Db)?;
        Ok(rows.into_iter().collect())
    }

    /// ユーザーがテナント（＋ org）内に存在するか（Task 9.2 の user 参照整合検証）。
    pub async fn user_exists(
        &self,
        ctx: &AuthContext,
        user_id: &str,
    ) -> Result<bool, StorageError> {
        let exists: bool = sqlx::query_scalar(
            "SELECT EXISTS(SELECT 1 FROM directory_user \
             WHERE tenant_id = $1 AND org = $2 AND user_id = $3)",
        )
        .bind(&ctx.tenant_id)
        .bind(&ctx.org)
        .bind(user_id)
        .fetch_one(&self.db)
        .await
        .map_err(StorageError::Db)?;
        Ok(exists)
    }

    /// ロール/部署がテナント（＋ org）内に存在するか（Task 9.2 の role 参照整合検証）。
    pub async fn role_exists(
        &self,
        ctx: &AuthContext,
        role_id: &str,
    ) -> Result<bool, StorageError> {
        let exists: bool = sqlx::query_scalar(
            "SELECT EXISTS(SELECT 1 FROM directory_role \
             WHERE tenant_id = $1 AND org = $2 AND role_id = $3)",
        )
        .bind(&ctx.tenant_id)
        .bind(&ctx.org)
        .bind(role_id)
        .fetch_one(&self.db)
        .await
        .map_err(StorageError::Db)?;
        Ok(exists)
    }

    /// role/部署の冪等 upsert（テナント＋role 単位）。ログイン時 claim 同期・dev_seed 用。
    pub async fn upsert_role(
        &self,
        role_id: &str,
        tenant_id: &str,
        org: &str,
        display_name: &str,
    ) -> Result<(), StorageError> {
        sqlx::query(
            "INSERT INTO directory_role (role_id, tenant_id, org, display_name) \
             VALUES ($1, $2, $3, $4) \
             ON CONFLICT (tenant_id, role_id) DO UPDATE \
               SET org = excluded.org, display_name = excluded.display_name, updated_at = now()",
        )
        .bind(role_id)
        .bind(tenant_id)
        .bind(org)
        .bind(display_name)
        .execute(&self.db)
        .await
        .map_err(StorageError::Db)?;
        Ok(())
    }

    /// テナント（＋ org）スコープのロール/部署検索（共有ダイアログのオートコンプリート）。
    ///
    /// `query` は role_id / 表示名の部分一致（ILIKE）。空文字は先頭ページを返す。
    /// keyset `(display_name, role_id)` 昇順でページングする。
    pub async fn search_roles(
        &self,
        ctx: &AuthContext,
        query: &str,
        cursor: Option<&str>,
        limit: usize,
    ) -> Result<DirectoryRolePage, StorageError> {
        let limit = limit.clamp(1, MAX_SEARCH_LIMIT);
        let pattern = format!("%{}%", escape_like(query.trim()));
        let (after_name, after_id) = match cursor {
            Some(c) => {
                let (name, id) = decode_cursor(c)?;
                (Some(name), Some(id))
            }
            None => (None, None),
        };

        let rows: Vec<DirectoryRoleRow> = sqlx::query_as(
            "SELECT role_id, display_name FROM directory_role \
             WHERE tenant_id = $1 AND org = $2 \
               AND (role_id ILIKE $3 ESCAPE '\\' OR display_name ILIKE $3 ESCAPE '\\') \
               AND ($4::text IS NULL OR (display_name, role_id) > ($4, $5)) \
             ORDER BY display_name, role_id LIMIT $6",
        )
        .bind(&ctx.tenant_id)
        .bind(&ctx.org)
        .bind(&pattern)
        .bind(after_name.as_deref())
        .bind(after_id.as_deref())
        .bind(limit as i64 + 1)
        .fetch_all(&self.db)
        .await
        .map_err(StorageError::Db)?;

        let has_more = rows.len() > limit;
        let items: Vec<DirectoryRole> = rows
            .into_iter()
            .take(limit)
            .map(|r| DirectoryRole {
                id: r.role_id,
                display_name: r.display_name,
            })
            .collect();
        let next_cursor = if has_more {
            items.last().map(|r| encode_cursor(&r.display_name, &r.id))
        } else {
            None
        };
        Ok(DirectoryRolePage { items, next_cursor })
    }
}

/// ILIKE のワイルドカード（`%` `_`）とエスケープ文字（`\`）を無害化する。
/// `ESCAPE '\'` と併用し、ユーザー入力がパターンメタ文字として効かないようにする。
pub(crate) fn escape_like(input: &str) -> String {
    let mut out = String::with_capacity(input.len());
    for ch in input.chars() {
        if matches!(ch, '\\' | '%' | '_') {
            out.push('\\');
        }
        out.push(ch);
    }
    out
}

/// keyset カーソルを `(email, user_id)` から組み立てる。
/// email / user_id は改行を含まないため `\n` 区切りで一意に復元できる。
fn encode_cursor(email: &str, user_id: &str) -> String {
    hex::encode(format!("{email}\n{user_id}").as_bytes())
}

/// [`encode_cursor`] の逆。壊れたカーソルは `Invalid`（panic しない）。
fn decode_cursor(cursor: &str) -> Result<(String, String), StorageError> {
    let invalid = || StorageError::Invalid("カーソルが不正です".into());
    let bytes = hex::decode(cursor).map_err(|_| invalid())?;
    let text = String::from_utf8(bytes).map_err(|_| invalid())?;
    let (email, user_id) = text.split_once('\n').ok_or_else(invalid)?;
    Ok((email.to_string(), user_id.to_string()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cursor_round_trips() {
        let c = encode_cursor(
            "alice@a-corp.example.com",
            "00000000-0000-0000-0000-000000000001",
        );
        let (email, id) = decode_cursor(&c).unwrap();
        assert_eq!(email, "alice@a-corp.example.com");
        assert_eq!(id, "00000000-0000-0000-0000-000000000001");
    }

    #[test]
    fn cursor_rejects_garbage() {
        assert!(decode_cursor("zzzz").is_err());
        assert!(decode_cursor(&hex::encode("no-newline")).is_err());
    }

    #[test]
    fn escape_like_neutralizes_wildcards() {
        assert_eq!(escape_like("a%b_c\\d"), "a\\%b\\_c\\\\d");
        assert_eq!(escape_like("plain"), "plain");
    }
}
