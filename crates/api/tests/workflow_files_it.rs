//! ファイルを扱うワークフローの結合テスト（実 Postgres ＋実 OpenFGA ＋ RustFS ＋隔離 DuckDB ランナー）。
//!
//! - **BUG-8**: ファイル保存トリガ（event storage.write）の run 入力に、保存されたファイルの id・版・操作・
//!   書込主体が載り、`{"$from":"trigger","path":"/file_id"}` で storage.read / csv.query が読める。
//!   削除とフォルダの作成では発火しない。wait(event) の起床にも同じ形が渡る。イベントは本番と同じ
//!   `api::workflow_runtime::relay_events` で配り、run は本番の executor（`ProdNodePorts`）で流す。
//! - wait(event) は、run の実行主体が保存されたファイルを読めるときだけ起きる。scope の無い購読でも、
//!   他人の読めないファイルの保存では起きず、その id・書いた人は実行履歴に載らない。
//! - **BUG-10**: 同意計画が csv.* の対象ファイルを提案し、その委譲で schedule の run（workflow
//!   プリンシパル）が csv.query / csv.patch できる。委譲が無ければファイルが見えず（not_found）失敗する。
//! - **BUG-28**: 新しい版の保存（presigned の finalize・内容の更新・提案の採用）でも、フォルダに束縛した
//!   トリガと wait(event) が起きる。同じファイルの続く保存は 1 回にまとめ、最後の版で起動する（D-38）。
//! - **BUG-29**: event の run が束縛フォルダへ書いても（storage.write・csv.patch・workflow.start の子の書込）、
//!   その保存で自分を起こさない（因果の鎖・D-26）。
//! - **BUG-39**: 委譲を外した後は、保存されたファイルを workflow プリンシパルが読めないので発火しない（D-50）。
//!
//! relay はテナントを問わず未配送のイベントを掴む。別の OpenFGA ストアを持つテストが互いのイベントを
//! 掴むと、閲覧の確認（トリガ・wait）と委譲の確認が別のストアに問うことになり起動しないので、このファイルの
//! テストは直列に流す。
//!
//! `STORAGE_TEST_DATABASE_URL` と `OPENFGA_TEST_URL` が無いとスキップする。csv.* の部分は隔離ランナー
//! （`SHIKI_TABULAR_RUNNER` か `crates/tabular/runner` のビルド成果物）が無いとスキップする。

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::print_stderr,
    clippy::pedantic,
    clippy::too_many_lines
)]

use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use authz::client::{OpenFgaClient, OpenFgaConfig};
use authz::{AuthContext, AuthzClient, Principal, PrincipalKind, Relation};
use chrono::Utc;
use serde_json::{json, Value};
use sqlx::postgres::PgPoolOptions;
use uuid::Uuid;
use workflow_engine::{
    CapabilityNodeExecutor, Catalog, DelegationStore, EffectJournal, GrantRequest, NodeExecutor,
    RegistrationService, RunStatus, RunStore, SchedulerStore, StepStatus, WorkerConfig, WorkflowIr,
    WorkflowRunLauncher, WorkflowStore,
};

/// このファイルのテストを直列にする（上のモジュールの説明）。
static SERIAL: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

struct NoopAudit;
impl workflow_engine::CapabilityAudit for NoopAudit {
    fn record(&self, _t: &str, _a: &str, _ok: bool, _m: &Value) {}
}

struct Env {
    pool: sqlx::PgPool,
    fga: Arc<dyn AuthzClient>,
    storage: Arc<storage::StorageService>,
    /// relay のデータ変更の経路（このテストでは使わない・relay の依存として渡す）。
    data: Arc<data::DataStore>,
    fsms: Arc<data::FsmStore>,
    workflows: WorkflowStore,
    runs: RunStore,
    launcher: WorkflowRunLauncher,
    registration: RegistrationService,
    executor: Arc<dyn NodeExecutor>,
    /// 隔離ランナーがあり、csv.* を実行できるか。
    tabular: bool,
    tenant: String,
    alice: AuthContext,
}

/// 参照先を確かめない resolver（このテストはテーブルを作らない）。
struct NoRefs;

#[async_trait::async_trait]
impl data::RefResolver for NoRefs {
    async fn user_exists(&self, _: &AuthContext, _: &str) -> Result<bool, String> {
        Ok(false)
    }
    async fn role_exists(&self, _: &AuthContext, _: &str) -> Result<bool, String> {
        Ok(false)
    }
    async fn file_readable(&self, _: &AuthContext, _: Uuid) -> Result<bool, String> {
        Ok(false)
    }
}

/// 隔離ランナーの実体（`SHIKI_TABULAR_RUNNER` か除外クレートの既定ビルド先）。
fn runner_path() -> Option<PathBuf> {
    if let Ok(p) = std::env::var("SHIKI_TABULAR_RUNNER") {
        let p = PathBuf::from(p);
        if p.exists() {
            return Some(p);
        }
    }
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .parent()?
        .parent()?
        .to_path_buf();
    ["release", "debug"]
        .iter()
        .map(|profile| {
            root.join("crates/tabular/runner/target")
                .join(profile)
                .join("shiki-tabular-runner")
        })
        .find(|p| p.exists())
}

