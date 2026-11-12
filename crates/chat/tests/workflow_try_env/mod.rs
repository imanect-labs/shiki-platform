//! 会話の AI の試運転 `try_workflow` の結合テストの共通部品（`workflow_try_it`・`workflow_try_shared_it`）。
//!
//! テストごとに専用の DB と OpenFGA のストアを作る（生成ジョブ・ワークフローの step の待ち行列を、ほかのテストの
//! ワーカーと共有しない）。run の状態はテストが SQL で進める（ワークフローのワーカーは起動しない）。
#![allow(dead_code, unreachable_pub)]

use std::sync::atomic::AtomicBool;
use std::sync::Arc;
use std::time::{Duration, Instant};

use agent_core::{Tool, ToolOutcome};
use artifact::{ArtifactRole, ArtifactStore};
use authz::client::{OpenFgaClient, OpenFgaConfig};
use authz::{AuthContext, AuthzClient, Principal};
use chat::worker::slot::{ParkLimits, RunSlot};
use chat::{
    ChatStore, ChatWorker, PostResult, ThreadRole, TryScope, TryWaitLimits, TryWorkflowDeps,
    TryWorkflowTool, WorkerConfig,
};
use llm_gateway::{
    GatewayConfig, LlmGateway, ModelCatalog, ModelEntry, ProviderConfig, ProviderKind,
};
use serde_json::{json, Value};
use sqlx::{postgres::PgPoolOptions, PgPool};
use storage::ShareTarget;
use tokio::sync::Semaphore;
use tokio::task::JoinHandle;
use uuid::Uuid;
use workflow_engine::{Catalog, DelegationStore, RunStore, WorkflowRunLauncher, WorkflowStore};

/// テストの道具（持ち主 alice・editor の bob・viewer の carol・共有されていない dave）。
pub struct Env {
    pub admin_url: String,
    pub db_name: String,
    pub pool: PgPool,
    pub fga: Arc<OpenFgaClient>,
    pub tenant: String,
    pub alice: AuthContext,
    pub bob: AuthContext,
    pub carol: AuthContext,
    pub dave: AuthContext,
    pub artifacts: Arc<ArtifactStore>,
    pub workflows: Arc<WorkflowStore>,
    pub launcher: Arc<WorkflowRunLauncher>,
    pub runs: Arc<RunStore>,
    pub chat: ChatStore,
}

pub fn user_ctx(tenant: &str, user: &str) -> AuthContext {
    AuthContext::new(
        Principal {
            kind: authz::PrincipalKind::User,
            id: user.into(),
            email: None,
            groups: vec![],
            roles: vec![],
            tenant_id: Some(tenant.into()),
        },
        "acme".into(),
        authz::TenantId::parse(tenant).unwrap(),
    )
}

