//! Stage A DoD e2e: **本番** executor（CapabilityNodeExecutor＋ProdNodePorts）で実チョークポイント
//! を通してワークフローが走ることを検証する（W4）。
//!
//! 対話トリガで [script.run（実 script-runtime）→ storage.write（実 StorageService・in-TX 冪等）] を
//! 実行し、run 完走・ノード間 dataflow・ファイル書込・effect_journal・run/step 監査を確認する。
//! 実 Postgres＋OpenFGA＋MinIO が要るため env ゲート（未設定はスキップ）。

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::print_stderr,
    clippy::pedantic,
    clippy::cognitive_complexity
)]

use std::sync::Arc;
use std::time::Duration;

use authz::client::{OpenFgaClient, OpenFgaConfig};
use authz::{AuthContext, AuthzClient, Principal, PrincipalKind, Relation};
use serde_json::{json, Value};
use sqlx::postgres::PgPoolOptions;
use uuid::Uuid;
use workflow_engine::{
    CapabilityAudit, CapabilityNodeExecutor, Catalog, DelegationStore, EffectJournal, RunStatus,
    RunStore, WorkerConfig, WorkflowRunLauncher, WorkflowStore, WorkflowWorker,
};

struct NoopAudit;
impl CapabilityAudit for NoopAudit {
    fn record(&self, _t: &str, _a: &str, _ok: bool, _m: &Value) {}
}

/// 対話ワークフロー: script が content を計算 → storage.write が書き込む（ノード間 dataflow）。
fn compute_and_write_ir() -> Value {
    json!({
        "ir_version": 1,
        "name": "e2e-compute-write",
        "declared_scopes": ["storage.write"],
        "nodes": [
            {
                "id": "compute",
                "type": "script.run",
                "params": {
                    "source": { "inline": "function main(i){ return { content: 'hello-' + i.name }; }" },
                    "input": { "$from": "input" }
                }
            },
            {
                "id": "save",
                "type": "storage.write",
                "params": {
                    "name": "e2e-out.txt",
                    "content": { "$from": "nodes.compute.output", "path": "/content" }
                }
            }
        ],
        "edges": [{ "from": "compute", "to": "save" }],
        "triggers": [{ "kind": "schedule", "cron": "0 9 * * *", "tz": "Asia/Tokyo" }]
    })
}

fn user_ctx(tenant: &str, org: &str, uid: &str) -> AuthContext {
    AuthContext::new(
        Principal {
            kind: PrincipalKind::User,
            id: uid.into(),
            email: None,
            groups: vec![],
            roles: vec![],
            tenant_id: Some(tenant.into()),
        },
        org.into(),
        tenant.into(),
    )
}

struct Env {
    pool: sqlx::PgPool,
    fga: Arc<dyn AuthzClient>,
    storage: Arc<storage::StorageService>,
    gateway: Arc<llm_gateway::LlmGateway>,
}

async fn setup() -> Option<Env> {
    let (Ok(db), Ok(fga_url)) = (
        std::env::var("STORAGE_TEST_DATABASE_URL"),
        std::env::var("OPENFGA_TEST_URL"),
    ) else {
        eprintln!("STORAGE_TEST_DATABASE_URL / OPENFGA_TEST_URL 未設定のためスキップ");
        return None;
    };
    let s3_endpoint = std::env::var("STORAGE_TEST_S3_ENDPOINT")
        .unwrap_or_else(|_| "http://localhost:9000".into());
    let pool = PgPoolOptions::new()
        .max_connections(5)
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
                store_name: format!("shiki-wf-e2e-{}", Uuid::new_v4()),
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

    // stub LLM ゲートウェイ（本テストは llm ノードを使わないが ProdNodePorts に必須）。
    let gateway = Arc::new(
        llm_gateway::LlmGateway::build(
            pool.clone(),
            http,
            llm_gateway::GatewayConfig {
                provider: llm_gateway::ProviderConfig {
                    kind: llm_gateway::ProviderKind::Stub,
                    base_url: None,
                    api_key: None,
                    timeout_secs: 30,
                },
                catalog: llm_gateway::ModelCatalog {
                    default_model: "stub".into(),
                    models: vec![llm_gateway::ModelEntry {
                        id: "stub".into(),
                        real_id: None,
                        prompt_price_micros_per_mtok: 0,
                        completion_price_micros_per_mtok: 0,
                    }],
                },
                langfuse: None,
            },
        )
        .expect("gateway"),
    );

    Some(Env {
        pool,
        fga,
        storage,
        gateway,
    })
}

