//! キーで追加か更新・足す数・まとめて登録の同じレコードを探す列（GAP-16）の結合テスト（実 Postgres ＋実 OpenFGA）。
//!
//! 本番の executor（`CapabilityNodeExecutor` ＋ `ProdNodePorts` の data 結線・実 effect_journal）でブロックを並行に
//! 実行し、次を確かめる。
//!
//! - 同じ値で並行に「レコードを追加か更新」しても 1 件だけになり、足した数はすべて残る（探す列の一意と rev の照合）。
//! - 並行に「足す数」で減らしても更新を失わない。列の最小値を外れる足し算は書かずに失敗する（在庫を 0 未満にしない）。
//! - 数式の列は足した後の値で計算し直される（data の 1 件の更新と同じ入口）。
//! - 無ければ追加は追加の権限、あれば書き換えは書き換えの権限で判定する（追加だけの人・見るだけの人・見えない行）。
//! - まとめて登録の同じレコードを探す列は、取り込み直しても件数を増やさない。

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::print_stderr,
    clippy::pedantic,
    clippy::too_many_lines
)]

use std::sync::Arc;

use async_trait::async_trait;
use authz::client::{OpenFgaClient, OpenFgaConfig};
use authz::{AuthContext, AuthzClient, Principal, PrincipalKind, Relation};
use data::{DataStore, FieldType, FsmStore, NewDataTable, NumericSpec, RefResolver, TableSchema};
use serde_json::{json, Value};
use sqlx::postgres::PgPoolOptions;
use uuid::Uuid;
use workflow_engine::{CapabilityNodeExecutor, EffectJournal, NodeContext, NodeExecutor};

struct Users;

#[async_trait]
impl RefResolver for Users {
    async fn user_exists(&self, _: &AuthContext, _: &str) -> Result<bool, String> {
        Ok(true)
    }
    async fn role_exists(&self, _: &AuthContext, _: &str) -> Result<bool, String> {
        Ok(false)
    }
    async fn file_readable(&self, _: &AuthContext, _: Uuid) -> Result<bool, String> {
        Ok(false)
    }
}

struct NoopAudit;
impl workflow_engine::CapabilityAudit for NoopAudit {
    fn record(&self, _t: &str, _a: &str, _ok: bool, _m: &Value) {}
}

fn user(tenant: &str, id: &str) -> AuthContext {
    AuthContext::new(
        Principal {
            kind: PrincipalKind::User,
            id: id.into(),
            email: None,
            groups: vec![],
            roles: vec![],
            tenant_id: Some(tenant.into()),
        },
        tenant.into(),
        tenant.into(),
    )
}

