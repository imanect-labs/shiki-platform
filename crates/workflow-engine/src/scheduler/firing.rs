//! イベントトリガの発火（trigger_firing の占有 → 抑止 → 起動 → 結果の記録・engine.md §5.4）。
//!
//! 1 つのイベント（outbox の 1 件）と 1 つのトリガの組を `trigger_firing` で 1 行に占有し、結果
//! （`outcome`）を同じ行に残す。起動の一時失敗は捨てずに `pending` で再試行を待ち（[`SchedulerStore::
//! retry_pending_firings`]）、回数の上限を超えたら `gave_up` にする（D-21・GAP-07 の T0）。
//!
//! 抑止（GAP-07・D-26）:
//! - **因果の鎖**: イベントを起こした書込の trace_id が run のもの（run_id の 32 桁 hex）なら、その run の
//!   `trigger_chain` ∪ {その run のワークフロー} を鎖とする。発火先のワークフローが鎖に含まれれば
//!   `suppressed_cycle`（自分の書込で自分を起こさない・A→B→A の循環）、鎖が [`MAX_TRIGGER_CHAIN`] を
//!   超えれば `suppressed_depth`。起動した run には鎖を引き継ぐ。
//! - **queued の上限**: 1 ワークフローの queued run が [`MAX_QUEUED_RUNS`] 以上なら `dropped_queue_full`。
//!
//! run の重複は `workflow_run (tenant_id, trigger_id, trigger_event_id)` の部分一意で防ぐ（launch に
//! `event_id` を渡す）。run の作成（launch の Tx）と firing の記録（この Tx）の間に落ちても、再配送・
//! 再試行は同じ run を返すので二重にならない（PIT-31）。

use chrono::{DateTime, Utc};
use serde_json::Value;
use sqlx::types::Json;
use uuid::Uuid;

use super::store::{map_db, SchedulerStore, SchedulerStoreError};
use super::{LaunchOutcome, RunLauncher};

/// 因果の鎖の長さの上限（これを超える連鎖の起動は `suppressed_depth`）。
pub const MAX_TRIGGER_CHAIN: usize = 4;
/// 1 ワークフローの queued run の上限（以上なら発火を捨てる・`dropped_queue_full`）。
pub const MAX_QUEUED_RUNS: i64 = 1000;
/// 一時失敗した起動の試行回数の上限（超えたら `gave_up`）。
pub const MAX_LAUNCH_ATTEMPTS: i32 = 5;
/// 1 tick で再試行する firing の件数。
const RETRY_BATCH: i64 = 100;
/// 再試行の間隔（秒）の基数と上限（10・20・40・80 秒…最大 10 分）。
const RETRY_BASE_SECS: i64 = 10;
const RETRY_CAP_SECS: i64 = 600;

/// 発火の対象のトリガ（候補の照合はイベント源ごとに呼び出し側が行う）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EventTriggerRef {
    pub tenant_id: String,
    pub trigger_id: String,
    pub workflow_id: Uuid,
}

/// 1 つのトリガの発火の結果。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FireOutcome {
    /// run を起動した。
    Fired(Uuid),
    /// このイベントでは既に処理した（発火済み・見送り・抑止・打ち切り）。
    AlreadyHandled,
    /// 起動を見送った（委譲の不成立・多重度あふれ・有効化されていない）。
    Skipped,
    /// 一時的に失敗し、再試行を待つ。
    Pending,
    /// 再試行の上限を超えて打ち切った。
    GaveUp,
    /// 因果の鎖による抑止（自分の書込・循環）。
    SuppressedCycle,
    /// 因果の鎖の長さによる抑止。
    SuppressedDepth,
    /// queued の上限による破棄。
    DroppedQueueFull,
}

