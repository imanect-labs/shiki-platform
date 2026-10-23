//! 監査ログの保持期間（#831）。
//!
//! human の判断（2026-10-22）:
//! - 既定 365 日。運用者がテナントごとに 90〜3650 日で変えられる（`tenant.audit_retention_days`）。
//! - 期限を過ぎた行は日次で削除する。外部への退避はしない。
//! - 実データを変える操作の行（ハッシュチェーン・#828）は**先頭から連続して**消し、消した最後の
//!   行の id と entry_hash を [`audit_chain_anchor`] に残す。検証はその境目から始める。
//! - テナント撤去の記録（`tenant.purge`）は消さない。撤去後は tombstone に残った保持期間で消し続ける。
//!
//! 全レプリカで同時に走っても壊れないよう、1 バッチごとに tenant＋org のチェーンのロック
//! （記録側と同じ advisory lock）を取り、その中で消して境目を進める。
//!
//! [`audit_chain_anchor`]: crate::audit_chain

use std::sync::Arc;
use std::time::Duration;

use authz::{AuthContext, Principal, PrincipalKind};
use chrono::{DateTime, Utc};
use serde_json::json;
use sqlx::PgPool;

use crate::audit::{record_on, AuditAction, AuditEntry, Chain, Decision};
use crate::error::StorageError;

/// 既定の保持日数（1 年）。
pub const DEFAULT_RETENTION_DAYS: i32 = 365;
/// 保持日数の下限（短くしすぎて調査できなくなるのを防ぐ）。
pub const MIN_RETENTION_DAYS: i32 = 90;
/// 保持日数の上限（10 年）。
pub const MAX_RETENTION_DAYS: i32 = 3650;

/// 1 回の DELETE で消す行数（ロックと WAL を小さく刻む）。
const BATCH: i64 = 2000;

/// 削除の記録の actor（人ではなく日次の処理）。
const ACTOR: &str = "system:audit-retention";

/// 保持日数の範囲を検証する（設定経路の入口で必ず通す）。
pub fn validate_retention_days(days: i32) -> Result<(), StorageError> {
    if (MIN_RETENTION_DAYS..=MAX_RETENTION_DAYS).contains(&days) {
        return Ok(());
    }
    Err(StorageError::Invalid(format!(
        "監査ログの保持日数は {MIN_RETENTION_DAYS}〜{MAX_RETENTION_DAYS} 日で指定してください（指定値: {days}）"
    )))
}

/// 1 回の掃除の結果。
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct SweepReport {
    /// 消した連結行（ハッシュチェーン）の数。
    pub chained: u64,
    /// 消した連結しない行（読み取り・拒否など）の数。
    pub unchained: u64,
}

impl SweepReport {
    fn add(&mut self, o: SweepReport) {
        self.chained += o.chained;
        self.unchained += o.unchained;
    }

    fn is_empty(self) -> bool {
        self.chained == 0 && self.unchained == 0
    }
}

/// 監査ログの保持期間を適用する。
#[derive(Clone)]
pub struct AuditRetention {
    db: PgPool,
}

impl AuditRetention {
    pub fn new(db: PgPool) -> Self {
        AuditRetention { db }
    }

    /// テナントの保持日数（テナント台帳に無いテナントは既定値）。
    pub async fn retention_days(&self, tenant_id: &str) -> Result<i32, StorageError> {
        let days: Option<i32> =
            sqlx::query_scalar("SELECT audit_retention_days FROM tenant WHERE tenant_id = $1")
                .bind(tenant_id)
                .fetch_optional(&self.db)
                .await?;
        Ok(days.unwrap_or(DEFAULT_RETENTION_DAYS))
    }