struct Env {
    pool: sqlx::PgPool,
    fga: Arc<dyn AuthzClient>,
    /// テストが作った OpenFGA のストア（片付けで消す）。
    fga_store: (String, String),
    data: Arc<DataStore>,
    executor: Arc<dyn NodeExecutor>,
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
        .max_connections(16)
        .connect(&db)
        .await
        .expect("pg");
    sqlx::migrate!("../../migrations")
        .run(&pool)
        .await
        .expect("migrate");
    let http = reqwest::Client::new();
    let client = OpenFgaClient::connect(
        http.clone(),
        &OpenFgaConfig {
            base_url: fga_url.clone(),
            store_name: format!("shiki-wf-upsert-{}", Uuid::new_v4()),
        },
        &authz::model::default_model(),
    )
    .await
    .expect("fga");
    let fga_store = (fga_url, client.store_id().to_string());
    let fga = Arc::new(client) as Arc<dyn AuthzClient>;
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
    let storage = Arc::new(storage::StorageService::new(
        pool.clone(),
        object_store,
        Arc::clone(&fga),
        std::time::Duration::from_secs(300),
        std::time::Duration::from_secs(900),
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
    let data = Arc::new(DataStore::new(
        pool.clone(),
        Arc::clone(&fga),
        Arc::new(Users),
    ));
    let artifacts = Arc::new(artifact::ArtifactStore::new(pool.clone(), Arc::clone(&fga)));
    let fsms = Arc::new(FsmStore::new(Arc::clone(&artifacts), (*data).clone()));
    let workflows = workflow_engine::WorkflowStore::new(Arc::clone(&artifacts));
    let runs = workflow_engine::RunStore::new(pool.clone(), workflow_engine::DEFAULT_LEASE_SECS);
    let delegation = workflow_engine::DelegationStore::new(pool.clone(), Arc::clone(&fga));
    let launcher = workflow_engine::WorkflowRunLauncher::new(delegation, workflows, runs);
    let ports = Arc::new(api::workflow_runtime::ProdNodePorts {
        storage,
        search: None,
        gateway,
        sandbox: None,
        sandbox_backend: sandbox_client::SandboxBackend::Wasm,
        secrets: None,
        tabular: None,
        skill_artifacts: None,
        launcher,
        http: api::workflow_runtime::HttpEgress::default(),
        db: pool.clone(),
        data: Some(Arc::clone(&data)),
        fsms: Some(fsms),
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
        fga_store,
        data,
        executor,
    })
}

impl Env {
    /// ブロックを 1 回実行する（実行した人 `who`・step ごとに別の冪等キー）。
    async fn run(&self, tenant: &str, who: &str, node_type: &str, params: Value) -> NodeOut {
        let ctx = NodeContext {
            tenant_id: tenant.into(),
            org: tenant.into(),
            run_id: Uuid::new_v4(),
            run_started_at: chrono::Utc::now(),
            step_path: "n1".into(),
            idempotency_key: format!("wf:{tenant}:{}:n1", Uuid::new_v4()),
            attempt: 1,
            principal: who.into(),
            principal_kind: "user".into(),
            input: json!({}),
            trigger: json!({}),
            node_outputs: Value::Null,
            each: None,
            trace_id: None,
            scope_ceiling: vec!["data.read".into(), "data.write".into()],
            cancel: workflow_engine::run::CancelSignal::default(),
            workflow_id: Uuid::nil(),
            cache: None,
        };
        let res = self.executor.execute(node_type, &params, &ctx).await;
        NodeOut {
            ok: res.ok,
            output: res.output,
            code: res.error.as_ref().map(|e| e.code.clone()),
            message: res.error.map(|e| e.message),
        }
    }

    async fn records(&self, who: &AuthContext, table: Uuid) -> Vec<data::DataRecord> {
        self.data
            .list_records(who, table, &data::ListRecordsOptions::default(), None)
            .await
            .unwrap()
            .items
    }

    async fn grant(&self, who: &AuthContext, rel: Relation, table: Uuid) {
        self.fga
            .write_tuple(
                &who.subject(),
                rel,
                &who.ns().data_table(&table.to_string()),
            )
            .await
            .unwrap();
    }

    async fn cleanup_tenant(&self, tenant: &str) {
        for table in ["effect_journal", "storage_event_outbox"] {
            sqlx::query(&format!("DELETE FROM {table} WHERE tenant_id = $1"))
                .bind(tenant)
                .execute(&self.pool)
                .await
                .unwrap();
        }
        self.data.purge_tenant(tenant).await.unwrap();
    }

    async fn delete_store(&self) {
        let (base, store) = &self.fga_store;
        let res = reqwest::Client::new()
            .delete(format!("{base}/stores/{store}"))
            .send()
            .await
            .expect("OpenFGA のストアを消す");
        assert!(res.status().is_success(), "delete store: {}", res.status());
    }
}

/// 本体を流し、落ちても片付けてから結果を返す（テナントの行・効果の記録・OpenFGA のストア）。
async fn finish(env: &Env, tenant: &str, body: impl std::future::Future<Output = ()>) {
    let outcome = futures::FutureExt::catch_unwind(std::panic::AssertUnwindSafe(body)).await;
    env.cleanup_tenant(tenant).await;
    env.delete_store().await;
    if let Err(p) = outcome {
        std::panic::resume_unwind(p);
    }
}

#[derive(Debug)]
struct NodeOut {
    ok: bool,
    output: Value,
    code: Option<String>,
    message: Option<String>,
}

/// 品目: SKU（重複を許さない）・名前・在庫（0 以上）・発注点・不足（発注点 − 在庫の数式）。
fn items_schema() -> TableSchema {
    let mut sku = data::FieldDef::new("sku", FieldType::Text);
    sku.unique = true;
    let name = data::FieldDef::new("name", FieldType::Text);
    let mut stock = data::FieldDef::new("stock", FieldType::Number);
    stock.numeric_spec = Some(NumericSpec {
        scale: 0,
        on_excess_scale: data::ExcessScale::Reject,
        min: Some(0.0),
        max: None,
    });
    let reorder = data::FieldDef::new("reorder_point", FieldType::Number);
    let mut shortage = data::FieldDef::new("shortage", FieldType::Formula);
    shortage.formula = Some(
        serde_json::from_value(
            json!({ "sub": [{ "field": "reorder_point" }, { "field": "stock" }] }),
        )
        .unwrap(),
    );
    TableSchema {
        fields: vec![sku, name, stock, reorder, shortage],
        status_field: None,
        row_policy: None,
        field_policy: vec![],
        aggregate_min_rows: None,
        fsm_ref: None,
        rag: None,
    }
}

async fn items_table(env: &Env, owner: &AuthContext) -> Uuid {
    env.data
        .create_table(
            owner,
            NewDataTable {
                name: "品目".into(),
                schema: items_schema(),
            },
            None,
        )
        .await
        .unwrap()
        .id
}

/// 同じ SKU で 8 本並行に「レコードを追加か更新」（在庫に 1 足す）しても、レコードは 1 件で在庫は 8。
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn concurrent_upserts_with_the_same_key_make_one_record() {
    let Some(env) = setup().await else { return };
    let env = Arc::new(env);
    let tenant = format!("t-{}", Uuid::new_v4().simple());
    let alice = user(&tenant, "alice");
    let body = async {
        let table = items_table(&env, &alice).await;
        let params = json!({ "table": table.to_string(), "match_on": "sku",
                             "data": { "sku": "A-1", "name": "ねじ", "reorder_point": 10 },
                             "add": { "stock": 1 } });
        let mut tasks = Vec::new();
        for _ in 0..8 {
            let (env, tenant, params) = (Arc::clone(&env), tenant.clone(), params.clone());
            tasks.push(tokio::spawn(async move {
                env.run(&tenant, "alice", "data.record.upsert", params)
                    .await
            }));
        }
        let mut created = 0;
        for t in tasks {
            let out = t.await.unwrap();
            assert!(out.ok, "{out:?}");
            if out.output["created"] == true {
                created += 1;
            }
        }
        assert_eq!(created, 1, "追加は 1 回だけ（ほかは書き換えに回る）");
        let rows = env.records(&alice, table).await;
        assert_eq!(rows.len(), 1, "同じ SKU は 1 件");
        assert_eq!(rows[0].data["stock"], 8, "足した数はすべて残る");
        assert_eq!(
            rows[0].data["shortage"], 2,
            "数式は足した後の値で計算し直す"
        );
    };
    finish(&env, &tenant, body).await;
}

/// 在庫 20 から 12 本並行に 1 ずつ減らしても 8 になる（読み・書きの間の更新を失わない）。在庫 1 から 2 本並行に
/// 1 ずつ減らすと、1 本は列の最小値（0）を外れるので書かずに失敗し、在庫は 0（0 未満にならない）。
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn concurrent_adds_do_not_lose_updates_or_go_below_the_minimum() {
    let Some(env) = setup().await else { return };
    let env = Arc::new(env);
    let tenant = format!("t-{}", Uuid::new_v4().simple());
    let alice = user(&tenant, "alice");
    let body = async {
        let table = items_table(&env, &alice).await;
        let rec = env
            .data
            .create_record(
                &alice,
                table,
                json!({ "sku": "B-1", "stock": 20, "reorder_point": 5 }),
                None,
            )
            .await
            .unwrap();
        let params = json!({ "table": table.to_string(), "record": rec.id.to_string(),
                             "add": { "stock": -1 } });
        let mut tasks = Vec::new();
        for _ in 0..12 {
            let (env, tenant, params) = (Arc::clone(&env), tenant.clone(), params.clone());
            tasks.push(tokio::spawn(async move {
                env.run(&tenant, "alice", "data.record.update", params)
                    .await
            }));
        }
        for t in tasks {
            let out = t.await.unwrap();
            assert!(out.ok, "{out:?}");
        }
        let now = env
            .data
            .get_record(&alice, table, rec.id, None)
            .await
            .unwrap();
        assert_eq!(now.data["stock"], 8);
        assert_eq!(now.data["shortage"], -3);
        assert_eq!(now.rev, 13, "12 回だけ書いた");

        let low = env
            .data
            .create_record(&alice, table, json!({ "sku": "B-2", "stock": 1 }), None)
            .await
            .unwrap();
        let params = json!({ "table": table.to_string(), "record": low.id.to_string(),
                             "add": { "stock": -1 } });
        let (a, b) = tokio::join!(
            env.run(&tenant, "alice", "data.record.update", params.clone()),
            env.run(&tenant, "alice", "data.record.update", params),
        );
        let oks = [&a, &b].iter().filter(|o| o.ok).count();
        assert_eq!(oks, 1, "{a:?} {b:?}");
        let failed = if a.ok { &b } else { &a };
        assert_eq!(failed.code.as_deref(), Some("invalid"), "{failed:?}");
        let now = env
            .data
            .get_record(&alice, table, low.id, None)
            .await
            .unwrap();
        assert_eq!(now.data["stock"], 0);
    };
    finish(&env, &tenant, body).await;
}

/// 無ければ追加は追加の権限、あれば書き換えは書き換えの権限で判定する。
///
/// - 追加だけの人（sam）は、新しい SKU を追加できるが、自分が追加したレコードを書き換えることはできない。
/// - 追加だけの人に見えない同じ SKU（alice の行）があると、1 件の追加と同じ一意の違反で断られ、何も変わらない。
/// - 見るだけの人（vic）は、追加も書き換えもできない。
/// - 編集できる人（ed）は書き換えられる。
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn upsert_checks_create_and_update_rights_separately() {
    let Some(env) = setup().await else { return };
    let tenant = format!("t-{}", Uuid::new_v4().simple());
    let alice = user(&tenant, "alice");
    let (sam, vic, ed) = (
        user(&tenant, "sam"),
        user(&tenant, "vic"),
        user(&tenant, "ed"),
    );
    let body = async {
        let table = items_table(&env, &alice).await;
        env.grant(&sam, Relation::Submitter, table).await;
        env.grant(&vic, Relation::Viewer, table).await;
        env.grant(&ed, Relation::Editor, table).await;
        let mine = env
            .data
            .create_record(&alice, table, json!({ "sku": "A-1", "stock": 5 }), None)
            .await
            .unwrap();
        let upsert = |sku: &str, stock: i64| {
            json!({ "table": table.to_string(), "match_on": "sku",
                    "data": { "sku": sku }, "add": { "stock": stock } })
        };

        // 追加だけの人: 新しい SKU は追加できる。
        let out = env
            .run(&tenant, "sam", "data.record.upsert", upsert("S-1", 2))
            .await;
        assert!(out.ok, "{out:?}");
        assert_eq!(out.output["created"], true);
        // 自分が追加したレコードは見えるが、書き換えの権限が無いので断る。
        let out = env
            .run(&tenant, "sam", "data.record.upsert", upsert("S-1", 2))
            .await;
        assert_eq!(out.code.as_deref(), Some("forbidden"), "{out:?}");
        // 見えない alice の行と同じ SKU は、1 件の追加と同じ一意の違反（明かすことは増えない）。
        let out = env
            .run(&tenant, "sam", "data.record.upsert", upsert("A-1", 1))
            .await;
        assert_eq!(out.code.as_deref(), Some("unique_violation"), "{out:?}");
        // 見るだけの人: 追加も書き換えもできない。
        let out = env
            .run(&tenant, "vic", "data.record.upsert", upsert("V-1", 1))
            .await;
        assert_eq!(out.code.as_deref(), Some("forbidden"), "{out:?}");
        let out = env
            .run(&tenant, "vic", "data.record.upsert", upsert("A-1", 1))
            .await;
        assert_eq!(out.code.as_deref(), Some("forbidden"), "{out:?}");
        // 編集できる人は書き換えられる。
        let out = env
            .run(&tenant, "ed", "data.record.upsert", upsert("A-1", 1))
            .await;
        assert!(out.ok, "{out:?}");
        assert_eq!(out.output["created"], false);
        assert_eq!(out.output["id"], mine.id.to_string());

        let rows = env.records(&alice, table).await;
        let mut got: Vec<(String, i64)> = rows
            .iter()
            .map(|r| {
                (
                    r.data["sku"].as_str().unwrap().to_string(),
                    r.data["stock"].as_i64().unwrap(),
                )
            })
            .collect();
        got.sort();
        assert_eq!(got, vec![("A-1".into(), 6), ("S-1".into(), 2)]);
    };
    finish(&env, &tenant, body).await;
}

/// まとめて登録の同じレコードを探す列: 取り込み直しても件数が増えず、同じ ID の行は書き換わる。
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn bulk_upsert_matched_on_a_key_does_not_duplicate_on_reimport() {
    let Some(env) = setup().await else { return };
    let tenant = format!("t-{}", Uuid::new_v4().simple());
    let alice = user(&tenant, "alice");
    let body = async {
        let mut rid = data::FieldDef::new("response_id", FieldType::Text);
        rid.unique = true;
        let nps = data::FieldDef::new("nps", FieldType::Number);
        let table = env
            .data
            .create_table(
                &alice,
                NewDataTable {
                    name: "回答".into(),
                    schema: TableSchema {
                        fields: vec![rid, nps],
                        status_field: None,
                        row_policy: None,
                        field_policy: vec![],
                        aggregate_min_rows: None,
                        fsm_ref: None,
                        rag: None,
                    },
                },
                None,
            )
            .await
            .unwrap()
            .id;
        let bulk = |rows: Value| json!({ "table": table.to_string(), "match_on": "response_id", "rows": rows });
        let first = env
            .run(
                &tenant,
                "alice",
                "data.bulk_upsert",
                bulk(json!([
                    { "data": { "response_id": "R-1", "nps": 9 } },
                    { "data": { "response_id": "R-2", "nps": 3 } },
                    { "data": { "response_id": "R-3", "nps": 7 } }
                ])),
            )
            .await;
        assert!(first.ok, "{first:?}");
        assert_eq!(first.output["rows_applied"], 3, "{first:?}");
        assert_eq!(first.output["rows_matched"], 0);
        // 取り込み直し（2 件は同じ ID・1 件は新しい ID）。
        let again = env
            .run(
                &tenant,
                "alice",
                "data.bulk_upsert",
                bulk(json!([
                    { "data": { "response_id": "R-1", "nps": 10 } },
                    { "data": { "response_id": "R-3", "nps": 7 } },
                    { "data": { "response_id": "R-4", "nps": 0 } }
                ])),
            )
            .await;
        assert!(again.ok, "{again:?}");
        assert_eq!(again.output["rows_applied"], 3, "{again:?}");
        assert_eq!(again.output["rows_matched"], 2);
        let rows = env.records(&alice, table).await;
        let mut got: Vec<(String, i64)> = rows
            .iter()
            .map(|r| {
                (
                    r.data["response_id"].as_str().unwrap().to_string(),
                    r.data["nps"].as_i64().unwrap(),
                )
            })
            .collect();
        got.sort();
        assert_eq!(
            got,
            vec![
                ("R-1".into(), 10),
                ("R-2".into(), 3),
                ("R-3".into(), 7),
                ("R-4".into(), 0)
            ]
        );
        // 同じ ID の行が 2 つあると、何も登録しない。
        let dup = env
            .run(
                &tenant,
                "alice",
                "data.bulk_upsert",
                bulk(json!([
                    { "data": { "response_id": "R-5", "nps": 1 } },
                    { "data": { "response_id": "R-5", "nps": 2 } }
                ])),
            )
            .await;
        assert_eq!(dup.code.as_deref(), Some("invalid"), "{dup:?}");
        assert!(
            dup.message
                .as_deref()
                .unwrap_or_default()
                .contains("1 件目と 2 件目"),
            "{dup:?}"
        );
        assert_eq!(env.records(&alice, table).await.len(), 4);
    };
    finish(&env, &tenant, body).await;
}
