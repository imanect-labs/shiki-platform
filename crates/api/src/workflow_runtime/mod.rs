//! ワークフロー実行時（worker/scheduler/event relay の起動時 spawn・Stage A W3）。
//!
//! `workflow.enabled` の時、本番 executor を組んで ①run ワーカー（`FOR UPDATE SKIP LOCKED` で
//! 多重起動安全）②スケジューラ（`LeaderLease` 単一リーダー・cron tick）③イベント relay
//! （outbox 配送台帳 `claim_undelivered("workflow")` → `match_event`）を detach タスクで走らせる。
//! RAG の `spawn_pipeline` と同型（プロセス生存中 loop・エラーは小休止して再試行）。

mod auth;
mod data_port;
mod ports;
mod skill_port;

use std::sync::Arc;

use serde::{Deserialize, Serialize};
use serde_json::Value;
use workflow_engine::{
    BucketConfig, CapabilityAudit, CapabilityNodeExecutor, EffectJournal, LeaderLease, RunStore,
    SchedulerStore, TokenBucket, WorkerConfig, WorkflowRunLauncher, WorkflowWorker,
};

pub use ports::ProdNodePorts;

/// ワークフロー実行時の設定（既定は無効・compose/e2e で有効化）。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WorkflowConfig {
    /// ワーカー/スケジューラを起動するか（無効なら /workflows は保存のみ）。
    #[serde(default)]
    pub enabled: bool,
    /// run ワーカーの並行数。
    #[serde(default = "default_worker_concurrency")]
    pub worker_concurrency: usize,
    /// スケジューラ/relay の tick 間隔（秒）。
    #[serde(default = "default_tick_secs")]
    pub tick_secs: u64,
    /// 3 階層並行上限: テナント全体の同時 step 数。
    #[serde(default = "default_tenant_steps")]
    pub tenant_step_limit: i32,
    /// 3 階層並行上限: workflow 単位の同時 step 数。
    #[serde(default = "default_workflow_steps")]
    pub workflow_step_limit: i32,
    /// 3 階層並行上限: ノード種単位の同時 step 数。
    #[serde(default = "default_node_kind_steps")]
    pub node_kind_step_limit: i32,
    /// step リース秒数。
    #[serde(default = "default_lease_secs")]
    pub lease_secs: i64,
    /// http.request の egress allowlist（secret 宛先束縛と AND・空なら secret 必須）。
    #[serde(default)]
    pub http_allowlist: Vec<String>,
    /// http.request のタイムアウト（ミリ秒）。
    #[serde(default = "default_http_timeout_ms")]
    pub http_timeout_ms: u64,
    /// 外部 API レート（トークンバケット容量・毎秒補充）。0 容量なら無効。
    #[serde(default = "default_rate_capacity")]
    pub rate_capacity: u32,
    #[serde(default = "default_rate_refill")]
    pub rate_refill_per_sec: f64,
}

impl Default for WorkflowConfig {
    fn default() -> Self {
        WorkflowConfig {
            enabled: false,
            worker_concurrency: default_worker_concurrency(),
            tick_secs: default_tick_secs(),
            tenant_step_limit: default_tenant_steps(),
            workflow_step_limit: default_workflow_steps(),
            node_kind_step_limit: default_node_kind_steps(),
            lease_secs: default_lease_secs(),
            http_allowlist: Vec::new(),
            http_timeout_ms: default_http_timeout_ms(),
            rate_capacity: default_rate_capacity(),
            rate_refill_per_sec: default_rate_refill(),
        }
    }
}

fn default_worker_concurrency() -> usize {
    4
}
fn default_tick_secs() -> u64 {
    5
}
fn default_tenant_steps() -> i32 {
    64
}
fn default_workflow_steps() -> i32 {
    16
}
fn default_node_kind_steps() -> i32 {
    8
}
fn default_lease_secs() -> i64 {
    // エンジン側の既定と単一ソースにする。ここに 30 を書くと、RunStore の resume 猶予が
    // 見ている既定とドリフトし得る（#457）。
    workflow_engine::DEFAULT_LEASE_SECS
}
fn default_http_timeout_ms() -> u64 {
    30_000
}
fn default_rate_capacity() -> u32 {
    60
}
fn default_rate_refill() -> f64 {
    1.0
}