impl Env {
    pub async fn new() -> Option<Self> {
        let (Ok(db_url), Ok(fga_url)) = (
            shiki_testenv::var("STORAGE_TEST_DATABASE_URL"),
            shiki_testenv::var("OPENFGA_TEST_URL"),
        ) else {
            eprintln!("STORAGE_TEST_DATABASE_URL / OPENFGA_TEST_URL 未設定のためスキップ");
            return None;
        };
        let admin = PgPoolOptions::new()
            .max_connections(1)
            .connect(&db_url)
            .await
            .expect("Postgres へ接続できること");
        let db_name = format!("chat_try_{}", Uuid::new_v4().simple());
        sqlx::query(&format!("CREATE DATABASE {db_name}"))
            .execute(&admin)
            .await
            .expect("テスト用 DB を作れること");
        admin.close().await;
        let mut url = reqwest::Url::parse(&db_url).expect("DB URL");
        url.set_path(&db_name);
        let pool = PgPoolOptions::new()
            .max_connections(8)
            .connect(url.as_str())
            .await
            .expect("テスト用 DB へ接続できること");
        sqlx::migrate!("../../migrations")
            .run(&pool)
            .await
            .expect("マイグレーション適用");
        let fga = Arc::new(
            OpenFgaClient::connect(
                reqwest::Client::new(),
                &OpenFgaConfig {
                    base_url: fga_url,
                    store_name: format!("shiki-chat-try-{}", Uuid::new_v4()),
                },
                &authz::model::default_model(),
            )
            .await
            .expect("OpenFGA 接続"),
        );
        let authz: Arc<dyn AuthzClient> = fga.clone();
        let tenant = format!("t-{}", Uuid::new_v4());
        let artifacts = Arc::new(ArtifactStore::new(pool.clone(), Arc::clone(&authz)));
        let workflows = Arc::new(WorkflowStore::new(Arc::clone(&artifacts)));
        let runs = Arc::new(RunStore::new(
            pool.clone(),
            workflow_engine::DEFAULT_LEASE_SECS,
        ));
        let launcher = Arc::new(WorkflowRunLauncher::new(
            DelegationStore::new(pool.clone(), Arc::clone(&authz)),
            (*workflows).clone(),
            (*runs).clone(),
        ));
        let chat = ChatStore::connect(pool.clone(), Arc::clone(&authz), None)
            .await
            .unwrap();
        Some(Env {
            admin_url: db_url,
            db_name,
            alice: user_ctx(&tenant, "alice"),
            bob: user_ctx(&tenant, "bob"),
            carol: user_ctx(&tenant, "carol"),
            dave: user_ctx(&tenant, "dave"),
            pool,
            fga,
            tenant,
            artifacts,
            workflows,
            launcher,
            runs,
            chat,
        })
    }

    /// 片付け（テスト用 DB と OpenFGA のストア）。
    pub async fn cleanup(&self) {
        self.pool.close().await;
        let admin = PgPoolOptions::new()
            .max_connections(1)
            .connect(&self.admin_url)
            .await
            .unwrap();
        let _ = sqlx::query(&format!(
            "DROP DATABASE IF EXISTS {} WITH (FORCE)",
            self.db_name
        ))
        .execute(&admin)
        .await;
        let fga_url = shiki_testenv::var("OPENFGA_TEST_URL").unwrap();
        let _ = reqwest::Client::new()
            .delete(format!("{fga_url}/stores/{}", self.fga.store_id()))
            .send()
            .await;
    }

    /// alice がワークフローを保存し、bob に編集・carol に閲覧を共有する。
    pub async fn create_shared(&self, ir: &Value) -> Uuid {
        let (wf, _) = self
            .workflows
            .create(&self.alice, ir, &Catalog::default(), None)
            .await
            .expect("save");
        for (who, role) in [
            ("bob", ArtifactRole::Editor),
            ("carol", ArtifactRole::Viewer),
        ] {
            self.artifacts
                .share(
                    &self.alice,
                    wf,
                    &ShareTarget::User { id: who.into() },
                    role,
                    None,
                )
                .await
                .expect("share");
        }
        wf
    }

    /// 会話と、その 1 回の生成（道具を直に呼ぶので生成のジョブは消し、run は終わったことにする）。
    pub async fn conversation(&self, who: &AuthContext) -> (Uuid, Uuid) {
        let thread = self
            .chat
            .create_thread(who, "試運転", true, None, None)
            .await
            .unwrap();
        let res = self
            .chat
            .post_message(
                who,
                thread.id,
                "作ったワークフローを試して",
                &[],
                None,
                Some(true),
                false,
                &[],
                None,
            )
            .await
            .unwrap();
        sqlx::query("DELETE FROM job_queue WHERE payload->>'run_id' = $1")
            .bind(res.run_id.to_string())
            .execute(&self.pool)
            .await
            .unwrap();
        sqlx::query("UPDATE generation_run SET status = 'done' WHERE run_id = $1")
            .bind(res.run_id)
            .execute(&self.pool)
            .await
            .unwrap();
        (thread.id, res.run_id)
    }

