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
    /// ワークフロー実行履歴の保持日数（日次 GC の起算・#448）。
    pub workflow_retention_days: i32,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(sqlx::FromRow)]
struct TenantRow {
    tenant_id: String,
    org: String,
    display_name: String,
    status: String,
    workflow_retention_days: i32,
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
            workflow_retention_days: r.workflow_retention_days,
            created_at: r.created_at,
            updated_at: r.updated_at,
        })
    }
}

/// ワークフロー実行履歴の保持日数の下限（1 日）。
pub const MIN_WORKFLOW_RETENTION_DAYS: i32 = 1;

/// ワークフロー実行履歴の保持日数の上限（10 年）。
///
/// 保持の意図ではなく**打ち間違いの検出**が目的。`90` のつもりで `9000` と入れると GC が事実上
/// 無効化され、`workflow_run` / `effect_journal` が無限に伸びる（それが #444 で潰した状態そのもの）。
pub const MAX_WORKFLOW_RETENTION_DAYS: i32 = 3650;

/// 保持日数の範囲を検証する（設定経路の入口で必ず通す）。
///
/// 下限が 1 日でよいのは、`effect_journal` の削除が **所有 run 行の不在**を条件にしているため
/// （`workflow-engine` の GC・#445）。run 行は terminal になるまで消えないので、保持日数を run の
/// 最大生存期間（`MAX_RUN_TIMEOUT_SEC` = 30 日）より短くしても、実行中 run の副作用記録が先に
/// 消えることはない。プライバシー要件で 7 日等に縮めるのは正当な選択なので下限で塞がない。
pub fn validate_workflow_retention_days(days: i32) -> Result<(), StorageError> {
    if (MIN_WORKFLOW_RETENTION_DAYS..=MAX_WORKFLOW_RETENTION_DAYS).contains(&days) {
        return Ok(());
    }
    Err(StorageError::Invalid(format!(
        "保持日数は {MIN_WORKFLOW_RETENTION_DAYS}〜{MAX_WORKFLOW_RETENTION_DAYS} 日で指定してください（指定値: {days}）"
    )))
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
             RETURNING tenant_id, org, display_name, status, workflow_retention_days, \
                       created_at, updated_at",
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
             RETURNING tenant_id, org, display_name, status, workflow_retention_days, \
                       created_at, updated_at",
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

    /// ワークフロー実行履歴の保持日数を設定する（#448）。戻り `false` = active なテナントが無い。
    ///
    /// 期限を過ぎた terminal run とその `step_execution` / `run_event` / `effect_journal` が
    /// 日次 GC で消える（`workflow-engine` の `HistoryGcWorker`）。
    ///
    /// **範囲検証はここで行う**（呼び出し側の約束にしない）。DB の CHECK は `> 0` しか見ないため、
    /// 検証を呼び出し側に委ねると 2 番目の呼び出し元が忘れた瞬間に `2_000_000_000` のような値が
    /// 入る。GC は `make_interval(days => ...)` を使うので、その値では `timestamp out of range` で
    /// クエリが落ち、テナントループを `?` で抜けて**全テナントの GC が止まる**。
    pub async fn set_workflow_retention_days(
        &self,
        tenant_id: &str,
        days: i32,
    ) -> Result<bool, StorageError> {
        validate_workflow_retention_days(days)?;
        let updated = sqlx::query(
            "UPDATE tenant SET workflow_retention_days = $2, updated_at = now() \
             WHERE tenant_id = $1 AND status = 'active'",
        )
        .bind(tenant_id)
        .bind(days)
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
            "SELECT tenant_id, org, display_name, status, workflow_retention_days, \
             created_at, updated_at \
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

    #[test]
    fn retention_days_range() {
        for ok in [1, 7, 90, 3650] {
            assert!(validate_workflow_retention_days(ok).is_ok(), "{ok} は許可");
        }
        for ng in [0, -1, 3651, i32::MAX] {
            assert!(validate_workflow_retention_days(ng).is_err(), "{ng} は拒否");
        }
    }
}