/// 能力ゲートウェイの監査。呼び出しは tracing/OTel へ流し（チョークポイント側 audit_log は別途 DB に残る）、
/// **実効スコープの外の呼び出し（`out_of_scope`）は実行主体つきで audit_log にも残す**（受け入れ条件
/// 「スコープ ceiling 外の呼び出しが out_of_scope で監査される」・検証 C-P2-3）。
struct RunAudit {
    recorder: storage::audit::AuditRecorder,
    /// run からワークフローを引く（out_of_scope の監査に workflow / run を残す・検証 N9）。
    db: sqlx::PgPool,
}
impl CapabilityAudit for RunAudit {
    fn record(&self, tenant_id: &str, api: &str, allowed: bool, meta: &Value) {
        if allowed {
            tracing::info!(target: "workflow.capability", tenant = %tenant_id, %api, meta = %meta, "capability 呼び出し");
        } else {
            tracing::warn!(target: "workflow.capability", tenant = %tenant_id, %api, meta = %meta, "capability 拒否");
        }
    }

    fn record_out_of_scope(&self, ec: &workflow_engine::ExecCtx, api: &str, meta: &Value) {
        self.record(&ec.tenant_id, api, false, meta);
        let ctx = auth::auth_ctx(ec);
        let recorder = self.recorder.clone();
        let db = self.db.clone();
        let (api, mut meta, trace) = (api.to_string(), meta.clone(), ec.trace_id.clone());
        let (run_id, step, kind) = (ec.run_id, ec.step_path.clone(), ec.principal_kind.clone());
        // 監査は同期の呼び出し口から非同期に書く（ノードの失敗を待たせない・書込失敗は warn）。
        tokio::spawn(async move {
            // どのワークフローのどの run・step かを残す（run が無い呼び出しは主体だけ）。
            if let Some(o) = meta.as_object_mut() {
                if let Some(run_id) = run_id {
                    o.insert("run_id".into(), serde_json::json!(run_id));
                    let wf: Option<(uuid::Uuid, i64)> = sqlx::query_as(
                        "SELECT workflow_id, version FROM workflow_run \
                         WHERE tenant_id = $1 AND run_id = $2",
                    )
                    .bind(&ctx.tenant_id)
                    .bind(run_id)
                    .fetch_optional(&db)
                    .await
                    .unwrap_or_else(|e| {
                        tracing::warn!(error = %e, "out_of_scope の監査でワークフローを引けません");
                        None
                    });
                    if let Some((workflow_id, version)) = wf {
                        o.insert("workflow_id".into(), serde_json::json!(workflow_id));
                        o.insert("workflow_version".into(), serde_json::json!(version));
                    }
                }
                if let Some(step) = step {
                    o.entry("step").or_insert_with(|| serde_json::json!(step));
                }
                o.insert("principal_kind".into(), serde_json::json!(kind));
            }
            let entry = storage::audit::AuditEntry {
                action: storage::audit::AuditAction::WorkflowCapabilityOutOfScope,
                object_type: "workflow_capability",
                object_id: &api,
                decision: storage::audit::Decision::Deny,
                trace_id: trace.as_deref(),
                metadata: meta,
            };
            if let Err(e) = recorder.record(&ctx, entry).await {
                tracing::warn!(error = %e, %api, "out_of_scope の監査記録に失敗");
            }
        });
    }
}