async fn setup() -> Option<Env> {
    let (Ok(db), Ok(fga_url)) = (
        shiki_testenv::var("STORAGE_TEST_DATABASE_URL"),
        shiki_testenv::var("OPENFGA_TEST_URL"),
    ) else {
        eprintln!("STORAGE_TEST_DATABASE_URL / OPENFGA_TEST_URL 未設定のためスキップ");
        return None;
    };
    let s3_endpoint = std::env::var("STORAGE_TEST_S3_ENDPOINT")
        .unwrap_or_else(|_| "http://localhost:9000".into());
    let pool = PgPoolOptions::new()
        .max_connections(8)
        .connect(&db)
        .await
        .expect("pg");
    sqlx::migrate!("../../migrations")
        .run(&pool)
        .await
        .expect("migrate");
    let http = reqwest::Client::new();
    let fga = Arc::new(
        OpenFgaClient::connect(
            http.clone(),
            &OpenFgaConfig {
                base_url: fga_url,
                store_name: format!("shiki-wf-files-{}", Uuid::new_v4()),
            },
            &authz::model::default_model(),
        )
        .await
        .expect("fga"),
    ) as Arc<dyn AuthzClient>;
    let s3 = storage::S3Config {
        internal_endpoint: s3_endpoint.clone(),
        public_endpoint: s3_endpoint,
        bucket: "shiki-it-blobs".into(),
        access_key: std::env::var("STORAGE_TEST_S3_ACCESS_KEY")
            .unwrap_or_else(|_| "minioadmin".into()),
        secret_key: std::env::var("STORAGE_TEST_S3_SECRET_KEY")
            .unwrap_or_else(|_| "minioadmin".into()),
        region: "us-east-1".into(),
        presign_get_ttl_secs: 300,
        presign_put_ttl_secs: 900,
        cors_allowed_origins: vec![],
    };
    let object_store: Arc<dyn storage::ObjectStore> = Arc::new(storage::S3ObjectStore::new(&s3));
    object_store.ensure_bucket().await.expect("bucket");
    let storage = Arc::new(storage::StorageService::new(
        pool.clone(),
        object_store,
        Arc::clone(&fga),
        Duration::from_secs(300),
        Duration::from_secs(900),
        5 * 1024 * 1024 * 1024,
    ));
    let gateway = Arc::new(
        llm_gateway::LlmGateway::build(
            pool.clone(),
            http,
            llm_gateway::GatewayConfig {
                provider: llm_gateway::ProviderConfig {
                    kind: llm_gateway::ProviderKind::Stub,
                    ..Default::default()
                },
                catalog: llm_gateway::ModelCatalog {
                    default_model: "stub".into(),
                    models: vec![llm_gateway::ModelEntry {
                        id: "stub".into(),
                        ..Default::default()
                    }],
                },
                ..Default::default()
            },
        )
        .expect("gateway"),
    );

    let tenant = format!("t-{}", Uuid::new_v4().simple());
    let alice = AuthContext::new(
        Principal {
            kind: PrincipalKind::User,
            id: "alice".into(),
            email: None,
            groups: vec![],
            roles: vec![],
            tenant_id: Some(tenant.clone()),
        },
        tenant.clone(),
        authz::TenantId::parse(tenant.clone()).unwrap(),
    );
    // alice はドライブのルートにファイル・フォルダを作れる（member@org）。
    fga.write_tuple(
        &alice.subject(),
        Relation::Member,
        &alice.ns().organization(alice.org()),
    )
    .await
    .expect("member tuple");

    let runner = runner_path();
    if runner.is_none() {
        shiki_testenv::missing(
            "SHIKI_TABULAR_RUNNER",
            "shiki-tabular-runner 未ビルドのため csv.* の確認",
        );
    }
    let tabular = runner.map(|r| {
        Arc::new(tabular::TabularService::new(
            Arc::clone(&storage),
            tabular::RunnerConfig::new(r.to_string_lossy().to_string(), Duration::from_secs(20)),
            tabular::Quotas::default(),
        ))
    });
    let artifacts = Arc::new(artifact::ArtifactStore::new(pool.clone(), Arc::clone(&fga)));
    let data = Arc::new(data::DataStore::new(
        pool.clone(),
        Arc::clone(&fga),
        Arc::new(NoRefs),
    ));
    let fsms = Arc::new(data::FsmStore::new(Arc::clone(&artifacts), (*data).clone()));
    let workflows = WorkflowStore::new(Arc::clone(&artifacts));
    let runs = RunStore::new(pool.clone(), workflow_engine::DEFAULT_LEASE_SECS);
    let delegation = DelegationStore::new(pool.clone(), Arc::clone(&fga));
    let launcher = WorkflowRunLauncher::new(delegation.clone(), workflows.clone(), runs.clone());
    let registration = RegistrationService::new(pool.clone(), delegation);
    let has_tabular = tabular.is_some();
    let ports = Arc::new(api::workflow_runtime::ProdNodePorts {
        storage: Arc::clone(&storage),
        search: None,
        gateway,
        sandbox: None,
        sandbox_backend: sandbox_client::SandboxBackend::Wasm,
        secrets: None,
        tabular,
        skill_artifacts: None,
        launcher: launcher.clone(),
        http: api::workflow_runtime::HttpEgress::default(),
        db: pool.clone(),
        data: None,
        fsms: None,
        web_search: None,
    });
    let executor: Arc<dyn NodeExecutor> = Arc::new(CapabilityNodeExecutor::new(
        ports,
        EffectJournal::new(pool.clone()),
        Arc::new(NoopAudit),
    ));
    Some(Env {
        pool,
        fga,
        storage,
        data,
        fsms,
        workflows,
        runs,
        launcher,
        registration,
        executor,
        tabular: has_tabular,
        tenant,
        alice,
    })
}

impl Env {
    fn worker(&self) -> workflow_engine::WorkflowWorker {
        workflow_engine::WorkflowWorker::new(
            self.runs.clone(),
            Arc::clone(&self.executor),
            WorkerConfig::default(),
        )
        .scoped_to_tenant(&self.tenant)
    }

    /// 本番と同じ relay で outbox を配り、ワーカーで捌く。`done` が真になるまで繰り返す。
    async fn relay_until<F, Fut>(&self, what: &str, done: F)
    where
        F: Fn() -> Fut,
        Fut: std::future::Future<Output = bool>,
    {
        let sched = SchedulerStore::new(self.pool.clone());
        let worker = self.worker();
        let deadline = std::time::Instant::now() + Duration::from_secs(30);
        loop {
            api::workflow_runtime::relay_events(&api::workflow_runtime::RelayDeps {
                db: &self.pool,
                hints: &storage::event::ClaimHints::default(),
                sched: &sched,
                runs: &self.runs,
                launcher: &self.launcher,
                storage: &self.storage,
                data: &self.data,
                fsms: &self.fsms,
            })
            .await
            .expect("relay");
            while worker.claim_and_run_once("w1").await.unwrap() {}
            if done().await {
                return;
            }
            assert!(std::time::Instant::now() < deadline, "{what} にならない");
            tokio::time::sleep(Duration::from_millis(200)).await;
        }
    }

    /// 同じテナント・組織の別の利用者（ドライブのルートにフォルダを作れる）。
    async fn user(&self, id: &str) -> AuthContext {
        let ctx = AuthContext::new(
            Principal {
                kind: PrincipalKind::User,
                id: id.into(),
                email: None,
                groups: vec![],
                roles: vec![],
                tenant_id: Some(self.tenant.clone()),
            },
            self.tenant.clone(),
            authz::TenantId::parse(self.tenant.clone()).unwrap(),
        );
        self.fga
            .write_tuple(
                &ctx.subject(),
                Relation::Member,
                &ctx.ns().organization(ctx.org()),
            )
            .await
            .expect("member tuple");
        ctx
    }

    /// ワークフローの run（作成順）: (run_id, 入力, 状態)。
    async fn runs_of(&self, wf: Uuid) -> Vec<(Uuid, Value, String)> {
        sqlx::query_as::<_, (Uuid, sqlx::types::Json<Value>, String)>(
            "SELECT run_id, input, status FROM workflow_run \
             WHERE tenant_id = $1 AND workflow_id = $2 ORDER BY created_at",
        )
        .bind(&self.tenant)
        .bind(wf)
        .fetch_all(&self.pool)
        .await
        .unwrap()
        .into_iter()
        .map(|(id, input, status)| (id, input.0, status))
        .collect()
    }

    async fn step(&self, run: Uuid, node: &str) -> (String, Option<Value>, Option<Value>) {
        sqlx::query_as(
            "SELECT status, output, error FROM step_execution \
             WHERE tenant_id = $1 AND run_id = $2 AND node_id = $3",
        )
        .bind(&self.tenant)
        .bind(run)
        .bind(node)
        .fetch_one(&self.pool)
        .await
        .unwrap()
    }

    /// 自分のテナントで、その操作の outbox イベントが workflow へ配送済みか。
    async fn delivered(&self, op: &str, node_id: Uuid) -> bool {
        sqlx::query_scalar(
            "SELECT EXISTS (SELECT 1 FROM storage_event_outbox o \
               JOIN outbox_delivery d ON d.event_id = o.id AND d.consumer = 'workflow' \
               WHERE o.tenant_id = $1 AND o.op = $2 AND o.node_id = $3)",
        )
        .bind(&self.tenant)
        .bind(op)
        .bind(node_id)
        .fetch_one(&self.pool)
        .await
        .unwrap()
    }

    /// 新しい版の保存をまとめて待っている発火を、時刻を進めて起こし、ワーカーで捌く（D-38）。
    ///
    /// 起動は本番の relay と同じ口（`retry_event_firings`）を通す（起動の前に workflow プリンシパルの閲覧を
    /// 確かめ直す・BUG-50）。
    async fn fire_coalesced(&self) {
        let sched = SchedulerStore::new(self.pool.clone());
        api::workflow_runtime::retry_event_firings(
            &api::workflow_runtime::RelayDeps {
                db: &self.pool,
                hints: &storage::event::ClaimHints::default(),
                sched: &sched,
                runs: &self.runs,
                launcher: &self.launcher,
                storage: &self.storage,
                data: &self.data,
                fsms: &self.fsms,
            },
            Utc::now()
                + chrono::Duration::seconds(workflow_engine::scheduler::COALESCE_QUIET_SECS + 1),
            Some(&self.tenant),
        )
        .await
        .unwrap();
        let worker = self.worker();
        while worker.claim_and_run_once("w1").await.unwrap() {}
    }