    pub fn tool(
        &self,
        conv: (Uuid, Uuid),
        slot: &Arc<RunSlot>,
        opts: &ToolOpts,
    ) -> TryWorkflowTool {
        TryWorkflowTool::new(
            TryWorkflowDeps {
                workflows: Arc::clone(&self.workflows),
                launcher: Arc::clone(&self.launcher),
                runs: Arc::clone(&self.runs),
                chat: self.chat.clone(),
                db: self.pool.clone(),
            },
            TryScope {
                chat_run_id: conv.1,
                thread_id: conv.0,
                chat_trace_id: Some(format!("chat-{}", conv.1.simple())),
                slot: Arc::clone(slot),
                cancel: Arc::clone(&opts.cancel),
                wait: TryWaitLimits {
                    max_wait: opts.max_wait,
                    first_poll: Duration::from_millis(50),
                    max_poll: Duration::from_millis(200),
                },
            },
        )
    }

    pub async fn count(&self, sql: &str, wf: Uuid) -> i64 {
        sqlx::query_scalar(sql)
            .bind(&self.tenant)
            .bind(wf)
            .fetch_one(&self.pool)
            .await
            .unwrap()
    }

    pub async fn runs_of(&self, wf: Uuid) -> i64 {
        self.count(
            "SELECT count(*) FROM workflow_run WHERE tenant_id = $1 AND workflow_id = $2",
            wf,
        )
        .await
    }

    pub async fn ledger_of(&self, wf: Uuid) -> i64 {
        self.count(
            "SELECT count(*) FROM workflow_trial_run WHERE tenant_id = $1 AND workflow_id = $2",
            wf,
        )
        .await
    }

    /// 最後に起動した試運転の run（台帳から）。
    pub async fn last_run(&self, wf: Uuid) -> Option<Uuid> {
        sqlx::query_scalar(
            "SELECT run_id FROM workflow_trial_run WHERE tenant_id = $1 AND workflow_id = $2 \
             AND run_id IS NOT NULL ORDER BY created_at DESC, seq DESC LIMIT 1",
        )
        .bind(&self.tenant)
        .bind(wf)
        .fetch_optional(&self.pool)
        .await
        .unwrap()
        .flatten()
    }