/// spawn に必要な依存（AppState 構築時の材料から組む）。launcher/runs は API と共有する。
pub struct RuntimeDeps {
    pub db: sqlx::PgPool,
    pub launcher: WorkflowRunLauncher,
    pub runs: RunStore,
    pub storage: Arc<storage::StorageService>,
    pub search: Option<Arc<rag::SearchService>>,
    pub gateway: Arc<llm_gateway::LlmGateway>,
    pub sandbox: Option<Arc<dyn sandbox_client::Sandbox>>,
    /// コード実行系（agent_invoke ノード）の隔離ティア（admin ポリシー・chat と同一の単一ソース）。
    pub sandbox_backend: sandbox_client::SandboxBackend,
    pub secrets: Option<Arc<secrets::SecretStore>>,
    /// CSV 表データ（csv.query/patch/write ノード・Task 11P.9）。
    pub tabular: Option<Arc<tabular::TabularService>>,
    /// skill.invoke の実行時解決（レジストリ→artifact を実行主体 ReBAC で読む・#344）。
    pub skill_artifacts: Option<Arc<artifact::ArtifactStore>>,
    pub http: reqwest::Client,
    pub redis_url: Option<String>,
    pub config: WorkflowConfig,
    /// 構造化データ（data ノード・`Shiki.data.*`・Task 15.7）。
    pub data: Arc<data::DataStore>,
    /// data.transition の FSM 定義の解決。
    pub fsms: Arc<data::FsmStore>,
}

/// 本番 executor を組む（能力ゲートウェイ → ポート・レート制御・script エンジン）。
async fn build_prod_executor(
    deps: &RuntimeDeps,
    launcher: &WorkflowRunLauncher,
) -> CapabilityNodeExecutor {
    let ports = Arc::new(ProdNodePorts {
        storage: Arc::clone(&deps.storage),
        search: deps.search.clone(),
        gateway: Arc::clone(&deps.gateway),
        sandbox: deps.sandbox.clone(),
        sandbox_backend: deps.sandbox_backend,
        secrets: deps.secrets.clone(),
        tabular: deps.tabular.clone(),
        skill_artifacts: deps.skill_artifacts.clone(),
        launcher: launcher.clone(),
        http: deps.http.clone(),
        db: deps.db.clone(),
        data: Some(Arc::clone(&deps.data)),
        fsms: Some(Arc::clone(&deps.fsms)),
    });
    let journal = EffectJournal::new(deps.db.clone());
    let audit = Arc::new(RunAudit {
        db: deps.db.clone(),
        recorder: storage::audit::AuditRecorder::new(deps.db.clone()),
    });
    let mut executor = CapabilityNodeExecutor::new(ports, journal, audit).with_http_allowlist(
        deps.config.http_allowlist.clone(),
        deps.config.http_timeout_ms,
    );

    // レート制御（Redis があれば・無ければ制限なしで続行）。
    if deps.config.rate_capacity > 0 {
        if let Some(url) = deps.redis_url.as_ref() {
            match build_bucket(url).await {
                Ok(bucket) => {
                    executor = executor.with_ratelimit(
                        bucket,
                        BucketConfig {
                            capacity: deps.config.rate_capacity,
                            refill_per_sec: deps.config.rate_refill_per_sec,
                        },
                    );
                }
                Err(e) => {
                    tracing::warn!(error = %e, "レート制御 Redis 接続に失敗（制限なしで継続）");
                }
            }
        }
    }

    // script エンジン（プリウォーム 1 回・失敗なら script ノードは permanent 失敗）。
    // ミニアプリの関数と同じエンジンを共有する（ゲストのコンパイルはプロセスで 1 回・起動の短縮）。
    match script_runtime::engine::ScriptEngine::shared() {
        Ok(engine) => {
            executor =
                executor.with_script_engine(engine, script_runtime::engine::Limits::default());
        }
        Err(e) => {
            tracing::warn!(error = %e, "script エンジン初期化に失敗（script ノードは失敗する）");
        }
    }
    executor
}