    /// このテストのテナントに作った run・発火・登録を消す（同じ DB を使うほかのテストや手元のサーバの tick に
    /// 残さない・BUG-63）。run を消せば step・wait_subscription は FK で消え、登録を消せばトリガも消える。
    async fn cleanup(&self) {
        for sql in [
            "DELETE FROM trigger_firing WHERE tenant_id = $1",
            "DELETE FROM workflow_run WHERE tenant_id = $1",
            "DELETE FROM workflow_registration WHERE tenant_id = $1",
        ] {
            sqlx::query(sql)
                .bind(&self.tenant)
                .execute(&self.pool)
                .await
                .unwrap();
        }
    }

    /// 利用者のアップロードで既存ファイルの新しい版を保存する（declare → presigned PUT → finalize）。
    async fn upload_new_version(&self, ctx: &AuthContext, target: Uuid, content: &[u8]) -> i64 {
        let sha = storage::content_address::sha256_hex(content);
        let ticket = self
            .storage
            .begin_upload(
                ctx,
                None,
                "",
                "text/plain",
                &sha,
                content.len() as i64,
                Some(target),
                None,
            )
            .await
            .unwrap();
        let put = reqwest::Client::new()
            .put(&ticket.upload_url)
            .body(content.to_vec())
            .send()
            .await
            .expect("presigned PUT");
        assert!(put.status().is_success(), "PUT: {}", put.status());
        self.storage
            .finalize_upload(ctx, ticket.upload_id, None)
            .await
            .unwrap()
            .version
    }

    /// 理由ごとの、トリガの抑止の監査の件数。
    async fn suppressed(&self, reason: &str) -> i64 {
        sqlx::query_scalar(
            "SELECT count(*) FROM audit_log WHERE tenant_id = $1 \
               AND action = 'workflow.trigger.suppressed' AND metadata->>'reason' = $2",
        )
        .bind(&self.tenant)
        .bind(reason)
        .fetch_one(&self.pool)
        .await
        .unwrap()
    }

    /// 同意計画の提案のうち対象が確定したものを、有効化の委譲にする（同意画面で全部を選んだのと同じ）。
    fn grants_from_plan(&self, ir: &WorkflowIr) -> Vec<GrantRequest> {
        RegistrationService::consent_plan(ir)
            .into_iter()
            .filter(|g| !g.needs_user_pick)
            .map(|g| {
                let id = g.object_id.expect("確定した対象");
                let object = match g.object_kind.as_str() {
                    "file" => self.alice.ns().file(&id),
                    "folder" => self.alice.ns().folder(&id),
                    other => panic!("想定外の対象: {other}"),
                };
                GrantRequest {
                    scope: g.scope,
                    object,
                    relation: Relation::parse(&g.relation).expect("relation"),
                }
            })
            .collect()
    }

    /// スケジュールを 1 回発火させ、ワーカーで完走させる。作られた run の id を返す。
    async fn fire_schedule_and_run(&self, wf: Uuid, at_secs: i64) -> Uuid {
        let sched = SchedulerStore::new(self.pool.clone());
        let fired = sched
            .tick_schedules(
                Utc::now() + chrono::Duration::seconds(at_secs),
                Some(&self.tenant),
                &self.launcher,
            )
            .await
            .unwrap();
        assert!(fired >= 1, "schedule が発火する（fired={fired}）");
        let worker = self.worker();
        while worker.claim_and_run_once("w1").await.unwrap() {}
        let runs = self.runs_of(wf).await;
        runs.last().expect("run が作られる").0
    }
}

const ORDERS_CSV: &[u8] = b"item,qty\nA,1\nB,2\nC,3\n";

/// ファイル保存トリガの run は、保存されたファイルを `$from trigger /file_id` で読める（BUG-8）。
/// 削除・フォルダの作成では発火しない。
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn file_save_trigger_hands_the_saved_file_to_the_run() {
    let _serial = SERIAL.lock().await;
    let Some(env) = setup().await else { return };
    let inbox = env
        .storage
        .create_folder(&env.alice, None, "受付", None)
        .await
        .unwrap();

    let file_ref = json!({ "$from": "trigger", "path": "/file_id" });
    let mut nodes =
        vec![json!({ "id": "rd", "type": "storage.read", "params": { "file": file_ref } })];
    let mut edges = vec![];
    let mut scopes = vec!["storage.read"];
    if env.tabular {
        nodes.push(json!({ "id": "q", "type": "csv.query", "params": {
            "file": file_ref, "sql": "SELECT count(*) AS n FROM data" } }));
        edges.push(json!({ "from": "rd", "to": "q" }));
        scopes.push("csv.read");
    }
    let ir = json!({
        "ir_version": 1, "name": "inbox-csv",
        "declared_scopes": scopes,
        "triggers": [ { "kind": "event", "source": "storage.write",
                        "scope": { "folder": inbox.id.to_string() } } ],
        "nodes": nodes, "edges": edges
    });
    let (wf, _) = env
        .workflows
        .create(&env.alice, &ir, &Catalog::default(), None)
        .await
        .expect("保存");
    let parsed = WorkflowIr::from_json(&ir).unwrap();
    // 委譲はトリガのフォルダの閲覧だけ（ファイルはトリガで決まるので選ばない）。配下のファイルは
    // フォルダの viewer で読める。
    let grants = env.grants_from_plan(&parsed);
    assert_eq!(grants.len(), 1, "{grants:?}");
    assert_eq!(
        grants[0].object,
        env.alice.ns().folder(&inbox.id.to_string())
    );
    env.registration
        .enable(&env.alice, wf, 1, &parsed, &grants)
        .await
        .expect("enable");

    let file = env
        .storage
        .write_file_internal(
            &env.alice,
            Some(inbox.id),
            "orders.csv",
            ORDERS_CSV,
            "text/csv",
            None,
        )
        .await
        .unwrap();
    let e = &env;
    env.relay_until("保存イベントの run の完了", || async move {
        e.runs_of(wf)
            .await
            .first()
            .is_some_and(|(_, _, s)| s == "succeeded" || s == "failed")
    })
    .await;

    let runs = env.runs_of(wf).await;
    assert_eq!(runs.len(), 1, "保存 1 回で run 1 件: {runs:?}");
    let (run, input, status) = &runs[0];
    // run 入力（$from trigger）に、保存されたファイルの id・版・操作・書込主体が載る。
    assert_eq!(input["file_id"], file.id.to_string());
    assert_eq!(input["version"], file.version);
    assert_eq!(input["op"], "create");
    assert_eq!(input["kind"], "file");
    assert_eq!(input["parent_id"], inbox.id.to_string());
    assert_eq!(input["size"], ORDERS_CSV.len());
    assert_eq!(input["actor"], "alice");
    assert_eq!(status, "succeeded", "rd: {:?}", env.step(*run, "rd").await);
    let (_, rd, _) = env.step(*run, "rd").await;
    let rd = rd.unwrap();
    assert_eq!(rd["id"], file.id.to_string());
    assert_eq!(rd["name"], "orders.csv");
    assert_eq!(rd["text"], String::from_utf8_lossy(ORDERS_CSV).as_ref());
    if env.tabular {
        // 委譲元のフォルダ経由で、workflow プリンシパルが CSV を集計できる。
        let (_, q, _) = env.step(*run, "q").await;
        assert_eq!(q.unwrap()["rows"], json!([["3"]]));
    }

    // 削除とフォルダの作成は「ファイルが保存されたとき」ではない（配送はされるが発火しない）。
    env.storage
        .soft_delete_file(&env.alice, file.id, None)
        .await
        .unwrap();
    let sub = env
        .storage
        .create_folder(&env.alice, Some(inbox.id), "sub", None)
        .await
        .unwrap();
    let (file_id, sub_id) = (file.id, sub.id);
    env.relay_until("削除・フォルダ作成の配送", || async move {
        e.delivered("delete", file_id).await && e.delivered("create", sub_id).await
    })
    .await;
    assert_eq!(
        env.runs_of(wf).await.len(),
        1,
        "削除・フォルダの作成では起動しない"
    );
}