    /// 台帳に run が載るまで待つ（道具を別のタスクで呼んでいるとき）。
    pub async fn wait_run(&self, wf: Uuid, not: Option<Uuid>) -> Uuid {
        let deadline = Instant::now() + Duration::from_secs(20);
        loop {
            if let Some(r) = self.last_run(wf).await.filter(|r| Some(*r) != not) {
                return r;
            }
            assert!(Instant::now() < deadline, "試運転の run が台帳に載らない");
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
    }

    pub async fn set_run(&self, run_id: Uuid, status: &str) {
        sqlx::query("UPDATE workflow_run SET status = $3 WHERE tenant_id = $1 AND run_id = $2")
            .bind(&self.tenant)
            .bind(run_id)
            .bind(status)
            .execute(&self.pool)
            .await
            .unwrap();
    }

    pub async fn set_steps(&self, run_id: Uuid, status: &str, output: Option<Value>) {
        sqlx::query(
            "UPDATE step_execution SET status = $3, output = $4 WHERE tenant_id = $1 AND run_id = $2",
        )
        .bind(&self.tenant)
        .bind(run_id)
        .bind(status)
        .bind(output.map(sqlx::types::Json))
        .execute(&self.pool)
        .await
        .unwrap();
    }

    pub async fn run_status(&self, run_id: Uuid) -> String {
        sqlx::query_scalar("SELECT status FROM workflow_run WHERE tenant_id = $1 AND run_id = $2")
            .bind(&self.tenant)
            .bind(run_id)
            .fetch_one(&self.pool)
            .await
            .unwrap()
    }

    /// 生成のワーカー（stub の LLM・試運転を配線）を動かす。`max_wait` は試運転を待つ上限。
    pub fn worker(&self, max_wait: Duration) -> Vec<JoinHandle<()>> {
        let worker = ChatWorker::new(
            self.pool.clone(),
            self.chat.clone(),
            chat::WorkerDeps {
                gateway: stub_gateway(self.pool.clone()),
                search: None,
                sandbox: None,
                artifacts: None,
                web_search: None,
                parser: None,
                storage: None,
                workflow_resolver: None,
                skill_artifacts: None,
                skill_catalog: None,
                workflow_store: Some(Arc::clone(&self.workflows)),
                workflow_catalog: None,
                collab: None,
                tabular: None,
                office: None,
                office_live: None,
                office_creator: None,
                office_engine: None,
            },
            WorkerConfig {
                system_prompt: "あなたはアシスタントです。".into(),
                model: Some("m".into()),
                lease_secs: 120,
                max_steps: 4,
                // stub は提示した定義から道具を探す（遅延の定義は読み込まれるまで載らない）。
                tool_search: false,
                try_wait: TryWaitLimits {
                    max_wait,
                    first_poll: Duration::from_millis(100),
                    max_poll: Duration::from_millis(200),
                },
                ..Default::default()
            },
        )
        .with_workflow_tries(Arc::clone(&self.launcher), Arc::clone(&self.runs));
        worker.spawn(2)
    }

    /// `who` が会話に話しかける（エージェントの扱い）。
    pub async fn post(&self, who: &AuthContext, thread_id: Uuid, text: &str) -> PostResult {
        self.chat
            .post_message(
                who,
                thread_id,
                text,
                &[],
                None,
                Some(true),
                false,
                &[],
                None,
            )
            .await
            .unwrap()
    }

    /// 生成が承認待ちになるのを待って、`who` が承認する（ツール呼び出しの id を返す）。
    pub async fn approve(&self, who: &AuthContext, thread_id: Uuid, run_id: Uuid) -> String {
        let call_id = wait_approval(&self.pool, run_id).await;
        assert!(self
            .chat
            .submit_approval(who, thread_id, run_id, &call_id, "try_workflow", true, None)
            .await
            .unwrap());
        call_id
    }

    /// alice の会話を利用者 `id` に共有する。
    pub async fn share(&self, thread_id: Uuid, id: &str, role: ThreadRole) {
        self.chat
            .share_thread(
                &self.alice,
                thread_id,
                &ShareTarget::User { id: id.into() },
                role,
                None,
            )
            .await
            .expect("share_thread");
    }

    /// alice の会話の、利用者 `id` への共有を外す。
    pub async fn unshare(&self, thread_id: Uuid, id: &str, role: ThreadRole) {
        self.chat
            .unshare_thread(
                &self.alice,
                thread_id,
                &ShareTarget::User { id: id.into() },
                role,
                None,
            )
            .await
            .expect("unshare_thread");
    }

    /// 会話に保存された tool_result の本文（作成順）。
    pub async fn tool_results(&self, thread_id: Uuid) -> Vec<String> {
        sqlx::query_scalar(
            "SELECT b->>'content' FROM message m, jsonb_array_elements(m.content) b \
             WHERE m.tenant_id = $1 AND m.thread_id = $2 AND b->>'type' = 'tool_result' \
             ORDER BY m.created_at, m.id",
        )
        .bind(&self.tenant)
        .bind(thread_id)
        .fetch_all(&self.pool)
        .await
        .unwrap()
    }

    /// メッセージの本文（text のブロックを改行でつないだもの）。
    pub async fn answer_text(&self, message_id: Uuid) -> String {
        let texts: Vec<String> = sqlx::query_scalar(
            "SELECT b->>'text' FROM message m, jsonb_array_elements(m.content) WITH ORDINALITY AS e(b, i) \
             WHERE m.tenant_id = $1 AND m.id = $2 AND b->>'type' = 'text' ORDER BY i",
        )
        .bind(&self.tenant)
        .bind(message_id)
        .fetch_all(&self.pool)
        .await
        .unwrap();
        texts.join("\n")
    }
}

/// 道具を組むときの設定（停止の旗・待つ上限）。
pub struct ToolOpts {
    pub cancel: Arc<AtomicBool>,
    pub max_wait: Duration,
}

impl ToolOpts {
    pub fn waiting(max_wait: Duration) -> Self {
        ToolOpts {
            cancel: Arc::new(AtomicBool::new(false)),
            max_wait,
        }
    }
}

/// 枠が `n` の生成の枠（1 つ取った状態）。
pub fn slot_of(n: usize, limits: Arc<ParkLimits>) -> (Arc<Semaphore>, Arc<RunSlot>) {
    let sem = Arc::new(Semaphore::new(n));
    let permit = Arc::clone(&sem).try_acquire_owned().unwrap();
    let slot = RunSlot::new(Arc::clone(&sem), limits, permit);
    (sem, slot)
}

/// AI に聞くブロック 1 つ（保存の検証を通る最小の IR）。`policies` を渡すと多重度の設定を付ける。
pub fn ask_ir(policies: Option<Value>) -> Value {
    let mut ir = json!({
        "ir_version": 1,
        "name": format!("try-{}", Uuid::new_v4().simple()),
        "display_name": "経費の確認",
        "nodes": [ { "id": "ask", "type": "llm.invoke", "label": "まとめる", "params": { "prompt": "まとめて" } } ],
        "edges": []
    });
    if let Some(p) = policies {
        ir["policies"] = p;
    }
    ir
}

/// テストの本体を走らせ、途中の assert が落ちても片付けてから落とす。
pub async fn run<F, Fut>(body: F)
where
    F: FnOnce(Arc<Env>) -> Fut,
    Fut: std::future::Future<Output = ()> + Send + 'static,
{
    let Some(env) = Env::new().await else { return };
    let env = Arc::new(env);
    let outcome = tokio::spawn(body(Arc::clone(&env))).await;
    env.cleanup().await;
    if let Err(e) = outcome {
        std::panic::resume_unwind(e.into_panic());
    }
}

pub async fn call(tool: &TryWorkflowTool, ctx: &AuthContext, input: Value) -> ToolOutcome {
    tool.call(ctx, input, Some("trace-try"))
        .await
        .expect("道具が返る")
}

pub fn stub_gateway(pool: PgPool) -> LlmGateway {
    let config = GatewayConfig {
        provider: ProviderConfig {
            kind: ProviderKind::Stub,
            ..Default::default()
        },
        catalog: ModelCatalog {
            default_model: "m".into(),
            models: vec![ModelEntry {
                id: "m".into(),
                ..Default::default()
            }],
        },
        ..Default::default()
    };
    LlmGateway::build(pool, reqwest::Client::new(), config).expect("gateway")
}

/// 生成の run の (状態, 承認待ちのツール呼び出し id)。
pub async fn chat_run(pool: &PgPool, run_id: Uuid) -> (String, Option<String>) {
    sqlx::query_as(
        "SELECT status, pending_approval->>'tool_call_id' FROM generation_run WHERE run_id = $1",
    )
    .bind(run_id)
    .fetch_one(pool)
    .await
    .unwrap()
}

/// 生成の run が承認待ちになるまで待ち、ツール呼び出しの id を返す。
pub async fn wait_approval(pool: &PgPool, run_id: Uuid) -> String {
    let deadline = Instant::now() + Duration::from_secs(60);
    loop {
        let (status, id) = chat_run(pool, run_id).await;
        if let (true, Some(id)) = (status == "waiting_approval", id) {
            return id;
        }
        assert!(Instant::now() < deadline, "承認待ちに入らない（{status}）");
        tokio::time::sleep(Duration::from_millis(200)).await;
    }
}

pub async fn wait_done(pool: &PgPool, run_id: Uuid) {
    let deadline = Instant::now() + Duration::from_secs(60);
    loop {
        let (status, _) = chat_run(pool, run_id).await;
        if status == "done" {
            return;
        }
        assert!(Instant::now() < deadline, "生成が終わらない（{status}）");
        tokio::time::sleep(Duration::from_millis(200)).await;
    }
}