    /// テナントの保持日数を設定し、変更を監査に残す（ハッシュチェーンに連結）。
    ///
    /// active なテナントのみ。戻り値は変更前の日数（テナントが無い・active でなければ `None`）。
    /// `actor` は監査の記録者（運用者 API なら `provisioner:<client>`、CLI なら `cli`）。
    pub async fn set_retention_days(
        &self,
        tenant_id: &str,
        days: i32,
        actor: &str,
        metadata: serde_json::Value,
    ) -> Result<Option<i32>, StorageError> {
        validate_retention_days(days)?;
        let mut tx = self.db.begin().await?;
        let Some((org, previous)): Option<(String, i32)> = sqlx::query_as(
            "SELECT org, audit_retention_days FROM tenant \
             WHERE tenant_id = $1 AND status = 'active' FOR UPDATE",
        )
        .bind(tenant_id)
        .fetch_optional(&mut *tx)
        .await?
        else {
            return Ok(None);
        };
        sqlx::query(
            "UPDATE tenant SET audit_retention_days = $2, updated_at = now() WHERE tenant_id = $1",
        )
        .bind(tenant_id)
        .bind(days)
        .execute(&mut *tx)
        .await?;
        let mut ctx = system_ctx(tenant_id, &org);
        ctx.principal.id = actor.to_string();
        let mut metadata = metadata;
        if let Some(m) = metadata.as_object_mut() {
            m.insert("retention_days".into(), json!(days));
            m.insert("previous".into(), json!(previous));
        }
        record_on(
            &mut tx,
            &ctx,
            AuditEntry {
                action: AuditAction::TenantAuditRetentionSet,
                object_type: "organization",
                object_id: &org,
                decision: Decision::Allow,
                trace_id: None,
                metadata,
            },
            Chain::Yes,
        )
        .await?;
        tx.commit().await?;
        Ok(Some(previous))
    }

    /// 全テナントに保持期間を適用する（`now` を起点に、各テナントの保持日数より古い行を消す）。
    pub async fn sweep(&self, now: DateTime<Utc>) -> Result<SweepReport, StorageError> {
        let mut total = SweepReport::default();
        for tenant_id in self.tenants().await? {
            let days = self.retention_days(&tenant_id).await?;
            let cutoff = now - chrono::Duration::days(i64::from(days));
            for org in self.orgs(&tenant_id).await? {
                let done = self.sweep_org(&tenant_id, &org, cutoff, days).await?;
                total.add(done);
            }
        }
        Ok(total)
    }

    /// 1 つの tenant＋org に保持期間を適用する。
    ///
    /// バッチごとに 1 つのトランザクションで、行の削除・境目の更新・削除の記録（チェーンに連結）を
    /// まとめて確定する。途中で落ちても「消したのに記録が無い」状態は残らない（残りは次回消す）。
    pub async fn sweep_org(
        &self,
        tenant_id: &str,
        org: &str,
        cutoff: DateTime<Utc>,
        days: i32,
    ) -> Result<SweepReport, StorageError> {
        let mut total = SweepReport::default();
        loop {
            let batch = self.sweep_batch(tenant_id, org, cutoff, days).await?;
            total.add(batch);
            if batch.chained < BATCH as u64 && batch.unchained < BATCH as u64 {
                return Ok(total);
            }
        }
    }