impl FireOutcome {
    /// 監査に残す抑止・打ち切りの理由（それ以外は `None`）。
    #[must_use]
    pub fn suppressed_reason(self) -> Option<&'static str> {
        match self {
            FireOutcome::SuppressedCycle => Some("suppressed_cycle"),
            FireOutcome::SuppressedDepth => Some("suppressed_depth"),
            FireOutcome::DroppedQueueFull => Some("dropped_queue_full"),
            FireOutcome::GaveUp => Some("gave_up"),
            _ => None,
        }
    }
}

/// 再試行の 1 tick の結果（打ち切ったものは呼び出し側が監査に残す）。
#[derive(Debug, Default)]
pub struct RetryReport {
    /// 再試行で起動した数。
    pub fired: usize,
    /// 打ち切った firing。
    pub gave_up: Vec<GaveUpFiring>,
}

/// 打ち切った firing（監査の材料）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GaveUpFiring {
    pub tenant_id: String,
    pub trigger_id: String,
    pub workflow_id: Uuid,
    pub event_id: i64,
}

/// 再試行の候補の行。
#[derive(sqlx::FromRow)]
struct PendingRow {
    tenant_id: String,
    trigger_id: String,
    event_id: i64,
    payload: Option<Json<Value>>,
    cause_run_id: Option<Uuid>,
    workflow_id: Option<Uuid>,
    live: bool,
}

/// 由来の run から引いた鎖（由来の run を含めた祖先ワークフロー）と、由来の run。
struct Cause {
    run_id: Uuid,
    chain: Vec<Uuid>,
}

/// trace_id から由来の run を引き、鎖を作る（run の trace_id は run_id の 32 桁 hex・engine.md §11.1）。
///
/// trace_id が run のものでなければ（利用者の操作の trace など）由来は無い。
async fn cause_of(
    conn: &mut sqlx::PgConnection,
    tenant_id: &str,
    cause_run_id: Option<Uuid>,
) -> Result<Option<Cause>, SchedulerStoreError> {
    let Some(run_id) = cause_run_id else {
        return Ok(None);
    };
    let row: Option<(Uuid, Vec<Uuid>)> = sqlx::query_as(
        "SELECT workflow_id, trigger_chain FROM workflow_run WHERE tenant_id = $1 AND run_id = $2",
    )
    .bind(tenant_id)
    .bind(run_id)
    .fetch_optional(&mut *conn)
    .await
    .map_err(map_db)?;
    Ok(row.map(|(workflow_id, mut chain)| {
        if !chain.contains(&workflow_id) {
            chain.push(workflow_id);
        }
        Cause { run_id, chain }
    }))
}

/// 書込の trace_id から由来の run の id を読む（run の trace_id は run_id の 32 桁 hex）。
#[must_use]
pub fn cause_run_of_trace(trace_id: Option<&str>) -> Option<Uuid> {
    let t = trace_id?;
    if t.len() != 32 {
        return None;
    }
    Uuid::try_parse(t).ok()
}

/// 次の再試行の時刻（指数で延ばす・上限つき）。
fn next_attempt_at(now: DateTime<Utc>, attempts: i32) -> DateTime<Utc> {
    let exp = 1_i64
        .checked_shl(u32::try_from((attempts - 1).max(0)).unwrap_or(0))
        .unwrap_or(i64::MAX);
    let secs = RETRY_BASE_SECS.saturating_mul(exp).min(RETRY_CAP_SECS);
    now + chrono::TimeDelta::seconds(secs)
}

