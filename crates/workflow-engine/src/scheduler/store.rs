//! occurrence 冪等記録＋trigger_firing（イベント）＋トリガ tick（engine.md §5.3/§5.4）。

use chrono::{DateTime, Utc};
use sqlx::PgPool;
use uuid::Uuid;

use super::cron;
use super::RunLauncher;

/// `catchup: "none"` で「時刻どおり」とみなす遅れの上限。tick は既定 5 秒ごとなので、稼働中の occurrence は
/// 数秒の遅れで評価される。
pub const ON_TIME_GRACE: chrono::TimeDelta = chrono::TimeDelta::seconds(60);

/// `catchup: "none"` でスケジューラが「動き続けていた」とみなす tick の間隔の上限（前回の評価からの経過）。
/// 1 回の tick ループが重い（取込直後の大量イベントの relay 等）・`tick_secs` を大きくした・リーダーの
/// 交代（リース満了待ち）で間が空いても、この範囲なら区間内の最新 occurrence は停止中の穴ではないので発火する。
pub const MAX_LIVE_TICK_GAP: chrono::TimeDelta = chrono::TimeDelta::minutes(5);

/// スケジューラ操作のエラー。
#[derive(Debug, thiserror::Error)]
pub enum SchedulerStoreError {
    #[error("内部エラー: {0}")]
    Internal(String),
}