/// worker/scheduler/event-relay を起動する（`enabled=false` なら何もしない）。
pub async fn spawn_workflow_runtime(deps: RuntimeDeps) {
    if !deps.config.enabled {
        tracing::info!("workflow ランタイムは無効（/workflows は保存のみ）");
        return;
    }
    let runs = deps.runs.clone();
    let launcher = deps.launcher.clone();

    let executor: Arc<dyn workflow_engine::NodeExecutor> =
        Arc::new(build_prod_executor(&deps, &launcher).await);

    // ① run ワーカー（多重起動安全・detach）。3 階層並行制御を結線（10.5・順番待ち）。
    let concurrency_store = workflow_engine::ConcurrencyStore::new(deps.db.clone());
    let worker = WorkflowWorker::new(
        runs.clone(),
        executor,
        WorkerConfig {
            lease_secs: deps.config.lease_secs,
            ..WorkerConfig::default()
        },
    )
    .with_concurrency(
        concurrency_store.clone(),
        workflow_engine::ConcurrencyLimits {
            tenant_steps: deps.config.tenant_step_limit,
            workflow_steps: deps.config.workflow_step_limit,
            node_kind_steps: deps.config.node_kind_step_limit,
        },
    );
    let worker_id = format!("wf-worker-{}", std::process::id());
    worker.spawn(deps.config.worker_concurrency, &worker_id);
    tracing::info!(
        concurrency = deps.config.worker_concurrency,
        "ワークフロー run ワーカーを起動しました"
    );

    // 実行履歴 GC（engine.md §12.2）はここでは起動しない。`workflow.enabled` と独立した保守
    // ランタイムとして `wiring::wire_workflow` が enabled ゲートの外で起動する（#448）。

    // ② スケジューラ ③ イベント relay（単一リーダー・tick ループ・detach）。
    let leader_id = format!("wf-sched-{}", std::process::id());
    let lease = LeaderLease::new(deps.db.clone(), leader_id, deps.config.lease_secs);
    let sched = SchedulerStore::new(deps.db.clone());
    let launcher_dyn = workflow_engine::run::launcher::into_dyn(launcher);
    let tick = std::time::Duration::from_secs(deps.config.tick_secs.max(1));
    let relay_db = deps.db.clone();
    let tick_runs = deps.runs.clone();
    let tick_concurrency = concurrency_store;

    // event consumer を登録し、現バックログ（有効化前の storage.write）を飛ばす。
    // 登録と「バックログの fast-forward」は原子的でなければならない（autocommit で呼ぶと登録だけ
    // 先にコミットされ、fast-forward 失敗時に relay がバックログ全件を拾って一斉発火する）。
    // pool 版が自前で txn を張る。
    if let Err(e) = storage::event::register_consumer_on_pool(&relay_db, "workflow").await {
        tracing::error!(error = %e, "workflow consumer 登録に失敗");
    }

    tokio::spawn(async move {
        loop {
            match lease.acquire_or_renew().await {
                Ok(true) => {
                    let now = chrono::Utc::now();
                    if let Err(e) = sched.tick_schedules(now, None, launcher_dyn.as_ref()).await {
                        tracing::warn!(error = %e, "スケジューラ tick でエラー");
                    }
                    if let Err(e) =
                        relay_events(&relay_db, &sched, &tick_runs, launcher_dyn.as_ref()).await
                    {
                        tracing::warn!(error = %e, "イベント relay でエラー");
                    }
                    // wait(timer) 起床・wait(event/timer) の timeout 回収（engine.md §5.1）。
                    if let Err(e) = tick_runs.wake_due_timers(now, None).await {
                        tracing::warn!(error = %e, "waiting_timer 起床でエラー");
                    }
                    if let Err(e) = tick_runs.expire_due_waits(now, None).await {
                        tracing::warn!(error = %e, "wait timeout 回収でエラー");
                    }
                    // ユーザーキャンセルのドレイン回収（running 完走後の terminal 化・Task 10.14）。
                    if let Err(e) = tick_runs.drain_cancel_requested(None).await {
                        tracing::warn!(error = %e, "cancel ドレインでエラー");
                    }
                    // run 多重度の promote・run timeout・並行カウンタ突合（10.5）。
                    if let Err(e) = tick_runs.promote_queued_runs(None).await {
                        tracing::warn!(error = %e, "queued promote でエラー");
                    }
                    if let Err(e) = tick_runs.expire_run_timeouts(now, None).await {
                        tracing::warn!(error = %e, "run timeout 回収でエラー");
                    }
                    // リース失効 step の回収（engine.md §9.5）。claim を ready 専用にした分の
                    // takeover はここが担う（#438）。並行カウンタの減分リークは直後の reconcile が
                    // running の実数から再計算して回収するため、**reconcile より前**に呼ぶ。
                    match tick_runs.reclaim_expired_leases(None).await {
                        Ok(0) => {}
                        Ok(n) => tracing::info!(reclaimed = n, "リース失効 step を回収しました"),
                        Err(e) => tracing::warn!(error = %e, "リース失効回収でエラー"),
                    }
                    if let Err(e) = tick_concurrency.reconcile().await {
                        tracing::warn!(error = %e, "並行カウンタ reconcile でエラー");
                    }
                }
                Ok(false) => {} // 別インスタンスがリーダー。
                Err(e) => tracing::warn!(error = %e, "リーダーリース取得に失敗"),
            }
            tokio::time::sleep(tick).await;
        }
    });
    tracing::info!("ワークフロースケジューラ/イベント relay を起動しました");
}

