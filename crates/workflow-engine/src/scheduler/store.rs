//! occurrence 冪等記録＋trigger_firing（イベント）＋トリガ tick（engine.md §5.3/§5.4）。

use chrono::{DateTime, Utc};
use sqlx::PgPool;
use uuid::Uuid;

use super::cron;
use super::RunLauncher;

/// スケジューラ操作のエラー。
#[derive(Debug, thiserror::Error)]
pub enum SchedulerStoreError {
    #[error("内部エラー: {0}")]
    Internal(String),
}

#[allow(clippy::needless_pass_by_value)]
fn map_db(e: sqlx::Error) -> SchedulerStoreError {
    SchedulerStoreError::Internal(format!("db: {e}"))
}

/// スケジュールトリガ 1 件（DB から読む）。
#[derive(Debug, Clone, sqlx::FromRow)]
struct ScheduleTriggerRow {
    tenant_id: String,
    trigger_id: String,
    workflow_id: Uuid,
    spec: sqlx::types::Json<serde_json::Value>,
    last_planned_at: Option<DateTime<Utc>>,
}

/// イベントトリガ 1 件（DB から読む）。
#[derive(Debug, Clone, sqlx::FromRow)]
#[allow(clippy::struct_field_names)]
struct EventTriggerRow {
    tenant_id: String,
    trigger_id: String,
    workflow_id: Uuid,
}

/// スケジューラ/マッチャの永続化。
#[derive(Clone)]
pub struct SchedulerStore {
    db: PgPool,
}

impl SchedulerStore {
    pub fn new(db: PgPool) -> Self {
        SchedulerStore { db }
    }

    /// スケジュール tick を 1 回回す（有効な schedule トリガの due occurrence を発火）。
    ///
    /// リーダーのみが呼ぶ前提。各 occurrence を **占有 TX（UNIQUE INSERT → run 起動）** で
    /// 冪等発火し、watermark を前進させる（クラッシュ再起動でも二重投入しない・PIT-31）。
    /// 発火した occurrence 数を返す。
    pub async fn tick_schedules(
        &self,
        now: DateTime<Utc>,
        launcher: &dyn RunLauncher,
    ) -> Result<usize, SchedulerStoreError> {
        // enabled な registration の enabled な schedule トリガを引く。
        let triggers: Vec<ScheduleTriggerRow> = sqlx::query_as(
            "SELECT t.tenant_id, t.trigger_id, t.workflow_id, t.spec, t.last_planned_at \
             FROM workflow_trigger t \
             JOIN workflow_registration r \
               ON r.tenant_id = t.tenant_id AND r.workflow_id = t.workflow_id \
             WHERE t.kind = 'schedule' AND t.enabled AND r.status = 'enabled'",
        )
        .fetch_all(&self.db)
        .await
        .map_err(map_db)?;

        let mut fired = 0usize;
        for t in &triggers {
            let cron5 = t.spec.0.get("cron").and_then(|v| v.as_str()).unwrap_or("");
            let tz = t.spec.0.get("tz").and_then(|v| v.as_str()).unwrap_or("UTC");
            // catchup: skip（既定）は区間内直近 1 occurrence のみ・none は全捨て watermark 前進。
            let catchup = t
                .spec
                .0
                .get("catchup")
                .and_then(|v| v.as_str())
                .unwrap_or("skip");
            let after = t.last_planned_at.unwrap_or(now);
            let Ok(mut occ) = cron::occurrences_between(cron5, tz, after, now, 1000) else {
                continue; // 不正な cron/tz は発火せずスキップ（保存時 V で弾く前提）。
            };
            if occ.is_empty() {
                // 発火無しでも watermark を前進させる（misfire 前進・再発見防止）。
                self.advance_watermark(&t.tenant_id, &t.trigger_id, now)
                    .await?;
                continue;
            }
            // catchup=skip は直近 1 occurrence のみ発火（残りは watermark で消化）。
            if catchup == "skip" || catchup == "none" {
                occ = match (catchup, occ.last()) {
                    (_, Some(&last)) if catchup != "none" => vec![last],
                    _ => vec![],
                };
            }
            for scheduled_at in &occ {
                if self.fire_occurrence(t, *scheduled_at, launcher).await? {
                    fired += 1;
                }
            }
            self.advance_watermark(&t.tenant_id, &t.trigger_id, now)
                .await?;
        }
        Ok(fired)
    }