/// wait(event) の起床にも、保存されたファイルの id が届く（同じペイロード・BUG-8）。
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn wait_event_receives_the_saved_file() {
    let _serial = SERIAL.lock().await;
    let Some(env) = setup().await else { return };
    let inbox = env
        .storage
        .create_folder(&env.alice, None, "提出", None)
        .await
        .unwrap();
    let ir = json!({
        "ir_version": 1, "name": "wait-for-upload",
        "declared_scopes": ["storage.read"],
        "triggers": [ { "kind": "interactive" } ],
        "nodes": [
            { "id": "w", "type": "control.wait", "params": {
                "kind": "event", "source": "storage.write",
                "scope": { "folder": inbox.id.to_string() }, "timeout_sec": 600 } },
            { "id": "rd", "type": "storage.read", "params": {
                "file": { "$from": "nodes.w.output", "path": "/file_id" } } }
        ],
        "edges": [ { "from": "w", "to": "rd" } ]
    });
    let (wf, _) = env
        .workflows
        .create(&env.alice, &ir, &Catalog::default(), None)
        .await
        .expect("保存");
    let run = env
        .launcher
        .start_interactive(&env.alice, wf, &json!({}))
        .await
        .unwrap()
        .unwrap();
    let worker = env.worker();
    while worker.claim_and_run_once("w1").await.unwrap() {}
    let steps: std::collections::HashMap<String, StepStatus> = env
        .runs
        .step_statuses(&env.tenant, run)
        .await
        .unwrap()
        .into_iter()
        .collect();
    assert_eq!(steps.get("w"), Some(&StepStatus::WaitingEvent));

    let file = env
        .storage
        .write_file_internal(
            &env.alice,
            Some(inbox.id),
            "report.txt",
            b"done",
            "text/plain",
            None,
        )
        .await
        .unwrap();
    let e = &env;
    env.relay_until("wait の起床と run の完了", || async move {
        e.runs
            .run_status(&e.tenant, run)
            .await
            .unwrap()
            .is_some_and(|s| s != RunStatus::Running)
    })
    .await;
    assert_eq!(
        env.runs.run_status(&env.tenant, run).await.unwrap(),
        Some(RunStatus::Succeeded),
        "rd: {:?}",
        env.step(run, "rd").await
    );
    let (_, w, _) = env.step(run, "w").await;
    let w = w.unwrap();
    assert_eq!(w["file_id"], file.id.to_string());
    assert_eq!(w["op"], "create");
    assert_eq!(w["actor"], "alice");
    let (_, rd, _) = env.step(run, "rd").await;
    assert_eq!(rd.unwrap()["text"], "done");
}

/// scope の無い wait(event) でも、run の実行主体（alice）が読めない他人のファイルの保存では起きない。
/// そのファイルの id・書いた人は実行履歴に載らず、alice が読めるファイルの保存で初めて起きる。
///
/// 新しく保存する wait(event) は scope が必須になった（D-49）。ここでは scope の無い保存済みの定義の
/// run（D-49 より前に保存したもの）を、run のピンした IR の scope を空にして再現する。
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn wait_event_without_scope_ignores_files_the_principal_cannot_read() {
    let _serial = SERIAL.lock().await;
    let Some(env) = setup().await else { return };
    let bob = env.user("bob").await;
    let somewhere = env
        .storage
        .create_folder(&env.alice, None, "somewhere", None)
        .await
        .unwrap();
    let ir = json!({
        "ir_version": 1, "name": "wait-any-upload",
        "declared_scopes": ["storage.read"],
        "triggers": [ { "kind": "interactive" } ],
        "nodes": [
            { "id": "w", "type": "control.wait", "params": {
                "kind": "event", "source": "storage.write",
                "scope": { "folder": somewhere.id.to_string() }, "timeout_sec": 600 } }
        ],
        "edges": []
    });
    // scope の無い wait は新しくは保存できない（D-49）。
    let mut legacy = ir.clone();
    legacy["nodes"][0]["params"]["scope"] = json!({});
    let rejected = env
        .workflows
        .create(&env.alice, &legacy, &Catalog::default(), None)
        .await;
    assert!(rejected.is_err(), "scope の無い wait(event) は保存できない");
    let (wf, _) = env
        .workflows
        .create(&env.alice, &ir, &Catalog::default(), None)
        .await
        .expect("保存");
    let run = env
        .launcher
        .start_interactive(&env.alice, wf, &json!({}))
        .await
        .unwrap()
        .unwrap();
    sqlx::query(
        "UPDATE workflow_run SET ir_snapshot = jsonb_set(ir_snapshot, '{nodes,0,params,scope}', '{}') \
         WHERE tenant_id = $1 AND run_id = $2",
    )
    .bind(&env.tenant)
    .bind(run)
    .execute(&env.pool)
    .await
    .unwrap();
    let worker = env.worker();
    while worker.claim_and_run_once("w1").await.unwrap() {}
    let e = &env;
    let w_status = move || async move {
        e.runs
            .step_statuses(&e.tenant, run)
            .await
            .unwrap()
            .into_iter()
            .find(|(p, _)| p == "w")
            .map(|(_, s)| s)
    };
    assert_eq!(w_status().await, Some(StepStatus::WaitingEvent));

    // bob が自分だけのフォルダに保存する（alice は読めない）。
    let private = env
        .storage
        .create_folder(&bob, None, "bob-private", None)
        .await
        .unwrap();
    let hidden = env
        .storage
        .write_file_internal(&bob, Some(private.id), "salary.csv", b"x", "text/csv", None)
        .await
        .unwrap();
    let hidden_id = hidden.id;
    env.relay_until("bob の保存の配送", || async move {
        e.delivered("create", hidden_id).await
    })
    .await;
    assert_eq!(
        w_status().await,
        Some(StepStatus::WaitingEvent),
        "読めないファイルの保存では起きない"
    );
    let (_, out, _) = env.step(run, "w").await;
    assert_eq!(out, None, "読めないファイルの id・書いた人を出力に載せない");
    let leaked: bool = sqlx::query_scalar(
        "SELECT EXISTS (SELECT 1 FROM run_event WHERE tenant_id = $1 AND run_id = $2            AND payload::text LIKE '%' || $3 || '%')",
    )
    .bind(&env.tenant)
    .bind(run)
    .bind(hidden.id.to_string())
    .fetch_one(&env.pool)
    .await
    .unwrap();
    assert!(!leaked, "実行の記録にも読めないファイルの id が残らない");

    // alice が読めるファイルの保存で起き、そのファイルが渡る。
    let mine = env
        .storage
        .write_file_internal(&env.alice, None, "memo.txt", b"hi", "text/plain", None)
        .await
        .unwrap();
    env.relay_until("alice の保存で wait が起きる", || async move {
        e.runs
            .run_status(&e.tenant, run)
            .await
            .unwrap()
            .is_some_and(|s| s != RunStatus::Running)
    })
    .await;
    assert_eq!(
        env.runs.run_status(&env.tenant, run).await.unwrap(),
        Some(RunStatus::Succeeded)
    );
    let (_, out, _) = env.step(run, "w").await;
    let out = out.unwrap();
    assert_eq!(out["file_id"], mine.id.to_string());
    assert_eq!(out["actor"], "alice");
}