impl SchedulerStore {
    /// イベント 1 件で 1 つのトリガを発火する（占有 → 抑止 → 起動 → 記録・同じ Tx の行ロックで直列化）。
    ///
    /// `cause_run_id` はイベントを起こした書込の由来の run（[`cause_run_of_trace`]）。`None` なら因果の鎖で
    /// 抑止しない（storage.write の経路は鎖をまだ使わない・BUG-29）。
    pub async fn fire_event_trigger(
        &self,
        t: &EventTriggerRef,
        event_id: i64,
        payload: &Value,
        cause_run_id: Option<Uuid>,
        launcher: &dyn RunLauncher,
    ) -> Result<FireOutcome, SchedulerStoreError> {
        let mut tx = self.db.begin().await.map_err(map_db)?;
        sqlx::query(
            "INSERT INTO trigger_firing (tenant_id, trigger_id, event_id, outcome) \
             VALUES ($1, $2, $3, 'pending') ON CONFLICT DO NOTHING",
        )
        .bind(&t.tenant_id)
        .bind(&t.trigger_id)
        .bind(event_id)
        .execute(&mut *tx)
        .await
        .map_err(map_db)?;
        // 行をロックして状態を読む（並行 tick・再配送はここで直列化される）。
        let (run_id, outcome, attempts): (Option<Uuid>, String, i32) = sqlx::query_as(
            "SELECT run_id, outcome, attempts FROM trigger_firing \
             WHERE tenant_id = $1 AND trigger_id = $2 AND event_id = $3 FOR UPDATE",
        )
        .bind(&t.tenant_id)
        .bind(&t.trigger_id)
        .bind(event_id)
        .fetch_one(&mut *tx)
        .await
        .map_err(map_db)?;
        if run_id.is_some() || outcome != "pending" {
            tx.commit().await.map_err(map_db)?;
            return Ok(FireOutcome::AlreadyHandled);
        }
        let out = self
            .launch_and_record(
                &mut tx,
                t,
                event_id,
                payload,
                cause_run_id,
                attempts,
                launcher,
            )
            .await?;
        tx.commit().await.map_err(map_db)?;
        Ok(out)
    }

    /// 抑止の判定 → 起動 → 結果の記録（firing の行ロックを持った Tx の中で）。
    #[allow(clippy::too_many_arguments)] // Tx・対象・イベント・由来・試行回数・起動口の束。
    async fn launch_and_record(
        &self,
        tx: &mut sqlx::Transaction<'_, sqlx::Postgres>,
        t: &EventTriggerRef,
        event_id: i64,
        payload: &Value,
        cause_run_id: Option<Uuid>,
        attempts: i32,
        launcher: &dyn RunLauncher,
    ) -> Result<FireOutcome, SchedulerStoreError> {
        let cause = cause_of(tx, &t.tenant_id, cause_run_id).await?;
        if let Some(c) = &cause {
            if c.chain.contains(&t.workflow_id) {
                return finish(
                    tx,
                    t,
                    event_id,
                    "suppressed_cycle",
                    FireOutcome::SuppressedCycle,
                )
                .await;
            }
            if c.chain.len() > MAX_TRIGGER_CHAIN {
                return finish(
                    tx,
                    t,
                    event_id,
                    "suppressed_depth",
                    FireOutcome::SuppressedDepth,
                )
                .await;
            }
        }
        let queued: i64 = sqlx::query_scalar(
            "SELECT count(*) FROM workflow_run \
             WHERE tenant_id = $1 AND workflow_id = $2 AND status = 'queued'",
        )
        .bind(&t.tenant_id)
        .bind(t.workflow_id)
        .fetch_one(&mut **tx)
        .await
        .map_err(map_db)?;
        if queued >= MAX_QUEUED_RUNS {
            return finish(
                tx,
                t,
                event_id,
                "dropped_queue_full",
                FireOutcome::DroppedQueueFull,
            )
            .await;
        }
        let result = launcher
            .launch(
                &t.tenant_id,
                t.workflow_id,
                "event",
                &t.trigger_id,
                payload,
                Some(event_id),
            )
            .await;
        match result {
            LaunchOutcome::Started(run_id) => {
                sqlx::query(
                    "UPDATE trigger_firing SET run_id = $4, outcome = 'fired', payload = NULL, \
                         next_attempt_at = NULL \
                     WHERE tenant_id = $1 AND trigger_id = $2 AND event_id = $3",
                )
                .bind(&t.tenant_id)
                .bind(&t.trigger_id)
                .bind(event_id)
                .bind(run_id)
                .execute(&mut **tx)
                .await
                .map_err(map_db)?;
                if let Some(c) = &cause {
                    // 起動した run に鎖を引き継ぐ（その run の書込が起こすイベントで循環を見つける）。
                    sqlx::query(
                        "UPDATE workflow_run SET trigger_chain = $3, cause_run_id = $4 \
                         WHERE tenant_id = $1 AND run_id = $2 AND cause_run_id IS NULL",
                    )
                    .bind(&t.tenant_id)
                    .bind(run_id)
                    .bind(&c.chain)
                    .bind(c.run_id)
                    .execute(&mut **tx)
                    .await
                    .map_err(map_db)?;
                }
                Ok(FireOutcome::Fired(run_id))
            }
            LaunchOutcome::Skipped(reason) => {
                tracing::info!(tenant = %t.tenant_id, trigger = %t.trigger_id, event_id, ?reason, "イベントトリガの起動を見送りました");
                finish(tx, t, event_id, "skipped", FireOutcome::Skipped).await
            }
            LaunchOutcome::Failed(message) => {
                let attempts = attempts + 1;
                let gave_up = attempts >= MAX_LAUNCH_ATTEMPTS;
                tracing::warn!(tenant = %t.tenant_id, trigger = %t.trigger_id, event_id, attempts, gave_up, error = %message, "イベントトリガの起動に失敗");
                sqlx::query(
                    "UPDATE trigger_firing SET outcome = $4, attempts = $5, next_attempt_at = $6, \
                         payload = $7, cause_run_id = $8 \
                     WHERE tenant_id = $1 AND trigger_id = $2 AND event_id = $3",
                )
                .bind(&t.tenant_id)
                .bind(&t.trigger_id)
                .bind(event_id)
                .bind(if gave_up { "gave_up" } else { "pending" })
                .bind(attempts)
                .bind((!gave_up).then(|| next_attempt_at(Utc::now(), attempts)))
                .bind((!gave_up).then_some(Json(payload)))
                .bind(cause_run_id)
                .execute(&mut **tx)
                .await
                .map_err(map_db)?;
                Ok(if gave_up {
                    FireOutcome::GaveUp
                } else {
                    FireOutcome::Pending
                })
            }
        }
    }