    /// 1 occurrence を冪等発火する（占有 TX: UNIQUE INSERT → run 起動・engine.md §5.3）。
    /// 戻り値 true = 新規発火（run 起動）、false = 既発火でスキップ。
    async fn fire_occurrence(
        &self,
        t: &ScheduleTriggerRow,
        scheduled_at: DateTime<Utc>,
        launcher: &dyn RunLauncher,
    ) -> Result<bool, SchedulerStoreError> {
        // UNIQUE INSERT で占有（衝突=既発火）。
        let inserted: Option<bool> = sqlx::query_scalar(
            "INSERT INTO schedule_occurrence (tenant_id, workflow_id, trigger_id, scheduled_at) \
             VALUES ($1, $2, $3, $4) \
             ON CONFLICT (tenant_id, workflow_id, trigger_id, scheduled_at) DO NOTHING \
             RETURNING true",
        )
        .bind(&t.tenant_id)
        .bind(t.workflow_id)
        .bind(&t.trigger_id)
        .bind(scheduled_at)
        .fetch_optional(&self.db)
        .await
        .map_err(map_db)?;
        if inserted.is_none() {
            return Ok(false); // 既発火。
        }
        // run 起動（委譲チェックは launcher 内）。run_id を occurrence に記録。
        let run_id = launcher
            .launch(&t.tenant_id, t.workflow_id, "schedule", &t.trigger_id)
            .await;
        sqlx::query(
            "UPDATE schedule_occurrence SET run_id = $4 \
             WHERE tenant_id = $1 AND workflow_id = $2 AND trigger_id = $3 AND scheduled_at = $5",
        )
        .bind(&t.tenant_id)
        .bind(t.workflow_id)
        .bind(&t.trigger_id)
        .bind(run_id)
        .bind(scheduled_at)
        .execute(&self.db)
        .await
        .map_err(map_db)?;
        Ok(true)
    }

    async fn advance_watermark(
        &self,
        tenant_id: &str,
        trigger_id: &str,
        now: DateTime<Utc>,
    ) -> Result<(), SchedulerStoreError> {
        sqlx::query(
            "UPDATE workflow_trigger SET last_planned_at = $3 \
             WHERE tenant_id = $1 AND trigger_id = $2",
        )
        .bind(tenant_id)
        .bind(trigger_id)
        .bind(now)
        .execute(&self.db)
        .await
        .map_err(map_db)?;
        Ok(())
    }

    /// イベント（storage.write）を該当する enabled トリガへマッチさせ run を起動する（engine.md §5.4）。
    ///
    /// `event_id`（outbox id）で `trigger_firing` を UNIQUE 記録し、outbox 1 件につき最大 1 run。
    /// 戻り値 = 起動した run 数。
    pub async fn match_event(
        &self,
        tenant_id: &str,
        source: &str,
        event_id: i64,
        scope: &serde_json::Value,
        launcher: &dyn RunLauncher,
    ) -> Result<usize, SchedulerStoreError> {
        // (tenant, kind=event, source) index で候補トリガを引く（enabled のみ）。
        let triggers: Vec<EventTriggerRow> = sqlx::query_as(
            "SELECT t.tenant_id, t.trigger_id, t.workflow_id FROM workflow_trigger t \
             JOIN workflow_registration r \
               ON r.tenant_id = t.tenant_id AND r.workflow_id = t.workflow_id \
             WHERE t.tenant_id = $1 AND t.kind = 'event' AND t.source = $2 \
               AND t.enabled AND r.status = 'enabled' \
               AND t.spec @> $3",
        )
        .bind(tenant_id)
        .bind(source)
        .bind(sqlx::types::Json(serde_json::json!({ "scope": scope })))
        .fetch_all(&self.db)
        .await
        .map_err(map_db)?;

        let mut fired = 0usize;
        for t in &triggers {
            // (trigger_id, event_id) UNIQUE で 1 イベント 1 run。
            let inserted: Option<bool> = sqlx::query_scalar(
                "INSERT INTO trigger_firing (tenant_id, trigger_id, event_id) \
                 VALUES ($1, $2, $3) ON CONFLICT DO NOTHING RETURNING true",
            )
            .bind(&t.tenant_id)
            .bind(&t.trigger_id)
            .bind(event_id)
            .fetch_optional(&self.db)
            .await
            .map_err(map_db)?;
            if inserted.is_none() {
                continue;
            }
            let run_id = launcher
                .launch(&t.tenant_id, t.workflow_id, "event", &t.trigger_id)
                .await;
            sqlx::query(
                "UPDATE trigger_firing SET run_id = $4 \
                 WHERE tenant_id = $1 AND trigger_id = $2 AND event_id = $3",
            )
            .bind(&t.tenant_id)
            .bind(&t.trigger_id)
            .bind(event_id)
            .bind(run_id)
            .execute(&self.db)
            .await
            .map_err(map_db)?;
            fired += 1;
        }
        Ok(fired)
    }
}