/// 同意計画が csv.* の対象ファイルを提案し、その委譲で schedule の run が CSV を集計・編集できる
/// （BUG-10）。委譲が無いまま有効化すると、run はファイルが見えずに失敗する（存在を明かさない not_found）。
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn scheduled_csv_nodes_run_with_the_files_delegated_in_the_consent_plan() {
    let _serial = SERIAL.lock().await;
    let Some(env) = setup().await else { return };
    if !env.tabular {
        return;
    }
    let file = env
        .storage
        .write_file_internal(&env.alice, None, "sales.csv", ORDERS_CSV, "text/csv", None)
        .await
        .unwrap();
    let id = file.id.to_string();
    let ir = json!({
        "ir_version": 1, "name": "nightly-csv",
        "declared_scopes": ["csv.read", "csv.write"],
        "triggers": [ { "kind": "schedule", "cron": "* * * * *", "tz": "UTC" } ],
        "nodes": [
            { "id": "q", "type": "csv.query", "params": {
                "file": id, "sql": "SELECT sum(qty) AS total FROM data" } },
            { "id": "p", "type": "csv.patch", "params": {
                "file": id, "base_rev": file.version,
                "ops": [ { "op": "cell_update", "row": 0, "col": 1, "value": "10" } ] } }
        ],
        "edges": [ { "from": "q", "to": "p" } ]
    });
    let (wf, _) = env
        .workflows
        .create(&env.alice, &ir, &Catalog::default(), None)
        .await
        .expect("保存");
    let parsed = WorkflowIr::from_json(&ir).unwrap();
    let file_obj = env.alice.ns().file(&id);
    let grants = env.grants_from_plan(&parsed);
    let want = [
        ("csv.read", Relation::Viewer),
        ("csv.write", Relation::Editor),
    ];
    for (scope, relation) in want {
        assert!(
            grants
                .iter()
                .any(|g| g.scope == scope && g.object == file_obj && g.relation == relation),
            "{scope} の委譲が提案される: {grants:?}"
        );
    }

    // 委譲なしで有効化（以前の同意計画と同じ状態）: 有効化はできるが、run は権限不足で落ちる。
    env.registration
        .enable(&env.alice, wf, 1, &parsed, &[])
        .await
        .expect("enable（委譲なし）");
    let denied = env.fire_schedule_and_run(wf, 61).await;
    let (status, _, err) = env.step(denied, "q").await;
    assert_eq!(status, "failed", "{err:?}");
    assert_eq!(err.unwrap()["code"], "not_found");

    // 同意計画の提案どおりに委譲すると、workflow プリンシパルが読み書きできる。
    env.registration
        .enable(&env.alice, wf, 1, &parsed, &grants)
        .await
        .expect("enable（委譲あり）");
    let run = env.fire_schedule_and_run(wf, 181).await;
    assert_ne!(run, denied);
    let principal: (String, String) = sqlx::query_as(
        "SELECT trigger_kind, principal_kind FROM workflow_run WHERE tenant_id = $1 AND run_id = $2",
    )
    .bind(&env.tenant)
    .bind(run)
    .fetch_one(&env.pool)
    .await
    .unwrap();
    assert_eq!(principal, ("schedule".into(), "workflow".into()));
    assert_eq!(
        env.runs.run_status(&env.tenant, run).await.unwrap(),
        Some(RunStatus::Succeeded),
        "q: {:?} / p: {:?}",
        env.step(run, "q").await,
        env.step(run, "p").await
    );
    let (_, q, _) = env.step(run, "q").await;
    assert_eq!(q.unwrap()["rows"], json!([["6"]]));
    let (_, p, _) = env.step(run, "p").await;
    assert_eq!(p.unwrap()["version"], file.version + 1);
    let (_, bytes) = env
        .storage
        .read_file_internal(&env.alice, file.id, None)
        .await
        .unwrap();
    assert_eq!(
        String::from_utf8_lossy(&bytes),
        "item,qty\nA,10\nB,2\nC,3\n"
    );
}

/// 新しい版の保存（利用者のアップロード・内容の更新・提案の採用）でも、フォルダに束縛したトリガが起動し、
/// folder を指定した wait(event) が起きる（BUG-28）。run 入力の parent_id は保存先のフォルダ。トリガは同じ
/// ファイルの続く保存を待ってから起動する（D-38）。
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn new_versions_start_file_triggers_and_wake_waits() {
    let _serial = SERIAL.lock().await;
    let Some(env) = setup().await else { return };
    let inbox = env
        .storage
        .create_folder(&env.alice, None, "版の受付", None)
        .await
        .unwrap();
    // 3 つの経路で版を重ねるファイル。作成のイベントは有効化より前に配り終える。
    let mut files = Vec::new();
    for name in ["upload.txt", "note.md", "proposal.txt"] {
        let f = env
            .storage
            .write_file_internal(&env.alice, Some(inbox.id), name, b"v1", "text/plain", None)
            .await
            .unwrap();
        files.push(f.id);
    }
    let e = &env;
    let ids = files.clone();
    env.relay_until("作成の配送", || {
        let ids = ids.clone();
        async move {
            for id in ids {
                if !e.delivered("create", id).await {
                    return false;
                }
            }
            true
        }
    })
    .await;

    let ir = json!({
        "ir_version": 1, "name": "inbox-versions",
        "declared_scopes": ["storage.read"],
        "triggers": [ { "kind": "event", "source": "storage.write",
                        "scope": { "folder": inbox.id.to_string() } } ],
        "nodes": [ { "id": "rd", "type": "storage.read", "params": {
            "file": { "$from": "trigger", "path": "/file_id" } } } ],
        "edges": []
    });
    let (wf, _) = env
        .workflows
        .create(&env.alice, &ir, &Catalog::default(), None)
        .await
        .expect("保存");
    let parsed = WorkflowIr::from_json(&ir).unwrap();
    env.registration
        .enable(&env.alice, wf, 1, &parsed, &env.grants_from_plan(&parsed))
        .await
        .expect("enable");
    // folder を指定した wait(event) で待っている run。
    let (waiter, _) = env
        .workflows
        .create(
            &env.alice,
            &json!({
                "ir_version": 1, "name": "wait-for-version",
                "declared_scopes": ["storage.read"],
                "triggers": [ { "kind": "interactive" } ],
                "nodes": [ { "id": "w", "type": "control.wait", "params": {
                    "kind": "event", "source": "storage.write",
                    "scope": { "folder": inbox.id.to_string() }, "timeout_sec": 600 } } ],
                "edges": []
            }),
            &Catalog::default(),
            None,
        )
        .await
        .expect("保存");
    let wait_run = env
        .launcher
        .start_interactive(&env.alice, waiter, &json!({}))
        .await
        .unwrap()
        .unwrap();
    let worker = env.worker();
    while worker.claim_and_run_once("w1").await.unwrap() {}

    // 版を重ねる: 利用者のアップロード（finalize）・内容の更新（Office・ノートの保存）・提案の採用。
    let v_upload = env
        .upload_new_version(&env.alice, files[0], b"v2 upload")
        .await;
    let v_note = env
        .storage
        .update_file_content_internal(&env.alice, files[1], b"v2 note", "text/markdown", None)
        .await
        .unwrap()
        .version;
    let proposal = env
        .storage
        .propose_file_content_internal(&env.alice, files[2], b"v2 proposal", "text/plain", None)
        .await
        .unwrap();
    let v_adopt = env
        .storage
        .adopt_proposal_version(&env.alice, files[2], proposal.version, None)
        .await
        .unwrap()
        .version;
    let ids = files.clone();
    env.relay_until("版の保存の配送と wait の起床", || {
        let ids = ids.clone();
        async move {
            for id in ids {
                if !e.delivered("update", id).await {
                    return false;
                }
            }
            e.runs
                .run_status(&e.tenant, wait_run)
                .await
                .unwrap()
                .is_some_and(|s| s != RunStatus::Running)
        }
    })
    .await;
    // wait(event) は新しい版の保存ですぐ起きる（まとめるのはトリガだけ）。
    assert_eq!(
        env.runs.run_status(&env.tenant, wait_run).await.unwrap(),
        Some(RunStatus::Succeeded)
    );
    let (_, w, _) = env.step(wait_run, "w").await;
    let w = w.unwrap();
    assert_eq!(w["op"], "update");
    assert_eq!(w["parent_id"], inbox.id.to_string());
    assert!(files.iter().any(|f| w["file_id"] == f.to_string()), "{w}");

    // トリガは同じファイルの続く保存を待つので、まだ起動しない。時刻が来たら版ごとに 1 回起動する。
    assert_eq!(env.runs_of(wf).await.len(), 0, "保存が続くのを待つ");
    env.fire_coalesced().await;
    let runs = env.runs_of(wf).await;
    assert_eq!(runs.len(), 3, "{runs:?}");
    for (file, version, text) in [
        (files[0], v_upload, "v2 upload"),
        (files[1], v_note, "v2 note"),
        (files[2], v_adopt, "v2 proposal"),
    ] {
        let (run, input, status) = runs
            .iter()
            .find(|(_, i, _)| i["file_id"] == file.to_string())
            .unwrap_or_else(|| panic!("{text} の run: {runs:?}"));
        assert_eq!(input["op"], "update", "{text}");
        assert_eq!(input["version"], version, "{text}");
        assert_eq!(input["parent_id"], inbox.id.to_string(), "{text}");
        assert_eq!(input["actor"], "alice");
        assert_eq!(
            status,
            "succeeded",
            "{text}: {:?}",
            env.step(*run, "rd").await
        );
        let (_, rd, _) = env.step(*run, "rd").await;
        assert_eq!(rd.unwrap()["text"], text);
    }
}