#[allow(clippy::needless_pass_by_value)]
pub(super) fn map_db(e: sqlx::Error) -> SchedulerStoreError {
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

/// ファイル保存トリガ（storage.write）の候補（フォルダの祖先束縛）。filter・閲覧の確認・発火は呼び出し側
/// （relay）が行う。
#[derive(Debug, Clone)]
pub struct FileTriggerCandidate {
    pub trigger: super::EventTriggerRef,
    /// registration の org（ワークフロー主体の AuthContext を組む）。
    pub org: String,
    /// トリガの filter（条件木の JSON・無ければ `None`）。
    pub filter: Option<serde_json::Value>,
}

impl FileTriggerCandidate {
    /// filter がペイロードに一致するか（fail-closed: 不正 filter・評価エラーは一致しない）。
    #[must_use]
    pub fn admits(&self, payload: &serde_json::Value) -> bool {
        trigger_filter_admits(
            self.filter.as_ref(),
            payload,
            &self.trigger.tenant_id,
            &self.trigger.trigger_id,
        )
    }
}

#[derive(sqlx::FromRow)]
struct FileTriggerRow {
    tenant_id: String,
    trigger_id: String,
    workflow_id: Uuid,
    org: String,
    filter: Option<sqlx::types::Json<serde_json::Value>>,
}

/// データ変更トリガの候補（テーブル束縛・GAP-07）。見え方と filter の評価は呼び出し側（relay）が行う。
#[derive(Debug, Clone)]
pub struct DataTriggerCandidate {
    pub trigger: super::EventTriggerRef,
    /// registration の org（ワークフロー主体の AuthContext を組む）。
    pub org: String,
    pub source: crate::vocab::EventSource,
    pub table_id: Uuid,
    /// トリガの filter（条件木の JSON・無ければ `None`）。
    pub filter: Option<serde_json::Value>,
}

impl DataTriggerCandidate {
    /// filter がペイロードに一致するか（fail-closed: 不正 filter・評価エラーは一致しない）。
    #[must_use]
    pub fn admits(&self, payload: &serde_json::Value) -> bool {
        trigger_filter_admits(
            self.filter.as_ref(),
            payload,
            &self.trigger.tenant_id,
            &self.trigger.trigger_id,
        )
    }
}

#[derive(sqlx::FromRow)]
struct DataTriggerRow {
    tenant_id: String,
    trigger_id: String,
    workflow_id: Uuid,
    org: String,
    source: String,
    table_id: String,
    filter: Option<sqlx::types::Json<serde_json::Value>>,
}

/// トリガの filter がペイロードに一致するか（fail-closed: 不正 filter・型不一致などの評価エラーは一致しない・
/// engine.md §5.6）。
pub(crate) fn trigger_filter_admits(
    filter: Option<&serde_json::Value>,
    payload: &serde_json::Value,
    tenant_id: &str,
    trigger_id: &str,
) -> bool {
    let Some(filter_json) = filter.filter(|v| !v.is_null()) else {
        return true;
    };
    match serde_json::from_value::<crate::ir::expr::Condition>(filter_json.clone()) {
        Ok(cond) => match crate::control::event_filter_matches(&cond, payload) {
            Ok(matched) => matched,
            Err(e) => {
                tracing::warn!(code = e.code, error = %e.message, tenant = tenant_id, trigger = %trigger_id, "トリガ filter の評価に失敗（fail-closed）");
                false
            }
        },
        Err(e) => {
            tracing::warn!(error = %e, tenant = tenant_id, trigger = %trigger_id, "トリガ filter が不正（fail-closed）");
            false
        }
    }
}

/// 読めないスケジュール（保存時の検証より前に保存された式など）は発火しない。黙って止まらないよう、どのトリガかを
/// 警告に残す（曜日の読み方を標準に直したとき、写せない式を誤りにした・BUG-95）。
fn unreadable_schedule(t: &ScheduleTriggerRow, cron5: &str, e: &cron::CronError) {
    tracing::warn!(
        tenant = %t.tenant_id, trigger = %t.trigger_id, workflow = %t.workflow_id, cron = %cron5, error = %e,
        "スケジュールを読めないため発火しません（ワークフローを開いて式を直し、保存し直してください）"
    );
}

/// 読めないスケジュールの警告を、同じトリガで出し直すまでの間。tick のたびに出すとログを埋める。
const UNREADABLE_WARN_EVERY: std::time::Duration = std::time::Duration::from_hours(1);

/// テナント・トリガ → 読めないスケジュールの警告を最後に出した時刻。
type WarnedAt = std::collections::HashMap<(String, String), std::time::Instant>;

/// 読めないスケジュールの警告を最後に出した時刻（このプロセスの中だけ）。
static UNREADABLE_WARNED: std::sync::LazyLock<std::sync::Mutex<WarnedAt>> =
    std::sync::LazyLock::new(Default::default);

/// 読めないスケジュールの警告をいま出すか（このトリガではじめてか、前に出してから [`UNREADABLE_WARN_EVERY`] たった）。
/// 出すなら時刻を覚える。
fn warn_unreadable_now(
    warned: &mut WarnedAt,
    t: &ScheduleTriggerRow,
    now: std::time::Instant,
) -> bool {
    let key = (t.tenant_id.clone(), t.trigger_id.clone());
    match warned.get(&key) {
        Some(at) if now.saturating_duration_since(*at) < UNREADABLE_WARN_EVERY => false,
        _ => {
            warned.insert(key, now);
            true
        }
    }
}

/// 読めないスケジュールを飛ばす（警告はトリガごとに 1 時間に 1 回）。
fn skip_unreadable(t: &ScheduleTriggerRow, cron5: &str, e: &cron::CronError) {
    let now = std::time::Instant::now();
    // 覚えておく表が使えない（ほかのスレッドが panic した）ときは、黙らずに毎回出す。
    let warn = match UNREADABLE_WARNED.lock() {
        Ok(mut warned) => warn_unreadable_now(&mut warned, t, now),
        Err(_) => true,
    };
    if warn {
        unreadable_schedule(t, cron5, e);
    }
}

/// スケジューラ/マッチャの永続化。
#[derive(Clone)]
pub struct SchedulerStore {
    pub(super) db: PgPool,
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
            // catchup: skip（既定）は区間内直近 1 occurrence のみ・none は時刻どおりの 1 件だけ（遅れた分は捨てる）。
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
                // **時刻どおりの発火だけ**: 区間内の最新 occurrence を、スケジューラが動き続けていた
                // （前回の評価から MAX_LIVE_TICK_GAP 以内）か、今から ON_TIME_GRACE 以内に来たものなら発火する。
                // それ以外（スケジューラ停止中の穴＝misfire）は捨てて watermark だけ前進させる
                // （ir.md §6.1・engine.md §5.2）。区間をまるごと捨てると、稼働中の時刻どおりの発火まで
                // 捨てて一度も発火しない（#904）。
                match cron::latest_occurrence_between(cron5, tz, after, now) {
                    Ok(Some(last))
                        if now - after <= MAX_LIVE_TICK_GAP || now - last <= ON_TIME_GRACE =>
                    {
                        vec![last]
                    }
                    Ok(_) => Vec::new(),
                    Err(e) => {
                        skip_unreadable(t, cron5, &e);
                        continue;
                    }
                }
            } else if catchup == "skip" {
                // **区間内の最新 occurrence のみ**。長時間ダウン（>1000 回分）でも最新を取りこぼさない
                // よう、固定 cap の enumerate ではなく最新を直接求める。
                match cron::latest_occurrence_between(cron5, tz, after, now) {
                    Ok(Some(last)) => vec![last],
                    Ok(None) => Vec::new(),
                    Err(e) => {
                        skip_unreadable(t, cron5, &e);
                        continue;
                    }
                }
            } else {
                // 全 occurrence（catchup=all は Stage A 外だが防御的に全件）。
                match cron::occurrences_between(cron5, tz, after, now, 10_000) {
                    Ok(v) => v,
                    Err(e) => {
                        skip_unreadable(t, cron5, &e);
                        continue;
                    }
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
        // 見送り・一時失敗は run_id NULL のまま（occurrence は 1 回きり・再試行はしない）。
        // 予定の時刻を渡し、run の実行情報の予定の日時にする（入力ときっかけの内容は null のまま・GAP-15）。
        let run_id = launcher
            .launch(
                &t.tenant_id,
                t.workflow_id,
                "schedule",
                &t.trigger_id,
                &super::schedule_payload(scheduled_at),
                None,
            )
            .await
            .run_id();
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

    /// データ変更トリガの候補を、テーブルの集合でまとめて引く（enabled かつ有効化バージョン一致のみ）。
    ///
    /// scope はテーブル束縛必須（`{ "table": "<uuid>" }`・V3 が正規形を強制）。table キーを持たない scope は
    /// ワイルドカードに縮退させず一致しない（fail-closed）。
    pub async fn data_trigger_candidates(
        &self,
        tenant_id: &str,
        tables: &[Uuid],
    ) -> Result<Vec<DataTriggerCandidate>, SchedulerStoreError> {
        if tables.is_empty() {
            return Ok(Vec::new());
        }
        let sources: Vec<&str> = crate::vocab::EventSource::ALL
            .iter()
            .filter(|s| s.is_data())
            .map(|s| s.as_str())
            .collect();
        let table_keys: Vec<String> = tables.iter().map(|t| t.hyphenated().to_string()).collect();
        let rows: Vec<DataTriggerRow> = sqlx::query_as(
            "SELECT t.tenant_id, t.trigger_id, t.workflow_id, r.org, t.source, \
                    t.spec->'scope'->>'table' AS table_id, t.spec->'filter' AS filter \
             FROM workflow_trigger t \
             JOIN workflow_registration r \
               ON r.tenant_id = t.tenant_id AND r.workflow_id = t.workflow_id \
             WHERE t.tenant_id = $1 AND t.kind = 'event' AND t.source = ANY($2) \
               AND t.enabled AND r.status = 'enabled' AND t.version = r.enabled_version \
               AND (t.spec->'scope'->>'table') = ANY($3)",
        )
        .bind(tenant_id)
        .bind(&sources)
        .bind(&table_keys)
        .fetch_all(&self.db)
        .await
        .map_err(map_db)?;
        Ok(rows
            .into_iter()
            .filter_map(|r| {
                Some(DataTriggerCandidate {
                    trigger: super::EventTriggerRef {
                        tenant_id: r.tenant_id,
                        trigger_id: r.trigger_id,
                        workflow_id: r.workflow_id,
                    },
                    org: r.org,
                    source: crate::vocab::EventSource::parse(&r.source)?,
                    table_id: Uuid::parse_str(&r.table_id).ok()?,
                    filter: r.filter.map(|j| j.0).filter(|v| !v.is_null()),
                })
            })
            .collect())
    }

    /// ファイル保存トリガ（storage.write）の候補を、イベント発生フォルダで引く（enabled かつ有効化バージョン
    /// 一致のみ・engine.md §5.4）。
    ///
    /// **祖先束縛**: トリガの folder scope が、イベント発生フォルダの祖先（node_closure・自分自身 depth 0 を
    /// 含むので完全一致も包含）なら一致する。**scope はフォルダ束縛必須（全購読禁止・fail-closed）**: folder
    /// キーを持たない scope（未対応形状・欠落）はワイルドカードに縮退させず一切マッチしない（保存時 V3 が
    /// Stage A の形状 { "folder": "<uuid>" } を強制する）。filter の評価・workflow プリンシパルの閲覧の確認・
    /// 発火（[`Self::fire_event_trigger`] / [`Self::defer_event_trigger`]）は呼び出し側（relay）が行う。
    pub async fn file_trigger_candidates(
        &self,
        tenant_id: &str,
        event_folder: Option<Uuid>,
    ) -> Result<Vec<FileTriggerCandidate>, SchedulerStoreError> {
        let Some(folder) = event_folder else {
            return Ok(Vec::new());
        };
        let rows: Vec<FileTriggerRow> = sqlx::query_as(
            "SELECT t.tenant_id, t.trigger_id, t.workflow_id, r.org, t.spec->'filter' AS filter \
             FROM workflow_trigger t \
             JOIN workflow_registration r \
               ON r.tenant_id = t.tenant_id AND r.workflow_id = t.workflow_id \
             WHERE t.tenant_id = $1 AND t.kind = 'event' AND t.source = 'storage.write' \
               AND t.enabled AND r.status = 'enabled' \
               AND t.version = r.enabled_version \
               AND (t.spec->'scope' ? 'folder') \
               AND EXISTS ( SELECT 1 FROM node_closure c \
                            WHERE c.tenant_id = $1 \
                              AND c.ancestor = (t.spec->'scope'->>'folder')::uuid \
                              AND c.descendant = $2 )",
        )
        .bind(tenant_id)
        .bind(folder)
        .fetch_all(&self.db)
        .await
        .map_err(map_db)?;
        Ok(rows
            .into_iter()
            .map(|r| FileTriggerCandidate {
                trigger: super::EventTriggerRef {
                    tenant_id: r.tenant_id,
                    trigger_id: r.trigger_id,
                    workflow_id: r.workflow_id,
                },
                org: r.org,
                filter: r.filter.map(|j| j.0).filter(|v| !v.is_null()),
            })
            .collect())
    }
}

#[cfg(test)]
mod unreadable_tests {
    use super::*;

    fn trigger(id: &str) -> ScheduleTriggerRow {
        ScheduleTriggerRow {
            tenant_id: "t1".into(),
            trigger_id: id.into(),
            workflow_id: Uuid::nil(),
            spec: sqlx::types::Json(serde_json::json!({ "cron": "0 9 * * L" })),
            last_planned_at: None,
            created_at: Utc::now(),
        }
    }

    /// 読めないスケジュールの警告は、トリガごとにはじめてと、前に出してから 1 時間たったときだけ出す（tick ごとに出さない）。
    #[test]
    fn unreadable_schedules_are_warned_once_an_hour() {
        let mut warned = WarnedAt::new();
        let start = std::time::Instant::now();
        let (a, b) = (trigger("a"), trigger("b"));
        assert!(warn_unreadable_now(&mut warned, &a, start));
        assert!(!warn_unreadable_now(
            &mut warned,
            &a,
            start + std::time::Duration::from_secs(5)
        ));
        assert!(!warn_unreadable_now(
            &mut warned,
            &a,
            start + std::time::Duration::from_secs(3_599)
        ));
        // ほかのトリガは別に数える。
        assert!(warn_unreadable_now(
            &mut warned,
            &b,
            start + std::time::Duration::from_secs(5)
        ));
        assert!(warn_unreadable_now(
            &mut warned,
            &a,
            start + UNREADABLE_WARN_EVERY
        ));
    }
}
