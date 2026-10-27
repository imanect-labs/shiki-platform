//! ワークフロー実行時（worker/scheduler/event relay の起動時 spawn・Stage A W3）。
//!
//! `workflow.enabled` の時、本番 executor を組んで ①run ワーカー（`FOR UPDATE SKIP LOCKED` で
//! 多重起動安全）②スケジューラ（`LeaderLease` 単一リーダー・cron tick）③イベント relay
//! （outbox 配送台帳 `claim_undelivered("workflow")` → トリガの照合・wait の起床・`relay.rs`）を
//! detach タスクで走らせる。
//! RAG の `spawn_pipeline` と同型（プロセス生存中 loop・エラーは小休止して再試行）。

mod agent_port;
mod agent_tools;
mod auth;
mod data_port;
mod decide_port;
mod llm_error;
mod llm_model;
mod llm_port;
mod ports;
mod relay;
mod relay_approval;
mod relay_data;
mod relay_origin;
mod skill_port;
mod text_file;

use std::sync::Arc;

use serde::{Deserialize, Serialize};
use serde_json::Value;
use workflow_engine::{
    BucketConfig, CapabilityAudit, CapabilityNodeExecutor, EffectJournal, LeaderLease, RunStore,
    SchedulerStore, TokenBucket, WorkerConfig, WorkflowRunLauncher, WorkflowWorker,
};

pub use ports::ProdNodePorts;
pub use relay::{relay_events, retry_event_firings, RelayDeps};
pub use relay_approval::{
    expire_due_approvals, expire_due_approvals_within, DueApprovalSweep, DueSweepLimits,
};

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
    /// AI エージェント（agent.invoke）の上限と web の天井（BUG-6・D-29）。
    #[serde(default)]
    pub agent: AgentConfig,
}

/// AI エージェント（agent.invoke）の設定。ノードの設定はこれを天井に縮めるだけ。
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct AgentConfig {
    /// プロセス内で同時に走らせる agent.invoke の数（未設定は run ワーカーの並行数の半分・最低 1）。
    /// 長く走る step がワーカーを占有して、ほかのテナントの step まで止めないようにする。
    #[serde(default)]
    pub max_concurrent: Option<usize>,
    /// web.fetch が読んでよいサイトの天井（ホスト名か `*.example.com`）。未設定・空なら web.fetch は
    /// 提示しない（ノードの `egress_allowlist` はこの部分集合に限る）。`http_allowlist`（http.request の
    /// 宛先）には戻さない: 広い宛先を http.request に許した環境で、AI の web.fetch まで同じ宛先を継いで
    /// 間接プロンプトインジェクションの出口を広げないため（D-59・PIT-79）。
    #[serde(default)]
    pub web_allowlist: Option<Vec<String>>,
    /// ノードで手順の上限を省略したときの値（既定 8・最大 24）。
    #[serde(default)]
    pub default_max_steps: Option<u32>,
    /// 1 回の実行の合計トークンの天井（既定 200,000）。
    #[serde(default)]
    pub max_total_tokens: Option<u64>,
    /// 1 回の実行の費用の天井（マイクロ USD・既定 500,000 = $0.5）。
    #[serde(default)]
    pub max_cost_usd_micros: Option<i64>,
}

impl WorkflowConfig {
    /// web.fetch が読んでよいサイトの天井（`agent.web_allowlist` だけ・未設定は空＝web.fetch を提示しない）。
    ///
    /// 保存時の照合（V3）・実行時の宛先・`GET /workflows/agent-settings`（エディタ）・AI 作成の照合は、
    /// すべてこの値を使う（D-59。`http_allowlist` には戻さない）。
    #[must_use]
    pub fn agent_web_ceiling(&self) -> Vec<String> {
        self.agent
            .web_allowlist
            .clone()
            .unwrap_or_default()
            .into_iter()
            .map(|h| h.trim().to_ascii_lowercase())
            .filter(|h| workflow_engine::valid_host_pattern(h))
            .collect()
    }