/// event の run が束縛フォルダへ書いても（storage.write・workflow.start の子の storage.write・トリガの
/// ファイルへの csv.patch）、その保存で自分を起こさない（BUG-29・因果の鎖・D-26）。抑止は監査に残る。
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn own_saves_and_started_children_do_not_restart_the_file_trigger() {
    let _serial = SERIAL.lock().await;
    let Some(env) = setup().await else { return };
    let inbox = env
        .storage
        .create_folder(&env.alice, None, "注文の受付", None)
        .await
        .unwrap();
    let folder = inbox.id.to_string();
    // 子（B）: 同じフォルダにメモを残す。
    let (child, _) = env
        .workflows
        .create(
            &env.alice,
            &json!({
                "ir_version": 1, "name": "child-memo",
                "declared_scopes": ["storage.write"],
                "triggers": [ { "kind": "interactive" } ],
                "nodes": [ { "id": "cw", "type": "storage.write", "params": {
                    "folder": folder, "name": "子のメモ.txt", "content": "child" } } ],
                "edges": []
            }),
            &Catalog::default(),
            None,
        )
        .await
        .expect("子を保存");
    // A: 保存されたら受付のメモを同じフォルダに書き、子を起動し、保存された CSV を直す（新しい版）。
    let mut nodes = vec![
        json!({ "id": "memo", "type": "storage.write", "params": {
            "folder": folder,
            "name": { "$template": "受付-{f}.txt",
                      "vars": { "f": { "$from": "trigger", "path": "/file_id" } } },
            "content": "ack" } }),
        json!({ "id": "kick", "type": "workflow.start", "params": {
            "name": "child-memo", "input": {} } }),
    ];
    let mut edges = vec![json!({ "from": "memo", "to": "kick" })];
    let mut scopes = vec!["storage.read", "storage.write", "workflow.start"];
    if env.tabular {
        nodes.push(json!({ "id": "p", "type": "csv.patch", "params": {
            "file": { "$from": "trigger", "path": "/file_id" },
            "base_rev": { "$from": "trigger", "path": "/version" },
            "ops": [ { "op": "cell_update", "row": 0, "col": 1, "value": "9" } ] } }));
        edges.push(json!({ "from": "kick", "to": "p" }));
        scopes.push("csv.write");
    }
    let ir = json!({
        "ir_version": 1, "name": "inbox-ack",
        "declared_scopes": scopes,
        "triggers": [ { "kind": "event", "source": "storage.write",
                        "scope": { "folder": folder } } ],
        "nodes": nodes, "edges": edges
    });
    let (wf, _) = env
        .workflows
        .create(&env.alice, &ir, &Catalog::default(), None)
        .await
        .expect("保存");
    let parsed = WorkflowIr::from_json(&ir).unwrap();
    let folder_obj = env.alice.ns().folder(&folder);
    let mut grants = vec![
        GrantRequest {
            scope: "storage.read".into(),
            object: folder_obj.clone(),
            relation: Relation::Viewer,
        },
        GrantRequest {
            scope: "storage.write".into(),
            object: folder_obj.clone(),
            relation: Relation::Editor,
        },
        GrantRequest {
            scope: "workflow.start".into(),
            object: env.alice.ns().artifact(&child.to_string()),
            relation: Relation::Viewer,
        },
    ];
    if env.tabular {
        grants.push(GrantRequest {
            scope: "csv.write".into(),
            object: folder_obj,
            relation: Relation::Editor,
        });
    }
    env.registration
        .enable(&env.alice, wf, 1, &parsed, &grants)
        .await
        .expect("enable");

    let orders = env
        .storage
        .write_file_internal(
            &env.alice,
            Some(inbox.id),
            "orders.csv",
            ORDERS_CSV,
            "text/csv",
            None,
        )
        .await
        .unwrap();
    let e = &env;
    let pending_events = move || async move {
        sqlx::query_scalar::<_, bool>(
            "SELECT EXISTS (SELECT 1 FROM storage_event_outbox o \
               WHERE o.tenant_id = $1 AND NOT (o.payload ? 'event_type') \
                 AND NOT EXISTS (SELECT 1 FROM outbox_delivery d \
                                 WHERE d.event_id = o.id AND d.consumer = 'workflow')) \
             OR EXISTS (SELECT 1 FROM workflow_run WHERE tenant_id = $1 AND status = 'running')",
        )
        .bind(&e.tenant)
        .fetch_one(&e.pool)
        .await
        .unwrap()
    };
    env.relay_until(
        "run と子の完了・書いたファイルの配送",
        || async move { !e.runs_of(wf).await.is_empty() && !pending_events().await },
    )
    .await;
    // まとめて待っている版の保存があれば起こす（自分の保存は待たずに抑止しているので何も起きない）。
    env.fire_coalesced().await;
    env.relay_until("後片付けの配送", || async move {
        !pending_events().await
    })
    .await;

    let runs = env.runs_of(wf).await;
    assert_eq!(runs.len(), 1, "人の保存 1 回につき 1 回だけ: {runs:?}");
    assert_eq!(
        runs[0].2,
        "succeeded",
        "memo: {:?} / kick: {:?}",
        env.step(runs[0].0, "memo").await,
        env.step(runs[0].0, "kick").await
    );
    assert_eq!(runs[0].1["file_id"], orders.id.to_string());
    let children: Vec<(Vec<Uuid>, String)> = sqlx::query_as(
        "SELECT trigger_chain, status FROM workflow_run WHERE tenant_id = $1 AND workflow_id = $2",
    )
    .bind(&env.tenant)
    .bind(child)
    .fetch_all(&env.pool)
    .await
    .unwrap();
    assert_eq!(
        children,
        vec![(vec![wf], "succeeded".to_string())],
        "子は親の鎖を引き継ぐ"
    );
    if env.tabular {
        let (_, p, _) = env.step(runs[0].0, "p").await;
        assert_eq!(p.unwrap()["version"], orders.version + 1, "CSV の新しい版");
    }
    // 受付のメモ・子のメモ・CSV の新しい版のそれぞれで抑止した。
    let want = if env.tabular { 3 } else { 2 };
    assert_eq!(env.suppressed("suppressed_cycle").await, want);
}

