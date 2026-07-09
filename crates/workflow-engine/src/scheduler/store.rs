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
    /// 初回 watermark の起点（last_planned_at が NULL のとき使う）。
    created_at: DateTime<Utc>,
}

/// イベントトリガ 1 件（DB から読む）。
#[derive(Debug, Clone, sqlx::FromRow)]
#[allow(clippy::struct_field_names)]
struct EventTriggerRow {
    tenant_id: String,
    trigger_id: String,
    workflow_id: Uuid,
    /// トリガ spec（filter 条件木を含む）。
    spec: sqlx::types::Json<serde_json::Value>,
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
    ///
    /// `tenant_scope` を渡すとそのテナントのトリガのみ tick する（tenant シャーディング・テスト分離）。
    /// `None` は全テナント横断（既定のリーダー動作）。
    pub async fn tick_schedules(
        &self,
        now: DateTime<Utc>,
        tenant_scope: Option<&str>,
        launcher: &dyn RunLauncher,
    ) -> Result<usize, SchedulerStoreError> {
        // enabled な registration の**有効化バージョンと一致する** enabled な schedule トリガを引く。
        // t.version = r.enabled_version で古い/未来バージョンの残存トリガが発火しないようにする。
        let triggers: Vec<ScheduleTriggerRow> = sqlx::query_as(
            "SELECT t.tenant_id, t.trigger_id, t.workflow_id, t.spec, t.last_planned_at, t.created_at \
             FROM workflow_trigger t \
             JOIN workflow_registration r \
               ON r.tenant_id = t.tenant_id AND r.workflow_id = t.workflow_id \
             WHERE t.kind = 'schedule' AND t.enabled AND r.status = 'enabled' \
               AND t.version = r.enabled_version \
               AND (($1::text IS NULL) OR (t.tenant_id = $1))",
        )
        .bind(tenant_scope)
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
            // 初回（last_planned_at NULL）は**トリガ作成時刻**から数える。tick 時刻から数えると
            // 作成〜初回 tick の間に到来した occurrence を取りこぼす。
            let after = t.last_planned_at.unwrap_or(t.created_at);
            let occ: Vec<DateTime<Utc>> = if catchup == "none" {
                // 全捨て（watermark だけ前進）。
                Vec::new()
            } else if catchup == "skip" {
                // **区間内の最新 occurrence のみ**。長時間ダウン（>1000 回分）でも最新を取りこぼさない
                // よう、固定 cap の enumerate ではなく最新を直接求める。
                match cron::latest_occurrence_between(cron5, tz, after, now) {
                    Ok(Some(last)) => vec![last],
                    Ok(None) => Vec::new(),
                    Err(_) => continue,
                }
            } else {
                // 全 occurrence（catchup=all は Stage A 外だが防御的に全件）。
                match cron::occurrences_between(cron5, tz, after, now, 10_000) {
                    Ok(v) => v,
                    Err(_) => continue,
                }
            };
            if occ.is_empty() {
                // 発火無しでも watermark を前進させる（misfire 前進・再発見防止）。
                self.advance_watermark(&t.tenant_id, &t.trigger_id, now)
                    .await?;
                continue;
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
        // occurrence 行を占有し、**FOR UPDATE で行ロックしてから** run_id を確認する。並行 tick が
        // 同一 occurrence を同時発火せず（競走で二重投入しない）、かつ予約後・run 起動前にクラッシュして
        // run_id が NULL のまま残った occurrence は次 tick で再試行できる（トリガ取りこぼし防止）。
        let mut tx = self.db.begin().await.map_err(map_db)?;
        sqlx::query(
            "INSERT INTO schedule_occurrence (tenant_id, workflow_id, trigger_id, scheduled_at) \
             VALUES ($1, $2, $3, $4) \
             ON CONFLICT (tenant_id, workflow_id, trigger_id, scheduled_at) DO NOTHING",
        )
        .bind(&t.tenant_id)
        .bind(t.workflow_id)
        .bind(&t.trigger_id)
        .bind(scheduled_at)
        .execute(&mut *tx)
        .await
        .map_err(map_db)?;
        // 行をロックして run_id を読む（並行 tick はここで直列化される）。
        let existing_run: Option<Uuid> = sqlx::query_scalar(
            "SELECT run_id FROM schedule_occurrence \
             WHERE tenant_id = $1 AND workflow_id = $2 AND trigger_id = $3 AND scheduled_at = $4 \
             FOR UPDATE",
        )
        .bind(&t.tenant_id)
        .bind(t.workflow_id)
        .bind(&t.trigger_id)
        .bind(scheduled_at)
        .fetch_one(&mut *tx)
        .await
        .map_err(map_db)?;
        if existing_run.is_some() {
            tx.commit().await.map_err(map_db)?; // 既に発火済み。
            return Ok(false);
        }
        // run_id が NULL（新規占有 or クラッシュ残り）→ ロック保持中に launch して記録する。
        let run_id = launcher
            .launch(
                &t.tenant_id,
                t.workflow_id,
                "schedule",
                &t.trigger_id,
                &serde_json::Value::Null,
            )
            .await;
        sqlx::query(
            "UPDATE schedule_occurrence SET run_id = $5 \
             WHERE tenant_id = $1 AND workflow_id = $2 AND trigger_id = $3 AND scheduled_at = $4",
        )
        .bind(&t.tenant_id)
        .bind(t.workflow_id)
        .bind(&t.trigger_id)
        .bind(scheduled_at)
        .bind(run_id)
        .execute(&mut *tx)
        .await
        .map_err(map_db)?;
        tx.commit().await.map_err(map_db)?;
        Ok(run_id.is_some())
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
    /// `event_folder` はイベント発生フォルダ id（**祖先束縛**の照合対象）・`payload` はイベントペイロード
    /// （filter 評価と `$from trigger` 透過に使う）。戻り値 = 起動した run 数。
    pub async fn match_event(
        &self,
        tenant_id: &str,
        source: &str,
        event_id: i64,
        event_folder: Option<Uuid>,
        payload: &serde_json::Value,
        launcher: &dyn RunLauncher,
    ) -> Result<usize, SchedulerStoreError> {
        // (tenant, kind=event, source) index で候補トリガを引く（enabled かつ有効化バージョン一致のみ）。
        // **祖先束縛**: トリガの folder scope が、イベント発生フォルダの祖先（node_closure・自分自身 depth 0
        // を含むので完全一致も包含）なら一致する。**scope はフォルダ束縛必須（全購読禁止・fail-closed）**:
        // folder キーを持たない scope（未対応形状・欠落）はワイルドカードに縮退させず一切マッチしない
        // （保存時 V3 が Stage A の形状 { "folder": "<uuid>" } を強制する）。
        let triggers: Vec<EventTriggerRow> = sqlx::query_as(
            "SELECT t.tenant_id, t.trigger_id, t.workflow_id, t.spec FROM workflow_trigger t \
             JOIN workflow_registration r \
               ON r.tenant_id = t.tenant_id AND r.workflow_id = t.workflow_id \
             WHERE t.tenant_id = $1 AND t.kind = 'event' AND t.source = $2 \
               AND t.enabled AND r.status = 'enabled' \
               AND t.version = r.enabled_version \
               AND (t.spec->'scope' ? 'folder') \
               AND EXISTS ( SELECT 1 FROM node_closure c \
                            WHERE c.tenant_id = $1 \
                              AND c.ancestor = (t.spec->'scope'->>'folder')::uuid \
                              AND c.descendant = $3 )",
        )
        .bind(tenant_id)
        .bind(source)
        .bind(event_folder)
        .fetch_all(&self.db)
        .await
        .map_err(map_db)?;

        let mut fired = 0usize;
        for t in &triggers {
            // filter 評価（イベントペイロードに対して・fail-closed: 不正 filter/不一致は発火しない・§5.6）。
            if let Some(filter_json) = t.spec.0.get("filter").filter(|v| !v.is_null()) {
                match serde_json::from_value::<crate::ir::expr::Condition>(filter_json.clone()) {
                    Ok(cond) if crate::control::event_filter_matches(&cond, payload) => {}
                    Ok(_) => continue,
                    Err(e) => {
                        tracing::warn!(error = %e, tenant = tenant_id, trigger = %t.trigger_id, "トリガ filter が不正（fail-closed）");
                        continue;
                    }
                }
            }
            // (trigger_id, event_id) UNIQUE で 1 イベント 1 run。占有 INSERT ＋ FOR UPDATE 行ロックで
            // 並行 tick/再配信を直列化しつつ、launch 前クラッシュで run_id が NULL のまま残った firing は
            // 次配信で再起動できるようにする（occurrence と同じ回復方式）。
            let mut tx = self.db.begin().await.map_err(map_db)?;
            sqlx::query(
                "INSERT INTO trigger_firing (tenant_id, trigger_id, event_id) \
                 VALUES ($1, $2, $3) ON CONFLICT DO NOTHING",
            )
            .bind(&t.tenant_id)
            .bind(&t.trigger_id)
            .bind(event_id)
            .execute(&mut *tx)
            .await
            .map_err(map_db)?;
            let existing_run: Option<Uuid> = sqlx::query_scalar(
                "SELECT run_id FROM trigger_firing \
                 WHERE tenant_id = $1 AND trigger_id = $2 AND event_id = $3 FOR UPDATE",
            )
            .bind(&t.tenant_id)
            .bind(&t.trigger_id)
            .bind(event_id)
            .fetch_one(&mut *tx)
            .await
            .map_err(map_db)?;
            if existing_run.is_some() {
                tx.commit().await.map_err(map_db)?; // 既に発火済み。
                continue;
            }
            let run_id = launcher
                .launch(&t.tenant_id, t.workflow_id, "event", &t.trigger_id, payload)
                .await;
            sqlx::query(
                "UPDATE trigger_firing SET run_id = $4 \
                 WHERE tenant_id = $1 AND trigger_id = $2 AND event_id = $3",
            )
            .bind(&t.tenant_id)
            .bind(&t.trigger_id)
            .bind(event_id)
            .bind(run_id)
            .execute(&mut *tx)
            .await
            .map_err(map_db)?;
            tx.commit().await.map_err(map_db)?;
            if run_id.is_some() {
                fired += 1;
            }
        }
        Ok(fired)
    }
}