    /// 起動に一時失敗した firing のうち時刻の来たものを再試行する（スケジューラの tick から呼ぶ）。
    ///
    /// トリガが無効化・削除された firing は `skipped` にする（無効化したワークフローを再試行で起こさない）。
    pub async fn retry_pending_firings(
        &self,
        now: DateTime<Utc>,
        tenant_scope: Option<&str>,
        launcher: &dyn RunLauncher,
    ) -> Result<RetryReport, SchedulerStoreError> {
        let rows: Vec<PendingRow> = sqlx::query_as(
            "SELECT f.tenant_id, f.trigger_id, f.event_id, f.payload, f.cause_run_id, t.workflow_id, \
                    coalesce(t.enabled AND r.status = 'enabled' AND t.version = r.enabled_version, \
                             false) AS live \
             FROM trigger_firing f \
             LEFT JOIN workflow_trigger t \
               ON t.tenant_id = f.tenant_id AND t.trigger_id = f.trigger_id \
             LEFT JOIN workflow_registration r \
               ON r.tenant_id = t.tenant_id AND r.workflow_id = t.workflow_id \
             WHERE f.outcome = 'pending' AND f.next_attempt_at <= $1 \
               AND (($2::text IS NULL) OR (f.tenant_id = $2)) \
             ORDER BY f.next_attempt_at LIMIT $3",
        )
        .bind(now)
        .bind(tenant_scope)
        .bind(RETRY_BATCH)
        .fetch_all(&self.db)
        .await
        .map_err(map_db)?;
        let mut report = RetryReport::default();
        for row in rows {
            let mut tx = self.db.begin().await.map_err(map_db)?;
            // 並行 tick と直列化し、まだ再試行を待っているかを読み直す。
            let state: Option<(String, i32)> = sqlx::query_as(
                "SELECT outcome, attempts FROM trigger_firing \
                 WHERE tenant_id = $1 AND trigger_id = $2 AND event_id = $3 FOR UPDATE",
            )
            .bind(&row.tenant_id)
            .bind(&row.trigger_id)
            .bind(row.event_id)
            .fetch_optional(&mut *tx)
            .await
            .map_err(map_db)?;
            let Some((outcome, attempts)) = state else {
                tx.rollback().await.map_err(map_db)?;
                continue;
            };
            let workflow_id = match row.workflow_id {
                Some(w) if outcome == "pending" && row.live => w,
                _ => {
                    if outcome == "pending" {
                        sqlx::query(
                            "UPDATE trigger_firing SET outcome = 'skipped', payload = NULL, \
                                 next_attempt_at = NULL \
                             WHERE tenant_id = $1 AND trigger_id = $2 AND event_id = $3",
                        )
                        .bind(&row.tenant_id)
                        .bind(&row.trigger_id)
                        .bind(row.event_id)
                        .execute(&mut *tx)
                        .await
                        .map_err(map_db)?;
                    }
                    tx.commit().await.map_err(map_db)?;
                    continue;
                }
            };
            let t = EventTriggerRef {
                tenant_id: row.tenant_id.clone(),
                trigger_id: row.trigger_id.clone(),
                workflow_id,
            };
            let payload = row.payload.map_or(Value::Null, |j| j.0);
            let out = self
                .launch_and_record(
                    &mut tx,
                    &t,
                    row.event_id,
                    &payload,
                    row.cause_run_id,
                    attempts,
                    launcher,
                )
                .await?;
            tx.commit().await.map_err(map_db)?;
            match out {
                FireOutcome::Fired(_) => report.fired += 1,
                FireOutcome::GaveUp => report.gave_up.push(GaveUpFiring {
                    tenant_id: t.tenant_id,
                    trigger_id: t.trigger_id,
                    workflow_id,
                    event_id: row.event_id,
                }),
                _ => {}
            }
        }
        Ok(report)
    }
}