    /// 1 バッチ分を消し、境目と削除の記録を同じトランザクションで残す。
    async fn sweep_batch(
        &self,
        tenant_id: &str,
        org: &str,
        cutoff: DateTime<Utc>,
        days: i32,
    ) -> Result<SweepReport, StorageError> {
        let mut tx = self.db.begin().await?;
        // 記録側（record_on）と同じロック: チェーンの末尾の読み書き・境目の更新・削除の記録を直列にする。
        sqlx::query("SELECT pg_advisory_xact_lock(hashtext($1))")
            .bind(format!("{tenant_id}|{org}"))
            .execute(&mut *tx)
            .await?;

        // 連結行は **id 順で先頭から連続した期限切れの部分だけ**を消す。作成時刻はレプリカの時計
        // （版 1 はトランザクション開始時刻）で決まり id 順と逆転しうるので、「期限より古い最後の行」
        // までを消すと、間にある期限内の行まで消えてしまう。id 順で最初の「期限内の行」か撤去の記録
        // （tenant.purge は消さない）より手前で止める。
        let stop: Option<i64> = sqlx::query_scalar(
            "SELECT min(id) FROM audit_log \
             WHERE tenant_id = $1 AND org = $2 AND chained AND (created_at >= $3 OR action = $4)",
        )
        .bind(tenant_id)
        .bind(org)
        .bind(cutoff)
        .bind(AuditAction::TenantPurge.as_str())
        .fetch_one(&mut *tx)
        .await?;
        let chained: Vec<(i64, Option<String>)> = sqlx::query_as(
            "DELETE FROM audit_log WHERE id IN ( \
                 SELECT id FROM audit_log \
                 WHERE tenant_id = $1 AND org = $2 AND chained AND id < $3 \
                 ORDER BY id LIMIT $4) \
             RETURNING id, entry_hash",
        )
        .bind(tenant_id)
        .bind(org)
        .bind(stop.unwrap_or(i64::MAX))
        .bind(BATCH)
        .fetch_all(&mut *tx)
        .await?;
        // 連結しない行（読み取り・拒否など）は順序に依らないので、期限より古いものを消す。
        let unchained = sqlx::query(
            "DELETE FROM audit_log WHERE id IN ( \
                 SELECT id FROM audit_log \
                 WHERE tenant_id = $1 AND org = $2 AND created_at < $3 AND NOT chained \
                   AND action <> $4 \
                 ORDER BY created_at, id LIMIT $5)",
        )
        .bind(tenant_id)
        .bind(org)
        .bind(cutoff)
        .bind(AuditAction::TenantPurge.as_str())
        .bind(BATCH)
        .execute(&mut *tx)
        .await?
        .rows_affected();
        let report = SweepReport {
            chained: chained.len() as u64,
            unchained,
        };
        if report.is_empty() {
            tx.rollback().await?;
            return Ok(report);
        }

        // 境目を進め（消した最後の連結行）、消した行数を足す。
        let purged = i64::try_from(report.chained + report.unchained).unwrap_or(i64::MAX);
        let last = chained.iter().max_by_key(|(id, _)| *id).cloned();
        let anchor_id: Option<i64> = match last {
            Some((id, hash)) => sqlx::query_scalar(
                "INSERT INTO audit_chain_anchor (tenant_id, org, anchor_id, anchor_hash, purged_rows) \
                 VALUES ($1, $2, $3, $4, $5) \
                 ON CONFLICT (tenant_id, org) DO UPDATE \
                 SET anchor_id = greatest(audit_chain_anchor.anchor_id, excluded.anchor_id), \
                     anchor_hash = CASE WHEN excluded.anchor_id > audit_chain_anchor.anchor_id \
                                        THEN excluded.anchor_hash ELSE audit_chain_anchor.anchor_hash END, \
                     purged_rows = audit_chain_anchor.purged_rows + excluded.purged_rows, \
                     updated_at = now() \
                 RETURNING anchor_id",
            )
            .bind(tenant_id)
            .bind(org)
            .bind(id)
            .bind(hash.unwrap_or_default())
            .bind(purged)
            .fetch_optional(&mut *tx)
            .await?,
            None => sqlx::query_scalar(
                "UPDATE audit_chain_anchor SET purged_rows = purged_rows + $3, updated_at = now() \
                 WHERE tenant_id = $1 AND org = $2 RETURNING anchor_id",
            )
            .bind(tenant_id)
            .bind(org)
            .bind(purged)
            .fetch_optional(&mut *tx)
            .await?,
        };
        record_on(
            &mut tx,
            &system_ctx(tenant_id, org),
            AuditEntry {
                action: AuditAction::AuditRetentionPurge,
                object_type: "organization",
                object_id: org,
                decision: Decision::Allow,
                trace_id: None,
                metadata: json!({
                    "retention_days": days,
                    "cutoff": cutoff,
                    "deleted_chained": report.chained,
                    "deleted_unchained": report.unchained,
                    "anchor_id": anchor_id,
                }),
            },
            Chain::Yes,
        )
        .await?;
        tx.commit().await?;
        Ok(report)
    }

