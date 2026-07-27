//! テナントレジストリ（SAAS.2 / #87）。プロビジョニング/削除のライフサイクル正本。
//!
//! 行は tombstone 方式（物理削除しない）: `deleted` を残して tenant_id 再利用による
//! 名前空間衝突を防ぐ。状態遷移は active → deleting → deleted の一方向で、
//! 全操作は冪等（プロビジョニング/撤去の再実行で収束する）。

use chrono::{DateTime, Utc};
use sqlx::PgPool;

use crate::error::StorageError;

/// テナントのライフサイクル状態。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TenantStatus {
    Active,
    Deleting,
    Deleted,
}

impl TenantStatus {
    pub const fn as_str(self) -> &'static str {
        match self {
            TenantStatus::Active => "active",
            TenantStatus::Deleting => "deleting",
            TenantStatus::Deleted => "deleted",
        }
    }

    fn parse(s: &str) -> Option<Self> {
        match s {
            "active" => Some(TenantStatus::Active),
            "deleting" => Some(TenantStatus::Deleting),
            "deleted" => Some(TenantStatus::Deleted),
            _ => None,
        }
    }
}

/// テナント 1 件。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Tenant {
    pub tenant_id: String,
    pub org: String,
    pub display_name: String,
    pub status: TenantStatus,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(sqlx::FromRow)]
struct TenantRow {
    tenant_id: String,
    org: String,
    display_name: String,
    status: String,
    created_at: DateTime<Utc>,
    updated_at: DateTime<Utc>,
}

impl TryFrom<TenantRow> for Tenant {
    type Error = StorageError;

    fn try_from(r: TenantRow) -> Result<Self, StorageError> {
        // status は DB の CHECK 制約で閉じているため、ここに来るのはスキーマ乖離のみ。
        let status = TenantStatus::parse(&r.status).ok_or_else(|| {
            StorageError::Invalid(format!("tenant.status が不正です: {}", r.status))
        })?;
        Ok(Tenant {
            tenant_id: r.tenant_id,
            org: r.org,
            display_name: r.display_name,
            status,
            created_at: r.created_at,
            updated_at: r.updated_at,
        })
    }
}

/// テナントレジストリのリポジトリ（Postgres backing）。
pub struct TenantStore {
    db: PgPool,
}

impl TenantStore {
    pub fn new(db: PgPool) -> Self {
        Self { db }
    }

    /// テナントを active として登録する（冪等）。
    ///
    /// - tombstone（deleted）の再利用は**拒否**: 旧テナントの FGA タプル/オブジェクトの
    ///   残骸と新テナントが名前空間衝突するのを防ぐ（別 id を使ってもらう）。
    /// - **deleting の再活性も拒否**: 撤去処理中/失敗後に create で active へ戻すと、
    ///   進行中の purge と新規プロビジョニングが競合する（ライフサイクルは一方向）。
    pub async fn upsert_active(
        &self,
        tenant_id: &str,
        org: &str,
        display_name: &str,
    ) -> Result<Tenant, StorageError> {
        // deleted の tombstone・撤去中（deleting）は上書きしない（fail-closed・一方向遷移）。
        let existing = self.get(tenant_id).await?;
        if let Some(t) = &existing {
            match t.status {
                TenantStatus::Deleted => {
                    return Err(StorageError::Invalid(format!(
                        "tenant_id '{tenant_id}' は削除済み（tombstone）のため再利用できません"
                    )));
                }
                TenantStatus::Deleting => {
                    return Err(StorageError::Invalid(format!(
                        "tenant_id '{tenant_id}' は撤去処理中のため再活性できません（purge 完了を待つこと）"
                    )));
                }
                TenantStatus::Active => {}
            }
        }
        let row: TenantRow = sqlx::query_as(
            "INSERT INTO tenant (tenant_id, org, display_name, status) \
             VALUES ($1, $2, $3, 'active') \
             ON CONFLICT (tenant_id) DO UPDATE \
               SET org = excluded.org, display_name = excluded.display_name, \
                   status = 'active', updated_at = now() \
               WHERE tenant.status = 'active' \
             RETURNING tenant_id, org, display_name, status, created_at, updated_at",
        )
        .bind(tenant_id)
        .bind(org)
        .bind(display_name)
        .fetch_one(&self.db)
        .await?;
        row.try_into()
    }