/// outbox の workflow 未配送イベントを 1 バッチ処理する（storage.write のみ・at-least-once）。
///
/// 各イベントを ①トリガマッチ（run 起動）②wait(event) 起床、の両方へ配る（engine.md §5.5 同一マッチャ）。
/// いずれも祖先束縛 scope（イベント発生フォルダ id）＋filter で照合する。
async fn relay_events(
    db: &sqlx::PgPool,
    sched: &SchedulerStore,
    runs: &RunStore,
    launcher: &dyn workflow_engine::RunLauncher,
) -> Result<(), anyhow::Error> {
    // 溜まっていれば 1 tick の中で続けて捌く（構造化データのレコード変更も outbox に乗るので、一括取込の
    // 直後は件数が多い。1 tick 1 バッチだと他の consumer と GC を長く止める・Task 15.6）。
    for _ in 0..RELAY_BATCHES_PER_TICK {
        if relay_events_batch(db, sched, runs, launcher).await? < RELAY_BATCH {
            break;
        }
    }
    Ok(())
}

/// 1 tick で捌く outbox の最大バッチ数と、1 バッチの件数（claim は配送済みの行を走査するので、1 回の件数を
/// 大きくして回数を減らす・一括取込の 10 万件で 64 件ずつだと追いつくまで 15〜20 分かかった・検証 D-14）。
const RELAY_BATCHES_PER_TICK: usize = 32;
const RELAY_BATCH: usize = 500;

async fn relay_events_batch(
    db: &sqlx::PgPool,
    sched: &SchedulerStore,
    runs: &RunStore,
    launcher: &dyn workflow_engine::RunLauncher,
) -> Result<usize, anyhow::Error> {
    let mut tx = db.begin().await?;
    let events = storage::event::claim_undelivered(&mut tx, "workflow", RELAY_BATCH as i64).await?;
    if events.is_empty() {
        return Ok(0);
    }
    let mut ids = Vec::with_capacity(events.len());
    for ev in &events {
        // data サービス由来イベント（FSM 遷移等・payload.event_type 付き）は storage.write では
        // ない。Phase 10.3 の data トリガが消費するまでは配送済みにしてスキップする
        // （storage.write として誤発火させない・Task 9.10）。
        if ev
            .payload
            .get("event_type")
            .and_then(|v| v.as_str())
            .is_some()
        {
            ids.push(ev.id);
            continue;
        }
        // Stage A: 全 WriteOp を storage.write source に写像。祖先束縛はイベント発生フォルダ id で照合。
        let event_folder = ev
            .payload
            .get("parent_id")
            .and_then(|v| v.as_str())
            .and_then(|s| uuid::Uuid::parse_str(s).ok());
        // match_event / wake_event_waits は自 tx（trigger_firing UNIQUE・wait_subscription fired）で冪等。
        sched
            .match_event(
                &ev.tenant_id,
                "storage.write",
                ev.id,
                event_folder,
                &ev.payload,
                launcher,
            )
            .await?;
        runs.wake_event_waits(&ev.tenant_id, "storage.write", event_folder, &ev.payload)
            .await?;
        ids.push(ev.id);
    }
    storage::event::mark_delivered(&mut tx, "workflow", &ids).await?;
    tx.commit().await?;
    tracing::debug!(
        count = ids.len(),
        "outbox → workflow event matcher / wait へ relay"
    );
    Ok(events.len())
}