#[tokio::test]
async fn interactive_run_completes_through_production_executor() {
    let Some(env) = setup().await else { return };
    let tenant = format!("t-{}", Uuid::new_v4().simple());
    let org = tenant.clone();
    let alice = user_ctx(&tenant, &org, "alice");

    // alice に org member を付与（storage.write を org 直下に許可）。
    fga::member(&env.fga, &alice).await;

    let artifacts = Arc::new(artifact::ArtifactStore::new(
        env.pool.clone(),
        Arc::clone(&env.fga),
    ));
    let workflows = WorkflowStore::new(Arc::clone(&artifacts));
    let runs = RunStore::new(env.pool.clone());
    let delegation = DelegationStore::new(env.pool.clone(), Arc::clone(&env.fga));
    let launcher = WorkflowRunLauncher::new(delegation, workflows.clone(), runs.clone());

    // IR 保存（V1〜V7）。
    let (wf_id, _ir) = workflows
        .create(&alice, &compute_and_write_ir(), &Catalog::default(), None)
        .await
        .expect("save ir");

    // 本番 executor（実 storage・実 script-runtime・stub llm）。
    let ports = Arc::new(api::workflow_runtime::ProdNodePorts {
        storage: Arc::clone(&env.storage),
        search: None,
        gateway: Arc::clone(&env.gateway),
        sandbox: None,
        secrets: None,
        launcher: launcher.clone(),
        http: reqwest::Client::new(),
        db: env.pool.clone(),
    });
    let engine = Arc::new(script_runtime::engine::ScriptEngine::new().expect("script engine"));
    let executor: Arc<dyn workflow_engine::NodeExecutor> = Arc::new(
        CapabilityNodeExecutor::new(
            ports,
            EffectJournal::new(env.pool.clone()),
            Arc::new(NoopAudit),
        )
        .with_script_engine(engine, script_runtime::engine::Limits::default()),
    );

    // 対話起動（本人権限）。
    let run_id = launcher
        .start_interactive(&alice, wf_id, &json!({ "name": "world" }))
        .await
        .expect("start")
        .expect("run id");

    // ワーカーが全 step を完走させる。
    let worker = WorkflowWorker::new(runs.clone(), executor, WorkerConfig::default())
        .scoped_to_tenant(&tenant);
    while worker.claim_and_run_once("w1").await.unwrap() {}

    assert_eq!(
        runs.run_status(&tenant, run_id).await.unwrap(),
        Some(RunStatus::Succeeded),
        "本番 executor で対話 run が完走する"
    );

    // ノード間 dataflow: script 出力 "hello-world" が storage.write されたファイルに入る。
    let count: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM node WHERE org = $1 AND name = 'e2e-out.txt' AND deleted_at IS NULL",
    )
    .bind(&org)
    .fetch_one(&env.pool)
    .await
    .unwrap();
    assert_eq!(count, 1, "ファイルが 1 つ書き込まれる（高々 1 バージョン）");

    // effect_journal に storage.write の記録が残る（本文は載らない）。
    let summaries: Vec<Value> =
        sqlx::query_scalar("SELECT result_summary FROM effect_journal WHERE tenant_id = $1")
            .bind(&tenant)
            .fetch_all(&env.pool)
            .await
            .unwrap();
    assert!(!summaries.is_empty(), "effect_journal に記録が残る");
    for s in &summaries {
        assert!(s.get("body").is_none(), "journal に本文を載せない");
    }

    // run_event（監査/履歴）に run.started と step.succeeded が乗る。
    let events: i64 =
        sqlx::query_scalar("SELECT count(*) FROM run_event WHERE tenant_id = $1 AND run_id = $2")
            .bind(&tenant)
            .bind(run_id)
            .fetch_one(&env.pool)
            .await
            .unwrap();
    assert!(events >= 3, "run/step イベントが履歴に乗る");
}

mod fga {
    use super::*;
    /// alice に org member タプルを付与する（storage.write の org 直下配置に必要）。
    pub(super) async fn member(fga: &Arc<dyn AuthzClient>, ctx: &AuthContext) {
        let org_obj = ctx.ns().organization(&ctx.org);
        fga.write_tuple(&ctx.subject(), Relation::Member, &org_obj)
            .await
            .expect("org member tuple");
    }
}