    /// エンジンに渡す agent.invoke の上限。
    #[must_use]
    pub fn agent_limits(&self) -> workflow_engine::AgentLimits {
        let d = workflow_engine::AgentLimits::default();
        workflow_engine::AgentLimits {
            max_concurrent: self
                .agent
                .max_concurrent
                .unwrap_or(self.worker_concurrency / 2)
                .max(1),
            default_max_steps: self.agent.default_max_steps.unwrap_or(d.default_max_steps),
            max_total_tokens: self.agent.max_total_tokens.unwrap_or(d.max_total_tokens),
            max_cost_usd_micros: self
                .agent
                .max_cost_usd_micros
                .unwrap_or(d.max_cost_usd_micros),
            web_ceiling: self.agent_web_ceiling(),
        }
    }
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
            agent: AgentConfig::default(),
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
    /// コード実行系（agent.invoke の code.run）の隔離ティア（admin ポリシー・chat と同一の単一ソース）。
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
    /// 公開 web の検索（agent.invoke の web.search・チャットと同じ設定）。
    pub web_search: Option<Arc<dyn websearch::SearchProvider>>,
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
        web_search: deps.web_search.clone(),
    });
    let journal = EffectJournal::new(deps.db.clone());
    let audit = Arc::new(RunAudit {
        db: deps.db.clone(),
        recorder: storage::audit::AuditRecorder::new(deps.db.clone()),
    });
    let mut executor = CapabilityNodeExecutor::new(ports, journal, audit)
        .with_http_allowlist(
            deps.config.http_allowlist.clone(),
            deps.config.http_timeout_ms,
        )
        .with_agent_limits(deps.config.agent_limits());

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

    // script エンジン（プリウォーム 1 回・失敗なら script ノードは失敗する）。ミニアプリの関数と同じエンジンを
    // 共有する（ゲストのコンパイルはプロセスで 1 回）。コンパイルの完了は script を実行するノードだけが待つ
    // （起動の経路で待たない・3 回目の検証: HTTP リスナがコンパイルを待って開かなかった）。
    executor = executor.with_script_engine(
        script_runtime::EngineHandle::shared(),
        script_runtime::engine::Limits::default(),
    );
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

    // ② スケジューラ ③ イベント relay（単一リーダー・それぞれのループ・detach）。
    let leader_id = format!("wf-sched-{}", std::process::id());
    let lease = LeaderLease::new(deps.db.clone(), leader_id, deps.config.lease_secs);
    let sched = SchedulerStore::new(deps.db.clone());
    let launcher_dyn = workflow_engine::run::launcher::into_dyn(launcher);
    let tick = std::time::Duration::from_secs(deps.config.tick_secs.max(1));
    let relay_db = deps.db.clone();
    let relay_storage = Arc::clone(&deps.storage);
    let relay_data = Arc::clone(&deps.data);
    let relay_fsms = Arc::clone(&deps.fsms);
    let relay_runs = deps.runs.clone();
    let relay_sched = sched.clone();
    let relay_launcher = Arc::clone(&launcher_dyn);
    let relay_lease = lease.clone();
    let tick_runs = deps.runs.clone();
    let tick_db = deps.db.clone();
    let tick_data = Arc::clone(&deps.data);
    let tick_fsms = Arc::clone(&deps.fsms);
    let tick_concurrency = concurrency_store;
    // claim の走査の下限（アイドルの間は outbox を走査しない・3 回目の検証 3-D-1）。
    let claim_hints = storage::event::ClaimHints::default();

    // event consumer を登録し、現バックログ（有効化前の storage.write）を飛ばす。
    // 登録と「バックログの fast-forward」は原子的でなければならない（autocommit で呼ぶと登録だけ
    // 先にコミットされ、fast-forward 失敗時に relay がバックログ全件を拾って一斉発火する）。
    // pool 版が自前で txn を張る。
    if let Err(e) = storage::event::register_consumer_on_pool(&relay_db, "workflow").await {
        tracing::error!(error = %e, "workflow consumer 登録に失敗");
    }

    // ③ イベント relay は tick（cron・timer・timeout）とは別のループで回す（データ変更の照合が重くても
    // スケジュールの発火や期限の回収を遅らせない・GAP-07）。リーダーは同じリース（同じ保持者）。
    tokio::spawn(async move {
        loop {
            match relay_lease.acquire_or_renew().await {
                Ok(true) => {
                    let relay = RelayDeps {
                        db: &relay_db,
                        hints: &claim_hints,
                        sched: &relay_sched,
                        runs: &relay_runs,
                        launcher: relay_launcher.as_ref(),
                        storage: &relay_storage,
                        data: &relay_data,
                        fsms: &relay_fsms,
                    };
                    if let Err(e) = relay_events(&relay).await {
                        tracing::warn!(error = %e, "イベント relay でエラー");
                    }
                }
                Ok(false) => {} // 別インスタンスがリーダー。
                Err(e) => tracing::warn!(error = %e, "リーダーリース取得に失敗"),
            }
            tokio::time::sleep(tick).await;
        }
    });

    tokio::spawn(async move {
        loop {
            match lease.acquire_or_renew().await {
                Ok(true) => {
                    let now = chrono::Utc::now();
                    if let Err(e) = sched.tick_schedules(now, None, launcher_dyn.as_ref()).await {
                        tracing::warn!(error = %e, "スケジューラ tick でエラー");
                    }
                    // wait(timer) 起床・wait(event/timer) の timeout 回収（engine.md §5.1）。
                    if let Err(e) = tick_runs.wake_due_timers(now, None).await {
                        tracing::warn!(error = %e, "waiting_timer 起床でエラー");
                    }
                    // 承認の期限切れ（決着の監査を残す・GAP-08）。実行主体が失権した承認は期限の出口へ進めずに
                    // run を失敗させる（D-62）。確かめが一時障害で終わった承認だけを次の tick に残し、ほかの
                    // 承認（ほかのテナントの分を含む）は止めない。
                    match expire_due_approvals(
                        &tick_db, &tick_runs, &tick_data, &tick_fsms, now, None,
                    )
                    .await
                    {
                        Ok(s) if s.deferred > 0 || s.more => tracing::warn!(
                            resolved = s.resolved,
                            deferred = s.deferred,
                            more = s.more,
                            "承認の期限切れの回収で決めずに残した承認があります（次の tick で続ける）"
                        ),
                        Ok(_) => {}
                        Err(e) => tracing::warn!(error = %e, "承認の期限切れの回収でエラー"),
                    }
                    // wait(event) の timeout の回収（承認は扱わない・上の回収が滞っても毎回進める）。
                    if let Err(e) = tick_runs.expire_due_waits(now, None).await {
                        tracing::warn!(error = %e, "wait timeout 回収でエラー");
                    }
                    // ユーザーキャンセルのドレイン回収（running 完走後の terminal 化・Task 10.14）。
                    if let Err(e) = tick_runs.drain_cancel_requested(None).await {
                        tracing::warn!(error = %e, "cancel ドレインでエラー");
                    }
                    // run 多重度の promote・run timeout・並行カウンタ突合（10.5）。
                    promote_queued(&tick_runs).await;
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

/// queued run の昇格（scheduler tick・全テナント）。
///
/// promote はワークフローごとに誤りを閉じ込める（1 つの誤りでほかのテナントの昇格を止めない・ピンした IR を
/// 知っている版で読めない run は failed で外し、新しい版の IR は新しい版のリーダーに任せる・D-65）。個々の
/// 理由は engine が warn に残すので、ここでは進められなかった数を残す。
async fn promote_queued(runs: &RunStore) {
    match runs.promote_queued_runs(None).await {
        Ok(r) if r.failed > 0 || r.skipped_workflows > 0 || r.deferred_newer_ir > 0 => {
            tracing::warn!(
                promoted = r.promoted,
                failed = r.failed,
                skipped_workflows = r.skipped_workflows,
                deferred_newer_ir = r.deferred_newer_ir,
                "queued promote で進められない run・ワークフローがありました"
            );
        }
        Ok(_) => {}
        Err(e) => tracing::warn!(error = %e, "queued promote でエラー"),
    }
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

#[cfg(test)]
mod agent_config_tests {
    #![allow(clippy::unwrap_used, clippy::expect_used)]
    use super::*;
    use figment::providers::{Format, Toml};

    fn load(toml: &str) -> WorkflowConfig {
        figment::Figment::from(Toml::string(toml))
            .extract()
            .expect("toml")
    }

    /// `agent.web_allowlist` が未設定なら、http.request の許可（`http_allowlist`）を継がず、web.fetch を
    /// 提示しない（保存時の照合・実行時の宛先・エディタへ返す天井が同じ空になる・D-59）。
    #[test]
    fn web_ceiling_does_not_fall_back_to_the_http_allowlist() {
        let cfg = load(r#"http_allowlist = ["api.example.com", "*.example.org"]"#);
        assert!(cfg.agent.web_allowlist.is_none());
        assert!(cfg.agent_web_ceiling().is_empty());
        assert!(cfg.agent_limits().web_ceiling.is_empty());
        // 空で設定したときも同じ（http_allowlist には戻らない）。
        let cfg = load(
            r#"
            http_allowlist = ["api.example.com"]
            [agent]
            web_allowlist = []
            "#,
        );
        assert!(cfg.agent_web_ceiling().is_empty());
    }

    /// 天井は `agent.web_allowlist` だけから作る（小文字にそろえ、書き方の誤ったものは外す）。
    #[test]
    fn web_ceiling_comes_only_from_the_agent_web_allowlist() {
        let cfg = load(
            r#"
            http_allowlist = ["api.example.com"]
            [agent]
            web_allowlist = ["Docs.Example.com", " *.example.net ", "bad host", "https://x.example.com"]
            "#,
        );
        let expected = vec!["docs.example.com".to_string(), "*.example.net".to_string()];
        assert_eq!(cfg.agent_web_ceiling(), expected);
        assert_eq!(cfg.agent_limits().web_ceiling, expected);
    }
}