    /// 監査ログに行のあるテナント（索引の先頭列を飛び飛びに引く・全件走査しない）。
    async fn tenants(&self) -> Result<Vec<String>, StorageError> {
        Ok(sqlx::query_scalar(
            "WITH RECURSIVE t(tenant_id) AS ( \
                 (SELECT tenant_id FROM audit_log ORDER BY tenant_id LIMIT 1) \
                 UNION ALL \
                 SELECT (SELECT a.tenant_id FROM audit_log a WHERE a.tenant_id > t.tenant_id \
                         ORDER BY a.tenant_id LIMIT 1) \
                 FROM t WHERE t.tenant_id IS NOT NULL) \
             SELECT tenant_id FROM t WHERE tenant_id IS NOT NULL",
        )
        .fetch_all(&self.db)
        .await?)
    }

    /// テナント内で監査ログに行のある org（同じく飛び飛びに引く）。
    async fn orgs(&self, tenant_id: &str) -> Result<Vec<String>, StorageError> {
        Ok(sqlx::query_scalar(
            "WITH RECURSIVE o(org) AS ( \
                 (SELECT org FROM audit_log WHERE tenant_id = $1 ORDER BY org LIMIT 1) \
                 UNION ALL \
                 SELECT (SELECT a.org FROM audit_log a WHERE a.tenant_id = $1 AND a.org > o.org \
                         ORDER BY a.org LIMIT 1) \
                 FROM o WHERE o.org IS NOT NULL) \
             SELECT org FROM o WHERE org IS NOT NULL",
        )
        .bind(tenant_id)
        .fetch_all(&self.db)
        .await?)
    }
}

fn system_ctx(tenant_id: &str, org: &str) -> AuthContext {
    AuthContext::new(
        Principal {
            kind: PrincipalKind::User,
            id: ACTOR.into(),
            email: None,
            groups: vec![],
            roles: vec![],
            tenant_id: Some(tenant_id.into()),
        },
        org.into(),
        tenant_id.into(),
    )
}

/// 掃除の間隔の下限（設定ミスで休みなく回さない）。
const MIN_INTERVAL: Duration = Duration::from_mins(10);
/// 失敗時の待機。
const RETRY: Duration = Duration::from_mins(5);

/// 保持期間の掃除をバックグラウンドで回す（既定 1 日ごと・起動直後にも 1 回）。
pub fn spawn_audit_retention_timer(
    retention: Arc<AuditRetention>,
    interval: Duration,
) -> tokio::task::JoinHandle<()> {
    let interval = interval.max(MIN_INTERVAL);
    tokio::spawn(async move {
        loop {
            let wait = match retention.sweep(Utc::now()).await {
                Ok(r) => {
                    if !r.is_empty() {
                        tracing::info!(
                            chained = r.chained,
                            unchained = r.unchained,
                            "監査ログの保持期間を過ぎた行を削除しました（#831）"
                        );
                    }
                    interval
                }
                Err(e) => {
                    tracing::warn!(error = %e, "監査ログの保持期間の適用に失敗しました");
                    RETRY.min(interval)
                }
            };
            tokio::time::sleep(wait).await;
        }
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn retention_days_bounds() {
        assert!(validate_retention_days(DEFAULT_RETENTION_DAYS).is_ok());
        assert!(validate_retention_days(90).is_ok());
        assert!(validate_retention_days(3650).is_ok());
        assert!(validate_retention_days(89).is_err());
        assert!(validate_retention_days(3651).is_err());
        assert!(validate_retention_days(0).is_err());
    }

    #[test]
    fn report_counts() {
        let mut r = SweepReport::default();
        assert!(r.is_empty());
        r.add(SweepReport {
            chained: 2,
            unchained: 3,
        });
        assert_eq!((r.chained, r.unchained, r.is_empty()), (2, 3, false));
    }
}