/// firing の結果を記録して Tx を返す（起動しない結果）。
async fn finish(
    tx: &mut sqlx::Transaction<'_, sqlx::Postgres>,
    t: &EventTriggerRef,
    event_id: i64,
    outcome: &str,
    out: FireOutcome,
) -> Result<FireOutcome, SchedulerStoreError> {
    sqlx::query(
        "UPDATE trigger_firing SET outcome = $4, payload = NULL, next_attempt_at = NULL \
         WHERE tenant_id = $1 AND trigger_id = $2 AND event_id = $3",
    )
    .bind(&t.tenant_id)
    .bind(&t.trigger_id)
    .bind(event_id)
    .bind(outcome)
    .execute(&mut **tx)
    .await
    .map_err(map_db)?;
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn run_trace_ids_name_their_run() {
        let run = Uuid::new_v4();
        assert_eq!(
            cause_run_of_trace(Some(&run.simple().to_string())),
            Some(run)
        );
        // 利用者の操作の trace（ハイフンつき・長さ違い）や無い trace は由来にならない。
        assert_eq!(cause_run_of_trace(Some(&run.to_string())), None);
        assert_eq!(cause_run_of_trace(Some("abc")), None);
        assert_eq!(cause_run_of_trace(None), None);
    }

    #[test]
    fn retry_backoff_doubles_up_to_the_cap() {
        let now = Utc::now();
        let secs = |a| (next_attempt_at(now, a) - now).num_seconds();
        assert_eq!(secs(1), 10);
        assert_eq!(secs(2), 20);
        assert_eq!(secs(4), 80);
        assert_eq!(secs(20), 600);
    }
}
