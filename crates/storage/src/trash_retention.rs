//! ゴミ箱の保持期間（GAP-108c・D-202）。
//!
//! human の判断（2026-11-11・D-202）:
//! - 仕組みだけ入れ、**既定は無効**（`tenant.trash_retention_days` が null）。運用者がテナントごとに日数（1〜3650）を
//!   入れたときだけ、ゴミ箱に入れてから日数を過ぎたものを自動で完全に削除する。
//! - 日数を入れたときも短くしたときも、**一斉には消さない猶予**を置く。
//!
//! 期限は `greatest(削除した時刻, trash_retention_since) + 日数`。日数を null から入れたときと短くしたときは
//! `trash_retention_since` を今にする（[`TrashRetention::set_retention_days`]）。長くしたとき・null に戻したとき・
//! 同じ値を入れ直したときは変えない。短くした時点から数え直すので、期限が前より延びる項目もある（一斉に消えない
//! ことを優先する）。
//!
//! 掃除（[`StorageService::sweep_trash_retention`]）は中断アップロードの回収タイマ（`upload_gc_timer`）の回に
//! 相乗りする。advisory lock を取った 1 台だけが進め、日数が入った active なテナントの期限を過ぎた削除の根を
//! テナントを横断して古い順に取り、利用者の完全削除と同じ本体（`service::purge`）で、根の組織のシステムの主体
//! （`system:trash-retention`）として消す。権限の check は通さず、監査に `storage.trash_retention.purge` を
//! チェーンで残す。中に生きているノードがある根（7p-4 より前に崩れた行）は消さずに warn を出して数える。
//! 1 回の掃除で消す根・ノードの数に上限を置き、残りは次の回に回す。

use authz::{AuthContext, Principal, PrincipalKind, TenantId};
use chrono::{DateTime, Utc};
use serde_json::json;
use sqlx::{PgConnection, PgPool};
use uuid::Uuid;

use crate::audit::{record_on, AuditAction, AuditEntry, Chain, Decision};
use crate::db_tenant::begin_cross_tenant_tx;
use crate::error::StorageError;
use crate::model::NodeKind;
use crate::service::{PurgeOutcome, PurgeReport, StorageService};

/// 保持日数の下限（1 日）。
pub const MIN_TRASH_RETENTION_DAYS: i32 = 1;
/// 保持日数の上限（10 年・打ち間違いの検出）。DB の CHECK（migration 0157）と同じ。
pub const MAX_TRASH_RETENTION_DAYS: i32 = 3650;

/// 1 回の掃除で完全に削除する根の上限（残りは次の回に回す）。
pub const SWEEP_MAX_ROOTS: usize = 100;
/// 1 回の掃除で完全に削除するノード（配下を含む）の合計の上限。届いたら次の根へ進まない（1 つの根はその大きさに
/// よらず最後まで消す）。「ゴミ箱を空にする」の 1 回と同じ大きさ（`EMPTY_TRASH_MAX_NODES`）。
pub const SWEEP_MAX_NODES: usize = 2_000;
/// 1 回の掃除で確かめる根の候補の上限（消さずに飛ばす根が続いても、1 回を短く保つ）。
const SWEEP_MAX_SCAN: usize = 500;
/// 1 回の問い合わせで引く根の候補の数。
const SWEEP_BATCH: i64 = 50;
/// 1 回の掃除で、commit の後にその場で片付ける片付け待ちの行の上限（残りは同じ回の `sweep_purge_pending` が片付ける）。
const SWEEP_INLINE_CLEANUP: usize = 300;

/// 自動の完全削除の主体（監査の actor・人ではなく掃除）。
pub const SWEEP_ACTOR: &str = "system:trash-retention";

/// テナントを横断するトランザクションの目的（`application_name` に載る）。
const PURPOSE: &str = "storage-trash-retention";
/// 1 台だけが進めるための advisory lock のキー（`pg_try_advisory_xact_lock(hashtextextended(<キー>, 0))`・ほかの
/// キーと接頭辞で分ける）。
pub const SWEEP_LOCK_KEY: &str = "storage:trash_retention";