    /// 撤去処理中へ遷移する（冪等: 既に deleting/deleted でも成功）。無ければ `None`。
    pub async fn mark_deleting(&self, tenant_id: &str) -> Result<Option<Tenant>, StorageError> {
        let row: Option<TenantRow> = sqlx::query_as(
            "UPDATE tenant SET status = CASE WHEN status = 'deleted' THEN status ELSE 'deleting' END, \
                    updated_at = now() \
             WHERE tenant_id = $1 \
             RETURNING tenant_id, org, display_name, status, created_at, updated_at",
        )
        .bind(tenant_id)
        .fetch_optional(&self.db)
        .await?;
        row.map(TryInto::try_into).transpose()
    }

    /// 撤去完了（tombstone）へ遷移する（冪等）。
    pub async fn mark_deleted(&self, tenant_id: &str) -> Result<(), StorageError> {
        sqlx::query(
            "UPDATE tenant SET status = 'deleted', updated_at = now() WHERE tenant_id = $1",
        )
        .bind(tenant_id)
        .execute(&self.db)
        .await?;
        Ok(())
    }

    /// org 管理者キャップ: 自律エージェントの全自動（bypass）承認モードの許可/禁止を設定する（#350）。
    ///
    /// `false` にすると当該テナントでは bypass を選べない（チャット API が明示エラーで弾き、
    /// 実行中の残存 bypass は承認必須へクランプされる）。戻り `false` = active なテナントが無い。
    pub async fn set_autonomous_bypass(
        &self,
        tenant_id: &str,
        allow: bool,
    ) -> Result<bool, StorageError> {
        let updated = sqlx::query(
            "UPDATE tenant SET allow_autonomous_bypass = $2, updated_at = now() \
             WHERE tenant_id = $1 AND status = 'active'",
        )
        .bind(tenant_id)
        .bind(allow)
        .execute(&self.db)
        .await?;
        Ok(updated.rows_affected() == 1)
    }

    /// 同じ org slug を使う**他の未削除テナント**が存在するか（Keycloak group の共有判定）。
    ///
    /// テナント削除時、org group を消すと同 org slug を使う他テナントの `groups` claim が
    /// 壊れるため、共有されている場合は group 削除をスキップする判断に使う。
    pub async fn org_shared_by_others(
        &self,
        org: &str,
        tenant_id: &str,
    ) -> Result<bool, StorageError> {
        let exists: bool = sqlx::query_scalar(
            "SELECT EXISTS(SELECT 1 FROM tenant \
             WHERE org = $1 AND tenant_id <> $2 AND status <> 'deleted')",
        )
        .bind(org)
        .bind(tenant_id)
        .fetch_one(&self.db)
        .await?;
        Ok(exists)
    }

    /// テナントを取得する（tombstone 含む。無ければ `None`）。
    pub async fn get(&self, tenant_id: &str) -> Result<Option<Tenant>, StorageError> {
        let row: Option<TenantRow> = sqlx::query_as(
            "SELECT tenant_id, org, display_name, status, created_at, updated_at \
             FROM tenant WHERE tenant_id = $1",
        )
        .bind(tenant_id)
        .fetch_optional(&self.db)
        .await?;
        row.map(TryInto::try_into).transpose()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn status_roundtrip() {
        for s in [
            TenantStatus::Active,
            TenantStatus::Deleting,
            TenantStatus::Deleted,
        ] {
            assert_eq!(TenantStatus::parse(s.as_str()), Some(s));
        }
        assert_eq!(TenantStatus::parse("bogus"), None);
    }
}