/// 委譲を外した後は、保存されたファイルを workflow プリンシパルが読めないので、トリガは発火しない
/// （BUG-39・D-50）。run 入力に読めないファイルの id・書いた人が載らず、発火の記録も残らない。
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn file_trigger_does_not_fire_once_the_delegation_is_removed() {
    let _serial = SERIAL.lock().await;
    let Some(env) = setup().await else { return };
    let inbox = env
        .storage
        .create_folder(&env.alice, None, "委譲の受付", None)
        .await
        .unwrap();
    let ir = json!({
        "ir_version": 1, "name": "inbox-delegated",
        "declared_scopes": ["storage.read"],
        "triggers": [ { "kind": "event", "source": "storage.write",
                        "scope": { "folder": inbox.id.to_string() } } ],
        "nodes": [ { "id": "rd", "type": "storage.read", "params": {
            "file": { "$from": "trigger", "path": "/file_id" } } } ],
        "edges": []
    });
    let (wf, _) = env
        .workflows
        .create(&env.alice, &ir, &Catalog::default(), None)
        .await
        .expect("保存");
    let parsed = WorkflowIr::from_json(&ir).unwrap();
    env.registration
        .enable(&env.alice, wf, 1, &parsed, &env.grants_from_plan(&parsed))
        .await
        .expect("enable");
    let first = env
        .storage
        .write_file_internal(
            &env.alice,
            Some(inbox.id),
            "first.txt",
            b"1",
            "text/plain",
            None,
        )
        .await
        .unwrap();
    let e = &env;
    env.relay_until("委譲がある間の起動", || async move {
        e.runs_of(wf)
            .await
            .first()
            .is_some_and(|(_, _, s)| s == "succeeded" || s == "failed")
    })
    .await;
    assert_eq!(env.runs_of(wf).await[0].2, "succeeded");

    // 同意をし直して、トリガのフォルダの閲覧を委譲から外す（別のフォルダだけを委譲する・トリガは有効のまま）。
    // grants が空の再有効化は既存の委譲を保つ軽量切替なので、全面の再同意で外す。
    let elsewhere = env
        .storage
        .create_folder(&env.alice, None, "別のフォルダ", None)
        .await
        .unwrap();
    env.registration
        .enable(
            &env.alice,
            wf,
            1,
            &parsed,
            &[GrantRequest {
                scope: "storage.read".into(),
                object: env.alice.ns().folder(&elsewhere.id.to_string()),
                relation: Relation::Viewer,
            }],
        )
        .await
        .expect("委譲を変えて有効化");
    let second = env
        .storage
        .write_file_internal(
            &env.alice,
            Some(inbox.id),
            "second.txt",
            b"2",
            "text/plain",
            None,
        )
        .await
        .unwrap();
    env.storage
        .update_file_content_internal(&env.alice, first.id, b"1b", "text/plain", None)
        .await
        .unwrap();
    let (first_id, second_id) = (first.id, second.id);
    env.relay_until("委譲を外した後の保存の配送", || async move {
        e.delivered("create", second_id).await && e.delivered("update", first_id).await
    })
    .await;
    env.fire_coalesced().await;
    let runs = env.runs_of(wf).await;
    assert_eq!(
        runs.len(),
        1,
        "委譲を外した後の保存では起動しない: {runs:?}"
    );
    let recorded: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM trigger_firing f WHERE f.tenant_id = $1 AND f.event_id IN ( \
           SELECT o.id FROM storage_event_outbox o WHERE o.tenant_id = $1 \
             AND ((o.node_id = $2 AND o.op = 'update') OR o.node_id = $3))",
    )
    .bind(&env.tenant)
    .bind(first_id)
    .bind(second_id)
    .fetch_one(&env.pool)
    .await
    .unwrap();
    assert_eq!(recorded, 0, "読めないファイルの発火は記録もしない");
}

/// 同じファイルの新しい版の保存が短い間に続いても（Office・ノートの自動保存）、トリガは最後の版で 1 回だけ
/// 起動する（D-38）。
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn consecutive_new_versions_of_a_file_start_one_run() {
    let _serial = SERIAL.lock().await;
    let Some(env) = setup().await else { return };
    let inbox = env
        .storage
        .create_folder(&env.alice, None, "議事録", None)
        .await
        .unwrap();
    let note = env
        .storage
        .write_file_internal(
            &env.alice,
            Some(inbox.id),
            "定例.md",
            b"v1",
            "text/markdown",
            None,
        )
        .await
        .unwrap();
    let e = &env;
    let note_id = note.id;
    env.relay_until("作成の配送", || async move {
        e.delivered("create", note_id).await
    })
    .await;
    let ir = json!({
        "ir_version": 1, "name": "minutes-saved",
        "declared_scopes": ["storage.read"],
        "triggers": [ { "kind": "event", "source": "storage.write",
                        "scope": { "folder": inbox.id.to_string() } } ],
        "nodes": [ { "id": "rd", "type": "storage.read", "params": {
            "file": { "$from": "trigger", "path": "/file_id" } } } ],
        "edges": []
    });
    let (wf, _) = env
        .workflows
        .create(&env.alice, &ir, &Catalog::default(), None)
        .await
        .expect("保存");
    let parsed = WorkflowIr::from_json(&ir).unwrap();
    env.registration
        .enable(&env.alice, wf, 1, &parsed, &env.grants_from_plan(&parsed))
        .await
        .expect("enable");
    let mut last = 0;
    for body in [b"v2".as_slice(), b"v3", b"v4"] {
        last = env
            .storage
            .update_file_content_internal(&env.alice, note.id, body, "text/markdown", None)
            .await
            .unwrap()
            .version;
    }
    let updates = move || async move {
        sqlx::query_scalar::<_, i64>(
            "SELECT count(*) FROM storage_event_outbox o \
               JOIN outbox_delivery d ON d.event_id = o.id AND d.consumer = 'workflow' \
               WHERE o.tenant_id = $1 AND o.op = 'update' AND o.node_id = $2",
        )
        .bind(&e.tenant)
        .bind(note_id)
        .fetch_one(&e.pool)
        .await
        .unwrap()
    };
    env.relay_until("3 回の保存の配送", || async move { updates().await == 3 })
        .await;
    assert_eq!(env.runs_of(wf).await.len(), 0, "保存が続く間は起動しない");
    env.fire_coalesced().await;
    let runs = env.runs_of(wf).await;
    assert_eq!(runs.len(), 1, "続く保存は 1 回にまとまる: {runs:?}");
    let (run, input, status) = &runs[0];
    assert_eq!(input["version"], last, "最後の版で起動する");
    assert_eq!(status, "succeeded");
    let (_, rd, _) = env.step(*run, "rd").await;
    assert_eq!(rd.unwrap()["text"], "v4");
    let outcomes: Vec<(String, i64)> = sqlx::query_as(
        "SELECT outcome, count(*) FROM trigger_firing WHERE tenant_id = $1 \
         GROUP BY outcome ORDER BY outcome",
    )
    .bind(&env.tenant)
    .fetch_all(&env.pool)
    .await
    .unwrap();
    assert_eq!(
        outcomes,
        vec![("coalesced".to_string(), 2), ("fired".to_string(), 1)]
    );
}