/// 保持日数の範囲を検証する（設定経路の入口で必ず通す）。
pub fn validate_trash_retention_days(days: i32) -> Result<(), StorageError> {
    if (MIN_TRASH_RETENTION_DAYS..=MAX_TRASH_RETENTION_DAYS).contains(&days) {
        return Ok(());
    }
    Err(StorageError::Invalid(format!(
        "ゴミ箱の保持日数は {MIN_TRASH_RETENTION_DAYS}〜{MAX_TRASH_RETENTION_DAYS} 日で指定してください（指定値: {days}）"
    )))
}

/// 日数を変えたときに猶予を置き直すか（null から入れた・短くした）。
///
/// 長くした・null に戻した・同じ値のときは置き直さない（期限が早まらないので、一斉に消えることはない）。
#[must_use]
pub fn resets_grace(previous: Option<i32>, next: Option<i32>) -> bool {
    match (previous, next) {
        (None, Some(_)) => true,
        (Some(p), Some(n)) => n < p,
        (_, None) => false,
    }
}

/// テナントのゴミ箱の保持期間の設定。
#[derive(Debug, Clone, Copy, PartialEq, Eq, sqlx::FromRow)]
pub struct TrashRetentionSetting {
    /// 保持日数（`None` は自動では消さない・既定）。
    pub days: Option<i32>,
    /// 猶予の起点（期限は `greatest(削除した時刻, since) + days`）。
    pub since: Option<DateTime<Utc>>,
}

/// [`TrashRetention::set_retention_days`] の結果。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TrashRetentionChange {
    /// 変更前の日数。
    pub previous: Option<i32>,
    /// 変更後の日数。
    pub days: Option<i32>,
    /// 猶予を置き直したか（null から入れた・短くした）。
    pub grace_reset: bool,
    /// 変更後の猶予の起点。
    pub since: Option<DateTime<Utc>>,
}

/// ゴミ箱の保持期間の設定を読み書きする（運用者 API・shiki-admin）。
#[derive(Clone)]
pub struct TrashRetention {
    db: PgPool,
}

impl TrashRetention {
    pub fn new(db: PgPool) -> Self {
        TrashRetention { db }
    }

    /// テナントの設定（テナント台帳に無ければ `None`）。
    pub async fn setting(
        &self,
        tenant_id: &str,
    ) -> Result<Option<TrashRetentionSetting>, StorageError> {
        Ok(sqlx::query_as(
            "SELECT trash_retention_days AS days, trash_retention_since AS since \
             FROM tenant WHERE tenant_id = $1",
        )
        .bind(tenant_id)
        .fetch_optional(&self.db)
        .await?)
    }

