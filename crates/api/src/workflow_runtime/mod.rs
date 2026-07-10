//! ワークフロー実行時（worker/scheduler/event relay の起動時 spawn・Stage A W3）。
//!
//! `workflow.enabled` の時、本番 executor を組んで ①run ワーカー（`FOR UPDATE SKIP LOCKED` で
//! 多重起動安全）②スケジューラ（`LeaderLease` 単一リーダー・cron tick）③イベント relay
//! （outbox 配送台帳 `claim_undelivered("workflow")` → `match_event`）を detach タスクで走らせる。
//! RAG の `spawn_pipeline` と同型（プロセス生存中 loop・エラーは小休止して再試行）。

mod ports;

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
fn default_lease_secs() -> i64 {
    30
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

/// 能力ゲートウェイの監査を tracing/OTel へ流す（チョークポイント側 audit_log は別途 DB に残る）。
struct TracingAudit;
impl CapabilityAudit for TracingAudit {
    fn record(&self, tenant_id: &str, api: &str, allowed: bool, meta: &Value) {
        if allowed {
            tracing::info!(target: "workflow.capability", tenant = %tenant_id, %api, meta = %meta, "capability 呼び出し");
        } else {
            tracing::warn!(target: "workflow.capability", tenant = %tenant_id, %api, meta = %meta, "capability 拒否");
        }
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
    pub http: reqwest::Client,
    pub redis_url: Option<String>,
    pub config: WorkflowConfig,
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
        launcher: launcher.clone(),
        http: deps.http.clone(),
        db: deps.db.clone(),
    });
    let journal = EffectJournal::new(deps.db.clone());
    let mut executor = CapabilityNodeExecutor::new(ports, journal, Arc::new(TracingAudit))
        .with_http_allowlist(
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
    match script_runtime::engine::ScriptEngine::new() {
        Ok(engine) => {
            executor = executor
                .with_script_engine(Arc::new(engine), script_runtime::engine::Limits::default());
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

    // ① run ワーカー（多重起動安全・detach）。
    let worker = WorkflowWorker::new(
        runs.clone(),
        executor,
        WorkerConfig {
            lease_secs: deps.config.lease_secs,
            ..WorkerConfig::default()
        },
    );
    let worker_id = format!("wf-worker-{}", std::process::id());
    worker.spawn(deps.config.worker_concurrency, &worker_id);
    tracing::info!(
        concurrency = deps.config.worker_concurrency,
        "ワークフロー run ワーカーを起動しました"
    );

    // ② スケジューラ ③ イベント relay（単一リーダー・tick ループ・detach）。
    let leader_id = format!("wf-sched-{}", std::process::id());
    let lease = LeaderLease::new(deps.db.clone(), leader_id, deps.config.lease_secs);
    let sched = SchedulerStore::new(deps.db.clone());
    let launcher_dyn = workflow_engine::run::launcher::into_dyn(launcher);
    let tick = std::time::Duration::from_secs(deps.config.tick_secs.max(1));
    let relay_db = deps.db.clone();
    let tick_runs = deps.runs.clone();

    // event consumer を登録し、現バックログ（有効化前の storage.write）を飛ばす。
    {
        let mut conn = match relay_db.acquire().await {
            Ok(c) => c,
            Err(e) => {
                tracing::error!(error = %e, "workflow consumer 登録用接続に失敗");
                return;
            }
        };
        if let Err(e) = storage::event::register_consumer(&mut conn, "workflow").await {
            tracing::error!(error = %e, "workflow consumer 登録に失敗");
        }
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
    let mut tx = db.begin().await?;
    let events = storage::event::claim_undelivered(&mut tx, "workflow", 64).await?;
    if events.is_empty() {
        return Ok(());
    }
    let mut ids = Vec::with_capacity(events.len());
    for ev in &events {
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
    Ok(())
}

async fn build_bucket(redis_url: &str) -> Result<TokenBucket, anyhow::Error> {
    let client = redis::Client::open(redis_url)?;
    let conn = client.get_connection_manager().await?;
    Ok(TokenBucket::new(conn))
}