/// まとめて待たせた発火（新しい版の保存・最長 10 分）は、起動する時点にも workflow プリンシパルが保存された
/// ファイルを読めるかを確かめ直す。待っている間に委譲を外したら起動せず、ファイルの id・書いた人を run の
/// 入力に載せない（BUG-50・D-50）。
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn deferred_file_trigger_rechecks_the_delegation_when_it_starts() {
    let _serial = SERIAL.lock().await;
    let Some(env) = setup().await else { return };
    let inbox = env
        .storage
        .create_folder(&env.alice, None, "見積", None)
        .await
        .unwrap();
    let doc = env
        .storage
        .write_file_internal(
            &env.alice,
            Some(inbox.id),
            "見積.txt",
            b"v1",
            "text/plain",
            None,
        )
        .await
        .unwrap();
    let e = &env;
    let doc_id = doc.id;
    env.relay_until("作成の配送", || async move {
        e.delivered("create", doc_id).await
    })
    .await;
    let ir = json!({
        "ir_version": 1, "name": "quote-saved",
        "declared_scopes": ["storage.read"],
        "triggers": [ { "kind": "event", "source": "storage.write",
                        "scope": { "folder": inbox.id.to_string() } } ],
        "nodes": [ { "id": "rd", "type": "storage.read", "params": {
            "file": { "$from": "trigger", "path": "/file_id" } } } ],
        "edges": []
    });
    let (wf, _) = env
        .workflows
        .create(&env.alice, &ir, &Catalog::default(), None)
        .await
        .expect("保存");
    let parsed = WorkflowIr::from_json(&ir).unwrap();
    env.registration
        .enable(&env.alice, wf, 1, &parsed, &env.grants_from_plan(&parsed))
        .await
        .expect("enable");
    // 委譲がある間に新しい版を保存する（relay の時点では読めるので、起動を待たせる）。
    env.storage
        .update_file_content_internal(&env.alice, doc.id, b"v2", "text/plain", None)
        .await
        .unwrap();
    env.relay_until("新しい版の配送", || async move {
        e.delivered("update", doc_id).await
    })
    .await;
    let waiting: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM trigger_firing WHERE tenant_id = $1 AND outcome = 'pending'",
    )
    .bind(&env.tenant)
    .fetch_one(&env.pool)
    .await
    .unwrap();
    assert_eq!(waiting, 1, "新しい版の保存は起動を待たせる");

    // 待っている間に、トリガのフォルダの閲覧を委譲から外す（別のフォルダだけを委譲し直す・トリガは有効のまま。
    // storage.read のスコープは委譲に残るので、run の開始の委譲の確認は通る）。
    let elsewhere = env
        .storage
        .create_folder(&env.alice, None, "別のフォルダ", None)
        .await
        .unwrap();
    env.registration
        .enable(
            &env.alice,
            wf,
            1,
            &parsed,
            &[GrantRequest {
                scope: "storage.read".into(),
                object: env.alice.ns().folder(&elsewhere.id.to_string()),
                relation: Relation::Viewer,
            }],
        )
        .await
        .expect("委譲を変えて有効化");
    env.fire_coalesced().await;
    let runs = env.runs_of(wf).await;
    assert!(
        runs.is_empty(),
        "委譲を外した後は、待たせていた発火でも起動しない（入力に file_id を載せない）: {runs:?}"
    );
    let outcomes: Vec<(String, i64)> = sqlx::query_as(
        "SELECT outcome, count(*) FROM trigger_firing WHERE tenant_id = $1 \
         GROUP BY outcome ORDER BY outcome",
    )
    .bind(&env.tenant)
    .fetch_all(&env.pool)
    .await
    .unwrap();
    assert_eq!(
        outcomes,
        vec![("skipped".to_string(), 1)],
        "見送りとして残し、再試行しない"
    );
    env.cleanup().await;
}

/// 利用者の保存の trace_id が run の形（run_id の 32 桁 hex）でも、待たせた発火に由来の run を残さない。因果の鎖は
/// outbox の非公開部の run の印だけで決まる（BUG-44）。保存の outbox には印（run の書込でなければ null）が載る。
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn saves_with_a_run_shaped_trace_id_leave_no_cause_run() {
    let _serial = SERIAL.lock().await;
    let Some(env) = setup().await else { return };
    let inbox = env
        .storage
        .create_folder(&env.alice, None, "由来", None)
        .await
        .unwrap();
    let doc = env
        .storage
        .write_file_internal(
            &env.alice,
            Some(inbox.id),
            "memo.txt",
            b"v1",
            "text/plain",
            None,
        )
        .await
        .unwrap();
    let e = &env;
    let doc_id = doc.id;
    env.relay_until("作成の配送", || async move {
        e.delivered("create", doc_id).await
    })
    .await;
    let ir = json!({
        "ir_version": 1, "name": "memo-saved",
        "declared_scopes": ["storage.read"],
        "triggers": [ { "kind": "event", "source": "storage.write",
                        "scope": { "folder": inbox.id.to_string() } } ],
        "nodes": [ { "id": "rd", "type": "storage.read", "params": {
            "file": { "$from": "trigger", "path": "/file_id" } } } ],
        "edges": []
    });
    let (wf, _) = env
        .workflows
        .create(&env.alice, &ir, &Catalog::default(), None)
        .await
        .expect("保存");
    let parsed = WorkflowIr::from_json(&ir).unwrap();
    env.registration
        .enable(&env.alice, wf, 1, &parsed, &env.grants_from_plan(&parsed))
        .await
        .expect("enable");
    // 利用者の要求の trace_id（OTel の 32 桁 hex・run の trace_id と同じ形）で新しい版を保存する。
    let trace = Uuid::new_v4().simple().to_string();
    env.storage
        .update_file_content_internal(&env.alice, doc.id, b"v2", "text/plain", Some(&trace))
        .await
        .unwrap();
    env.relay_until("新しい版の配送", || async move {
        e.delivered("update", doc_id).await
    })
    .await;
    let mark: Option<Value> = sqlx::query_scalar(
        "SELECT private_payload FROM storage_event_outbox \
         WHERE tenant_id = $1 AND node_id = $2 AND op = 'update'",
    )
    .bind(&env.tenant)
    .bind(doc_id)
    .fetch_one(&env.pool)
    .await
    .unwrap();
    assert_eq!(
        mark,
        Some(json!({ "origin_run": null })),
        "保存の outbox に印が載る"
    );
    let causes: Vec<(String, Option<Uuid>)> =
        sqlx::query_as("SELECT outcome, cause_run_id FROM trigger_firing WHERE tenant_id = $1")
            .bind(&env.tenant)
            .fetch_all(&env.pool)
            .await
            .unwrap();
    assert_eq!(
        causes,
        vec![("pending".to_string(), None)],
        "実在しない run の id を由来に残さない"
    );
    env.fire_coalesced().await;
    let runs = env.runs_of(wf).await;
    assert_eq!(runs.len(), 1, "{runs:?}");
    let (cause, chain): (Option<Uuid>, Vec<Uuid>) = sqlx::query_as(
        "SELECT cause_run_id, trigger_chain FROM workflow_run WHERE tenant_id = $1 AND run_id = $2",
    )
    .bind(&env.tenant)
    .bind(runs[0].0)
    .fetch_one(&env.pool)
    .await
    .unwrap();
    assert_eq!((cause, chain), (None, vec![]));
    env.cleanup().await;
}