    /// テナントの保持日数を設定し（`None` で自動の完全削除を止める）、変更を監査に残す（ハッシュチェーンに連結）。
    ///
    /// active なテナントのみ（無い・active でなければ `None`）。null から入れたときと短くしたときは猶予の起点を
    /// 今にする（[`resets_grace`]）。`actor` は監査の記録者（運用者 API なら `provisioner:<client>`、CLI なら `cli`）。
    pub async fn set_retention_days(
        &self,
        tenant_id: &str,
        days: Option<i32>,
        actor: &str,
        metadata: serde_json::Value,
    ) -> Result<Option<TrashRetentionChange>, StorageError> {
        if let Some(d) = days {
            validate_trash_retention_days(d)?;
        }
        let mut tx = self.db.begin().await?;
        let Some((org, previous)): Option<(String, Option<i32>)> = sqlx::query_as(
            "SELECT org, trash_retention_days FROM tenant \
             WHERE tenant_id = $1 AND status = 'active' FOR UPDATE",
        )
        .bind(tenant_id)
        .fetch_optional(&mut *tx)
        .await?
        else {
            return Ok(None);
        };
        let grace_reset = resets_grace(previous, days);
        let since: Option<DateTime<Utc>> = sqlx::query_scalar(
            "UPDATE tenant SET trash_retention_days = $2, \
                 trash_retention_since = CASE WHEN $3 THEN now() ELSE trash_retention_since END, \
                 updated_at = now() \
             WHERE tenant_id = $1 RETURNING trash_retention_since",
        )
        .bind(tenant_id)
        .bind(days)
        .bind(grace_reset)
        .fetch_one(&mut *tx)
        .await?;
        let ctx = system_ctx(tenant_id, &org, actor)?;
        let mut metadata = metadata;
        if let Some(m) = metadata.as_object_mut() {
            m.insert("retention_days".into(), json!(days));
            m.insert("previous".into(), json!(previous));
            m.insert("grace_reset".into(), json!(grace_reset));
            m.insert("since".into(), json!(since));
        }
        record_on(
            &mut tx,
            &ctx,
            AuditEntry {
                action: AuditAction::TenantTrashRetentionSet,
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
        Ok(Some(TrashRetentionChange {
            previous,
            days,
            grace_reset,
            since,
        }))
    }
}

/// [`StorageService::sweep_trash_retention`] の 1 回の結果。
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct TrashRetentionSweep {
    /// advisory lock を取って進めたか（取れなければ、ほかの台が進めているので何もしない）。
    pub leader: bool,
    /// 完全に削除した根の数。
    pub purged: usize,
    /// 完全に削除したノードの数（配下を含む）。
    pub nodes: usize,
    /// 中に生きているノードがあって消さずに飛ばした根の数（7p-4 より前に崩れた行・毎回 warn を出す）。
    pub live_descendants: usize,
    /// 消せずに失敗した根の数（次の回に回す）。
    pub failed: usize,
    /// 上限に届いて、残りを次の回に回したか。
    pub more: bool,
}

/// 期限を過ぎた削除の根の候補。
#[derive(sqlx::FromRow)]
struct DueRoot {
    tenant_id: String,
    org: String,
    id: Uuid,
    kind: String,
    deleted_at: DateTime<Utc>,
}

/// 保持期間の完全削除（[`StorageService::purge_for_retention`]・`service::purge`）の結果。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum RetentionPurge {
    /// 完全に削除した。
    Purged(PurgeReport),
    /// 押さえた後の値で、もう期限を過ぎていない（削除し直した・保持期間を変えた・テナントが active でない）。
    NotDue,
    /// 押さえた後の値で、消さない理由がある（ゴミ箱に無い・削除したフォルダの中・中に生きているノード）。
    Refused(PurgeOutcome),
}

/// 押さえた根が、まだ期限を過ぎていたときの値（監査の metadata に残す）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct Due {
    pub(crate) days: i32,
    pub(crate) deleted_at: DateTime<Utc>,
}

impl StorageService {
    /// 同じ接続プールで、ゴミ箱の保持期間の設定を読み書きする窓口。
    pub fn trash_retention(&self) -> TrashRetention {
        TrashRetention::new(self.pool().clone())
    }