async fn build_bucket(redis_url: &str) -> Result<TokenBucket, anyhow::Error> {
    let client = redis::Client::open(redis_url)?;
    let conn = client.get_connection_manager().await?;
    Ok(TokenBucket::new(conn))
}

#[cfg(test)]
mod audit_tests {
    #![allow(clippy::unwrap_used, clippy::expect_used)]
    use super::*;

    /// 実効スコープの外の呼び出しは実行主体つきで audit_log に残る（検証 C-P2-3。以前は tracing だけ）。
    #[tokio::test]
    async fn out_of_scope_is_written_to_the_audit_log() {
        let Ok(url) = std::env::var("STORAGE_TEST_DATABASE_URL") else {
            eprintln!("STORAGE_TEST_DATABASE_URL 未設定のためスキップ");
            return;
        };
        let pool = sqlx::postgres::PgPoolOptions::new()
            .max_connections(2)
            .connect(&url)
            .await
            .unwrap();
        sqlx::migrate!("../../migrations").run(&pool).await.unwrap();
        let tenant = format!("t-{}", uuid::Uuid::new_v4());
        let audit = RunAudit {
            recorder: storage::audit::AuditRecorder::new(pool.clone()),
            db: pool.clone(),
        };
        let run_id = uuid::Uuid::new_v4();
        let workflow_id = uuid::Uuid::new_v4();
        sqlx::query(
            "INSERT INTO workflow_run (tenant_id, run_id, org, workflow_id, version, trigger_kind, principal) \
             VALUES ($1, $2, 'acme', $3, 3, 'schedule', 'wf-1')",
        )
        .bind(&tenant)
        .bind(run_id)
        .bind(workflow_id)
        .execute(&pool)
        .await
        .unwrap();
        let ec = workflow_engine::ExecCtx {
            tenant_id: tenant.clone(),
            org: "acme".into(),
            principal: "wf-1".into(),
            principal_kind: "workflow".into(),
            trace_id: Some("trace-oos".into()),
            run_id: Some(run_id),
            step_path: Some("n1".into()),
        };
        audit.record_out_of_scope(
            &ec,
            "data.write",
            &serde_json::json!({ "reason": "out_of_scope", "step": "n1" }),
        );
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
        let row = loop {
            let row: Option<(String, String, String, String, serde_json::Value)> = sqlx::query_as(
                "SELECT action, object_id, decision, coalesce(trace_id, ''), metadata FROM audit_log \
                 WHERE tenant_id = $1",
            )
            .bind(&tenant)
            .fetch_optional(&pool)
            .await
            .unwrap();
            if let Some(r) = row {
                break r;
            }
            assert!(std::time::Instant::now() < deadline, "audit_log に残らない");
            tokio::time::sleep(std::time::Duration::from_millis(50)).await;
        };
        assert_eq!(row.0, "workflow.capability.out_of_scope");
        assert_eq!(row.1, "data.write");
        assert_eq!(row.2, "deny");
        assert_eq!(row.3, "trace-oos");
        assert_eq!(row.4["reason"], "out_of_scope");
        // どのワークフローのどの run・step かが分かる（検証 N9）。
        assert_eq!(row.4["run_id"], serde_json::json!(run_id));
        assert_eq!(row.4["workflow_id"], serde_json::json!(workflow_id));
        assert_eq!(row.4["workflow_version"], 3);
        assert_eq!(row.4["step"], "n1");
        assert_eq!(row.4["principal_kind"], "workflow");
    }
}