    /// 保持期間を過ぎたゴミ箱の中身を、古い順に完全に削除する（テナント横断の保守処理・`now` を起点にする）。
    ///
    /// advisory lock が取れなければ（ほかの台が進めている）何もせずに `leader: false` を返す。根ごとの失敗は
    /// warn に残して [`TrashRetentionSweep::failed`] に数えて続ける。候補を引けない（DB に届かない）ときだけ `Err`。
    pub async fn sweep_trash_retention(
        &self,
        now: DateTime<Utc>,
    ) -> Result<TrashRetentionSweep, StorageError> {
        // リーダーのロックは、この掃除の間だけ持つ Tx に取る（落ちても Tx と一緒に外れる）。候補もこの Tx で引く。
        let mut lock = begin_cross_tenant_tx(self.pool(), PURPOSE).await?;
        let leader: bool =
            sqlx::query_scalar("SELECT pg_try_advisory_xact_lock(hashtextextended($1, 0))")
                .bind(SWEEP_LOCK_KEY)
                .fetch_one(&mut *lock)
                .await?;
        let mut report = TrashRetentionSweep {
            leader,
            ..TrashRetentionSweep::default()
        };
        if !leader {
            lock.rollback().await?;
            return Ok(report);
        }
        let mut after: Option<(DateTime<Utc>, Uuid)> = None;
        let mut scanned = 0usize;
        let mut budget = SWEEP_INLINE_CLEANUP;
        'scan: loop {
            let batch = due_roots(&mut lock, now, after).await?;
            let short = (batch.len() as i64) < SWEEP_BATCH;
            for root in &batch {
                after = Some((root.deleted_at, root.id));
                scanned += 1;
                self.purge_due_root(root, now, &mut budget, &mut report)
                    .await;
                if report.purged >= SWEEP_MAX_ROOTS
                    || report.nodes >= SWEEP_MAX_NODES
                    || scanned >= SWEEP_MAX_SCAN
                {
                    report.more = true;
                    break 'scan;
                }
            }
            if short {
                break;
            }
        }
        // 書いたものは無い（ロックを外すだけ）。
        lock.rollback().await?;
        Ok(report)
    }

    /// 期限を過ぎた根を 1 つ完全に削除し、結果を `report` に数える。
    async fn purge_due_root(
        &self,
        root: &DueRoot,
        now: DateTime<Utc>,
        budget: &mut usize,
        report: &mut TrashRetentionSweep,
    ) {
        let Some(kind) = NodeKind::parse(&root.kind) else {
            tracing::warn!(
                tenant_id = %root.tenant_id,
                node_id = %root.id,
                kind = %root.kind,
                "ゴミ箱の保持期間: 未知のノード種別のため完全に削除しません"
            );
            report.failed += 1;
            return;
        };
        match self
            .purge_for_retention(&root.tenant_id, &root.org, (root.id, kind), now, budget)
            .await
        {
            Ok(RetentionPurge::Purged(r)) => {
                report.purged += 1;
                report.nodes += r.nodes;
            }
            Ok(RetentionPurge::Refused(PurgeOutcome::HasLiveDescendants)) => {
                tracing::warn!(
                    tenant_id = %root.tenant_id,
                    org = %root.org,
                    node_id = %root.id,
                    "ゴミ箱の保持期間: 削除したフォルダの中に生きているノードがあるため、完全に削除せずに飛ばしました"
                );
                report.live_descendants += 1;
            }
            // 候補を選んだ後に、復元した・削除し直した・親を削除した・保持期間を変えた（次の回に選び直す）か、
            // 並行して完全に削除された（利用者の完全削除・ほかの台）。
            Ok(_) | Err(StorageError::NotFound) => {}
            Err(e) => {
                tracing::warn!(
                    error = %e,
                    tenant_id = %root.tenant_id,
                    node_id = %root.id,
                    "ゴミ箱の保持期間: 完全に削除できませんでした（次の回にやり直します）"
                );
                report.failed += 1;
            }
        }
    }
}

/// 日数が入った active なテナントの、`now` で期限を過ぎた削除の根を、`after` の次から古い順に引く。
///
/// 期限 `greatest(deleted_at, since) + days <= now` は、`deleted_at <= now - days` かつ
/// `since <= now - days`（since が null なら猶予なし）と同じ。テナントごとに部分索引 `node_trash_idx`
/// （migration 0157）を古い順に読み、全体でも古い順に並べ直す。
async fn due_roots(
    conn: &mut PgConnection,
    now: DateTime<Utc>,
    after: Option<(DateTime<Utc>, Uuid)>,
) -> Result<Vec<DueRoot>, StorageError> {
    // tenant-sql: allow(テナントを横断する保守の掃除・begin_cross_tenant_tx で RLS を迂回する)
    Ok(sqlx::query_as(
        "SELECT r.tenant_id, r.org, r.id, r.kind, r.deleted_at \
         FROM tenant t \
         CROSS JOIN LATERAL ( \
             SELECT n.tenant_id, n.org, n.id, n.kind, n.deleted_at FROM node n \
             WHERE n.tenant_id = t.tenant_id AND n.deleted_at IS NOT NULL AND n.system = false \
               AND n.deleted_at <= $1 - make_interval(days => t.trash_retention_days) \
               AND ($2::timestamptz IS NULL OR (n.deleted_at, n.id) > ($2::timestamptz, $3::uuid)) \
               AND NOT EXISTS ( \
                 SELECT 1 FROM node p \
                 WHERE p.tenant_id = n.tenant_id AND p.id = n.parent_id AND p.deleted_at IS NOT NULL) \
             ORDER BY n.deleted_at, n.id LIMIT $4) r \
         WHERE t.status = 'active' AND t.trash_retention_days IS NOT NULL \
           AND (t.trash_retention_since IS NULL \
                OR t.trash_retention_since <= $1 - make_interval(days => t.trash_retention_days)) \
         ORDER BY r.deleted_at, r.id LIMIT $4",
    )
    .bind(now)
    .bind(after.map(|(ts, _)| ts))
    .bind(after.map(|(_, id)| id))
    .bind(SWEEP_BATCH)
    .fetch_all(&mut *conn)
    .await?)
}

/// 押さえた根（`ctx` のテナント・組織）が、`now` でまだ期限を過ぎているか（完全削除の Tx の中で、根と配下を
/// 押さえた後に呼ぶ）。過ぎていれば日数と削除した時刻を返す。テナントが active でない・日数が null に戻った・
/// 削除し直して期限がまだ来ていない・システム領域（#392・ゴミ箱に出さない）のときは `None`。
///
/// テナントの行を `FOR KEY SHARE` で完全削除の Tx の終わりまで押さえる。設定の変更
/// （[`TrashRetention::set_retention_days`]・`FOR UPDATE`）と待ち合い、変更が先に commit すれば新しい値で確かめ
/// 直して（READ COMMITTED の再評価）`None` になり、完全削除が先なら変更はその commit の後になる（保持期間を
/// 止めた後に、古い日数での完全削除が確定しない）。ロックの順は node → tenant → blob → 監査のチェーンで、
/// tenant → 監査のチェーンの設定の変更とは逆にならない。
pub(crate) async fn due_on(
    conn: &mut PgConnection,
    ctx: &AuthContext,
    root_id: Uuid,
    now: DateTime<Utc>,
) -> Result<Option<Due>, StorageError> {
    let row: Option<(i32, DateTime<Utc>)> = sqlx::query_as(
        "SELECT t.trash_retention_days, n.deleted_at \
         FROM node n JOIN tenant t ON t.tenant_id = n.tenant_id \
         WHERE n.tenant_id = $1 AND n.org = $2 AND n.id = $3 AND n.deleted_at IS NOT NULL \
           AND n.system = false AND t.status = 'active' AND t.trash_retention_days IS NOT NULL \
           AND greatest(n.deleted_at, t.trash_retention_since) \
               + make_interval(days => t.trash_retention_days) <= $4 \
         FOR KEY SHARE OF t",
    )
    .bind(ctx.tenant_id())
    .bind(ctx.org())
    .bind(root_id)
    .bind(now)
    .fetch_optional(&mut *conn)
    .await?;
    Ok(row.map(|(days, deleted_at)| Due { days, deleted_at }))
}

/// 設定の変更を監査に刻むための合成コンテキスト。`tenant_id` は DB の値でも文字ポリシーを通す。
fn system_ctx(tenant_id: &str, org: &str, actor: &str) -> Result<AuthContext, StorageError> {
    let tenant = TenantId::parse(tenant_id)
        .map_err(|e| StorageError::Invalid(format!("tenant_id が不正です: {e}")))?;
    Ok(AuthContext::new(
        Principal {
            kind: PrincipalKind::User,
            id: actor.to_string(),
            email: None,
            groups: vec![],
            roles: vec![],
            tenant_id: Some(tenant_id.to_string()),
        },
        org.into(),
        tenant,
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn retention_days_bounds() {
        assert!(validate_trash_retention_days(1).is_ok());
        assert!(validate_trash_retention_days(30).is_ok());
        assert!(validate_trash_retention_days(3650).is_ok());
        assert!(validate_trash_retention_days(0).is_err());
        assert!(validate_trash_retention_days(-1).is_err());
        assert!(validate_trash_retention_days(3651).is_err());
    }

    #[test]
    fn grace_resets_only_when_deadline_could_come_earlier() {
        // null から入れた・短くした: 置き直す（入れた直後・縮めた直後に一斉に消えない）。
        assert!(resets_grace(None, Some(30)));
        assert!(resets_grace(Some(60), Some(10)));
        // 長くした・同じ・null に戻した・null のまま: 置き直さない。
        assert!(!resets_grace(Some(10), Some(30)));
        assert!(!resets_grace(Some(30), Some(30)));
        assert!(!resets_grace(Some(30), None));
        assert!(!resets_grace(None, None));
    }
}
