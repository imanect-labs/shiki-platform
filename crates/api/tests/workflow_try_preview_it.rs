//! 試運転の下見（`GET /workflows/try-preview`・会話の AI の試運転の承認カード・D-197）の結合テスト。
//!
//! 実 Postgres ＋実 OpenFGA で、ルータ（セッション認証）を通して次を固定する。
//!
//! - 持ち主は `can_try=true` で、書くテーブルのうち本人が見られるものの名前が入る（見られないテーブルは名前なし）。
//! - 閲覧だけ共有された人は 200 で `can_try=false`（拒否の監査を残さない）。
//! - 見えない人・別のテナント・無い id・ワークフローでないものは、同じ 404（区別しない）。
//! - id だけを受ける（名前は 400）。版を省くと 400、無い版は 404。
//! - 共同編集者が保存した版は、`saved_by` が共同編集者になる（持ち主と分けて出す）。
//! - `effects` は `RegistrationService::run_effects(ir)` に名前を入れたものと一致する（観測とカードの一致の片側）。
//!
//! `STORAGE_TEST_DATABASE_URL` と `OPENFGA_TEST_URL` が無ければスキップ（オブジェクトストアは使わない）。

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::print_stderr,
    clippy::pedantic,
    clippy::too_many_lines
)]

use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;

use api::{
    build_router,
    config::*,
    session::{MemorySessionStore, SessionStore},
    state::AppState,
};
use artifact::{ArtifactKind, NewArtifact};
use async_trait::async_trait;
use authz::client::{OpenFgaClient, OpenFgaConfig};
use authz::{AuthContext, AuthzClient, Principal, Relation};
use axum::body::Body;
use axum::http::{header::COOKIE, Request, StatusCode};
use data::{FieldDef, FieldType, NewDataTable, TableSchema};
use http_body_util::BodyExt;
use serde_json::{json, Value};
use sqlx::postgres::PgPoolOptions;
use tower::ServiceExt;
use uuid::Uuid;
use workflow_engine::registration::EffectTarget;
use workflow_engine::{RegistrationService, WorkflowIr};

const TENANT: &str = "default";

/// 利用者（セッション ID = 利用者 ID）。alice: 持ち主／bob: 閲覧だけ／carol: 共同編集者／dave: 権限なし。
const USERS: [&str; 4] = ["alice", "bob", "carol", "dave"];

/// no-op ObjectStore（AppState を満たすためだけ・本テストはオブジェクトに触れない）。
struct NoopStore;

#[async_trait]
impl storage::object_store::ObjectStore for NoopStore {
    async fn ensure_bucket(&self) -> Result<(), storage::ObjectStoreError> {
        Ok(())
    }
    async fn presign_get_internal(
        &self,
        _k: &str,
        _t: Duration,
    ) -> Result<String, storage::ObjectStoreError> {
        Ok("http://noop".into())
    }
    async fn presign_put(
        &self,
        _k: &str,
        _t: Duration,
        _l: i64,
    ) -> Result<String, storage::ObjectStoreError> {
        Ok("http://noop".into())
    }
    async fn presign_get(
        &self,
        _k: &str,
        _t: Duration,
        _f: Option<&str>,
        _c: Option<&str>,
    ) -> Result<String, storage::ObjectStoreError> {
        Ok("http://noop".into())
    }
    async fn read_and_hash(&self, _k: &str) -> Result<(String, u64), storage::ObjectStoreError> {
        Err(storage::ObjectStoreError::NotFound("noop".into()))
    }
    async fn put_object(
        &self,
        _k: &str,
        _b: Vec<u8>,
        _c: &str,
    ) -> Result<(), storage::ObjectStoreError> {
        Ok(())
    }
    async fn get_object(&self, _k: &str) -> Result<Vec<u8>, storage::ObjectStoreError> {
        Err(storage::ObjectStoreError::NotFound("noop".into()))
    }
    async fn exists(&self, _k: &str) -> Result<bool, storage::ObjectStoreError> {
        Ok(false)
    }
    async fn copy(&self, _s: &str, _d: &str) -> Result<(), storage::ObjectStoreError> {
        Ok(())
    }
    async fn delete(&self, _k: &str) -> Result<(), storage::ObjectStoreError> {
        Ok(())
    }
    async fn list_prefix(
        &self,
        _p: &str,
        _c: Option<&str>,
    ) -> Result<(Vec<String>, Option<String>), storage::ObjectStoreError> {
        Ok((vec![], None))
    }
    async fn delete_batch(&self, _k: &[String]) -> Result<(), storage::ObjectStoreError> {
        Ok(())
    }
}

fn config(db_url: &str) -> AppConfig {
    AppConfig {
        bug_report: Default::default(),
        server: ServerConfig {
            host: "0.0.0.0".into(),
            port: 0,
            cors_allowed_origins: vec![],
        },
        database: DatabaseConfig {
            url: db_url.into(),
            migrate_url: None,
            max_connections: 5,
        },
        auth: AuthConfig {
            issuer: "http://localhost/realms/shiki".into(),
            internal_base_url: None,
            jwks_uri: None,
            audience: "shiki-api".into(),
            jwks_ttl_secs: 300,
            client_id: "shiki-web".into(),
            client_secret: None,
            redirect_uri: "http://localhost:3000/auth/callback".into(),
            post_logout_redirect_uri: "http://localhost:3000/".into(),
            scopes: "openid profile".into(),
            tenancy: Tenancy::Single,
            tenant_id: Some(TENANT.into()),
            provisioner_client_id: None,
            provisioner_client_secret: None,
            admin_base_url: None,
        },
        authz: AuthzConfig {
            base_url: "http://localhost:8080".into(),
            store_name: "shiki".into(),
        },
        session: SessionConfig {
            redis_url: "redis://localhost:6379".into(),
            ttl_secs: 86400,
            secure: false,
            refresh_leeway_secs: 60,
        },
        telemetry: TelemetryConfig {
            otlp_endpoint: None,
            service_name: "test".into(),
            log_format: LogFormat::Json,
        },
        storage: StorageConfig {
            backend: ObjectStoreBackend::S3,
            s3: None,
            max_upload_size_bytes: 5 * 1024 * 1024 * 1024,
            upload_gc: api::config::UploadGcConfig::default(),
            audit_retention: api::config::AuditRetentionConfig::default(),
        },
        vector: VectorConfig {
            backend: VectorStoreBackend::Qdrant,
        },
        rag: rag::RagConfig::default(),
        help: rag::help::HelpConfig::default(),
        llm: LlmConfig {
            backend: LlmBackend::Stub,
            base_url: None,
            api_key: None,
            default_model: None,
            models: vec![],
            context_window_tokens: None,
            langfuse: None,
            user_agent: None,
            session_header: None,
            ..Default::default()
        },
        chat: api::config::ChatConfig::default(),
        websearch: api::config::WebSearchConfig::default(),
        secrets: api::config::SecretsConfig::default(),
        data: api::config::DataConfig::default(),
        workflow: api::workflow_runtime::WorkflowConfig::default(),
        gateway: api::config::GatewayConfig::default(),
        tabular: api::config::TabularConfig::default(),
        office: api::config::OfficeConfig::default(),
        insights: api::insights::InsightsConfig::default(),
        membership_reconcile: api::membership_reconcile::MembershipReconcileConfig::default(),
        egress: Default::default(),
    }
}

struct Env {
    app: axum::Router,
    pool: sqlx::PgPool,
    fga: Arc<dyn AuthzClient>,
    data: Arc<data::DataStore>,
    artifacts: Arc<artifact::ArtifactStore>,
}

fn principal(id: &str) -> Principal {
    Principal {
        kind: authz::PrincipalKind::User,
        id: id.into(),
        email: None,
        groups: vec!["/acme".into()],
        roles: vec![],
        tenant_id: None,
    }
}

fn ctx_in(tenant: &str, id: &str) -> AuthContext {
    AuthContext::new(
        principal(id),
        "acme".into(),
        authz::TenantId::parse(tenant).unwrap(),
    )
}

fn ctx(id: &str) -> AuthContext {
    ctx_in(TENANT, id)
}

async fn setup() -> Option<Env> {
    let (Ok(db_url), Ok(fga_url)) = (
        shiki_testenv::var("STORAGE_TEST_DATABASE_URL"),
        shiki_testenv::var("OPENFGA_TEST_URL"),
    ) else {
        eprintln!("STORAGE_TEST_DATABASE_URL / OPENFGA_TEST_URL 未設定のためスキップ");
        return None;
    };
    let pool = PgPoolOptions::new()
        .max_connections(8)
        .connect(&db_url)
        .await
        .expect("pg");
    sqlx::migrate!("../../migrations")
        .run(&pool)
        .await
        .expect("migrate");
    let fga: Arc<dyn AuthzClient> = Arc::new(
        OpenFgaClient::connect(
            reqwest::Client::new(),
            &OpenFgaConfig {
                base_url: fga_url,
                store_name: format!("shiki-try-preview-{}", Uuid::new_v4()),
            },
            &authz::model::default_model(),
        )
        .await
        .expect("fga"),
    );
    let storage = Arc::new(storage::StorageService::new(
        pool.clone(),
        Arc::new(NoopStore) as Arc<dyn storage::ObjectStore>,
        Arc::clone(&fga),
        Duration::from_secs(120),
        Duration::from_secs(900),
        64 * 1024 * 1024,
    ));
    let sessions = Arc::new(MemorySessionStore::new());
    for u in USERS {
        sessions
            .put(
                TENANT,
                &format!("sid-{u}"),
                &api::session::SessionRecord {
                    principal: principal(u),
                    tenant_id: TENANT.into(),
                    access_token: "a".into(),
                    refresh_token: None,
                    id_token: None,
                    access_expires_at: chrono::Utc::now().timestamp() + 3600,
                    csrf_token: format!("csrf-{u}"),
                    keycloak_sid: None,
                },
                Duration::from_secs(3600),
            )
            .await
            .unwrap();
    }
    let directory = Arc::new(storage::DirectoryStore::new(pool.clone()));
    let data_store = Arc::new(data::DataStore::new(
        pool.clone(),
        Arc::clone(&fga),
        Arc::new(api::data_refs::ApiRefResolver {
            directory: Arc::clone(&directory),
            storage: Arc::clone(&storage),
        }),
    ));
    let artifacts = Arc::new(artifact::ArtifactStore::new(pool.clone(), Arc::clone(&fga)));
    let app = build_app(
        &pool,
        config(&db_url),
        storage,
        sessions,
        &fga,
        directory,
        Arc::clone(&data_store),
        Arc::clone(&artifacts),
    );
    Some(Env {
        app,
        pool,
        fga,
        data: data_store,
        artifacts,
    })
}

#[allow(clippy::too_many_arguments)]
fn build_app(
    pool: &sqlx::PgPool,
    config: AppConfig,
    storage: Arc<storage::StorageService>,
    sessions: Arc<MemorySessionStore>,
    authz: &Arc<dyn AuthzClient>,
    directory: Arc<storage::DirectoryStore>,
    data_store: Arc<data::DataStore>,
    artifacts: Arc<artifact::ArtifactStore>,
) -> axum::Router {
    let tabular = Arc::new(tabular::TabularService::new(
        Arc::clone(&storage),
        tabular::RunnerConfig::new("/nonexistent/shiki-tabular-runner", Duration::from_secs(5)),
        tabular::Quotas::default(),
    ));
    let collab = Arc::new(collab::CollabHub::new(
        pool.clone(),
        Arc::clone(authz),
        Arc::clone(&storage),
    ));
    let mini_app_code = Arc::new(app_platform::MiniAppCodeStore::new(
        Arc::clone(&artifacts),
        app_platform::Registry::new(pool.clone()),
    ));
    let installs = Arc::new(app_platform::InstallService::new(
        pool.clone(),
        app_platform::Registry::new(pool.clone()),
        Arc::clone(&mini_app_code),
        Arc::clone(&data_store),
        Arc::clone(authz),
        None,
        vec![],
    ));
    let skill_installs = Arc::new(app_platform::SkillInstallService::new(
        pool.clone(),
        app_platform::Registry::new(pool.clone()),
        app_platform::TrustedKeyStore::new(pool.clone()),
        Arc::clone(&artifacts),
        Arc::clone(authz),
    ));
    let bundles = Arc::new(app_platform::BundleStore::new(
        Arc::new(NoopStore),
        Arc::clone(authz),
        storage::audit::AuditRecorder::new(pool.clone()),
    ));
    let app_usage = Arc::new(app_platform::AppUsageStore::new(
        pool.clone(),
        Arc::clone(authz),
    ));
    let state = AppState {
        bug_report: None,
        config: Arc::new(config),
        insights: Arc::new(api::insights::InsightsService::new(pool.clone())),
        db: api::state::ReadinessProbe::new(pool.clone()),
        authz: Arc::clone(authz),
        jwks: Arc::new(api::middleware::JwksCache::new(
            reqwest::Client::new(),
            "http://localhost/jwks".into(),
            Duration::from_secs(300),
        )),
        sessions,
        http: reqwest::Client::new(),
        storage: Arc::clone(&storage),
        collab,
        tabular,
        artifacts: Arc::clone(&artifacts),
        data: Arc::clone(&data_store),
        data_views: Arc::new(data::DataViewStore::new(
            Arc::clone(&artifacts),
            (*data_store).clone(),
        )),
        fsms: Arc::new(data::FsmStore::new(
            Arc::clone(&artifacts),
            (*data_store).clone(),
        )),
        mini_app_code: Arc::clone(&mini_app_code),
        installs,
        skill_installs,
        bundles,
        app_usage,
        ui_actions: Arc::new(gui::ActionDispatcher::new(
            storage::audit::AuditRecorder::new(pool.clone()),
        )),
        skills: Arc::new(gui::SkillStore::new(Arc::clone(&artifacts))),
        mini_apps: Arc::new(gui::MiniAppStore::new(Arc::clone(&artifacts), pool.clone())),
        secrets: None,
        workflows: Arc::new(workflow_engine::WorkflowStore::new(Arc::clone(&artifacts))),
        workflow_launcher: None,
        workflow_runs: None,
        workflow_registration: Arc::new(workflow_engine::RegistrationService::new(
            pool.clone(),
            workflow_engine::DelegationStore::new(pool.clone(), Arc::clone(authz)),
        )),
        workflow_summaries: Arc::new(workflow_engine::WorkflowSummaryStore::new(pool.clone())),
        workflow_layout: Arc::new(workflow_engine::EditorLayoutStore::new(pool.clone())),
        workflow_approvals: Arc::new(workflow_engine::ApprovalStore::new(pool.clone())),
        audit: Arc::new(storage::audit::AuditRecorder::new(pool.clone())),
        directory,
        tenants: Arc::new(storage::TenantStore::new(pool.clone())),
        search: None,
        chat: None,
        llm: None,
        rag_admin: Arc::new(rag::RagAdmin::new(pool.clone(), None, None)),
        office: None,
        egress_proxy: None,
        docx_composer: Arc::new(office::DocxComposer::new(
            reqwest::Client::new(),
            "http://127.0.0.1:1",
        )),
    };
    build_router(state)
}

/// 本人のセッションで GET する。
async fn get(env: &Env, user: &str, path: &str) -> (StatusCode, Value) {
    let r = Request::builder()
        .method("GET")
        .uri(path)
        .header(
            COOKIE,
            format!("shiki_session=sid-{user}.{TENANT}; shiki_csrf=csrf-{user}"),
        )
        .body(Body::empty())
        .unwrap();
    let resp = env.app.clone().oneshot(r).await.unwrap();
    let status = resp.status();
    let bytes = resp.into_body().collect().await.unwrap().to_bytes();
    (
        status,
        serde_json::from_slice(&bytes).unwrap_or(Value::Null),
    )
}

fn preview_path(workflow: &str, version: &str) -> String {
    format!("/workflows/try-preview?workflow={workflow}&version={version}")
}

/// `owner` が持つテーブル（1 列だけ）。
async fn table(env: &Env, owner: &str, name: &str) -> data::DataTable {
    let schema = TableSchema {
        fields: vec![FieldDef::new("title", FieldType::Text)],
        status_field: None,
        row_policy: None,
        field_policy: vec![],
        aggregate_min_rows: None,
        fsm_ref: None,
        rag: None,
    };
    env.data
        .create_table(
            &ctx(owner),
            NewDataTable {
                name: format!("{name}-{}", Uuid::new_v4().simple()),
                schema,
            },
            None,
        )
        .await
        .unwrap()
}

/// 参照名（IR の name の形・テナントの中で一意）。
fn unique_name(prefix: &str) -> String {
    format!("{prefix}-{}", &Uuid::new_v4().simple().to_string()[..12])
}

/// ワークフローの本文（保存の検証は通さない・下見は本文を読むだけ）。
fn ir_body(name: &str, visible_table: Uuid, hidden_table: Uuid, child: &str) -> Value {
    json!({
        "ir_version": 1,
        "name": name,
        "declared_scopes": ["data.write", "net.http", "workflow.start", "llm.invoke"],
        "triggers": [{ "kind": "interactive" }],
        "nodes": [
            { "id": "add", "type": "data.record.create",
              "params": { "table": visible_table.to_string(), "values": { "title": "x" } } },
            { "id": "fix", "type": "data.record.update",
              "params": { "table": hidden_table.to_string(), "record_id": "r", "values": {} } },
            { "id": "dyn", "type": "data.record.delete",
              "params": { "table": { "$from": "input", "path": "table" }, "record_id": "r" } },
            { "id": "named", "type": "data.record.create",
              "params": { "table": "orders-by-name", "values": { "title": "x" } } },
            { "id": "call", "type": "http.request",
              "params": { "url": "https://API.example.com/hook", "method": "post" } },
            { "id": "child", "type": "workflow.start", "params": { "name": child } },
            { "id": "ghost", "type": "workflow.start", "params": { "name": "no-such-flow" } },
            { "id": "again", "type": "workflow.start", "params": { "name": child } },
            { "id": "ask", "type": "llm.invoke", "params": { "prompt": "x" } }
        ],
        "edges": []
    })
}

/// 持ち主（alice）が作ったワークフロー一式。
struct Fixture {
    id: Uuid,
    name: String,
    body: Value,
    visible: data::DataTable,
    hidden: data::DataTable,
    child: Uuid,
    child_name: String,
}

async fn fixture(env: &Env) -> Fixture {
    // alice が持つテーブルと、dave が持つ（alice には見えない）テーブル。
    let visible = table(env, "alice", "orders").await;
    let hidden = table(env, "dave", "salaries").await;
    let alice = ctx("alice");
    let child_name = unique_name("child");
    let child = env
        .artifacts
        .create(
            &alice,
            NewArtifact {
                kind: ArtifactKind::Workflow,
                name: child_name.clone(),
                body: json!({ "ir_version": 1, "name": child_name, "nodes": [], "edges": [] }),
            },
            None,
        )
        .await
        .unwrap()
        .id;
    let name = unique_name("try-flow");
    let body = ir_body(&name, visible.id, hidden.id, &child_name);
    let id = env
        .artifacts
        .create(
            &alice,
            NewArtifact {
                kind: ArtifactKind::Workflow,
                name: name.clone(),
                body: body.clone(),
            },
            None,
        )
        .await
        .unwrap()
        .id;
    Fixture {
        id,
        name,
        body,
        visible,
        hidden,
        child,
        child_name,
    }
}

/// ワークフローを `user` に `relation` で共有する。
async fn share(env: &Env, id: Uuid, user: &str, relation: Relation) {
    let c = ctx(user);
    env.fga
        .write_tuple(&c.subject(), relation, &c.ns().artifact(&id.to_string()))
        .await
        .unwrap();
}

/// 期待する副作用（エンジンの純関数に、見られるテーブルの名前だけを入れたもの）。
fn expected_effects(body: &Value, names: &[(Uuid, &str)]) -> Value {
    let ir = WorkflowIr::from_json(body).unwrap();
    let mut effects = RegistrationService::run_effects(&ir);
    let names: HashMap<String, String> = names
        .iter()
        .map(|(id, n)| (id.to_string(), (*n).to_string()))
        .collect();
    effects.set_target_names(EffectTarget::Table, &names);
    serde_json::to_value(effects).unwrap()
}

/// 本人の試運転の拒否の監査（`workflow.try_run`）の行数。
async fn try_run_audits(env: &Env, user: &str, id: Uuid) -> i64 {
    sqlx::query_scalar(
        "SELECT count(*) FROM audit_log WHERE tenant_id = $1 AND actor = $2 \
         AND action = 'workflow.try_run' AND object_id = $3",
    )
    .bind(TENANT)
    .bind(user)
    .bind(id.to_string())
    .fetch_one(&env.pool)
    .await
    .unwrap()
}

/// 持ち主: 試運転でき、書くテーブルのうち見られるものの名前が入る。副作用は engine の純関数と一致する。
#[tokio::test]
async fn owner_can_try_and_sees_names_of_tables_it_can_see() {
    let Some(env) = setup().await else { return };
    let f = fixture(&env).await;
    let (s, b) = get(&env, "alice", &preview_path(&f.id.to_string(), "1")).await;
    assert_eq!(s, StatusCode::OK, "{b}");
    assert_eq!(b["workflow_id"], f.id.to_string());
    assert_eq!(b["name"], f.name);
    assert_eq!(b["version"], 1);
    assert_eq!(b["latest_version"], 1);
    assert_eq!(b["owner"], "alice");
    assert_eq!(b["owned_by_me"], true);
    assert_eq!(b["saved_by"], "alice");
    assert_eq!(b["saved_by_me"], true);
    assert!(b["saved_at"].as_str().is_some(), "{b}");
    assert_eq!(b["can_try"], true);

    // 見られるテーブルは名前つき、見られないテーブルは id だけ、値の式は実行のときに決まる。
    let writes = b["effects"]["writes"].as_array().unwrap();
    let by_node = |n: &str| writes.iter().find(|w| w["node_id"] == n).unwrap().clone();
    assert_eq!(by_node("add")["target_name"], f.visible.name.as_str());
    assert_eq!(by_node("add")["target_id"], f.visible.id.to_string());
    assert_eq!(by_node("fix")["target_name"], Value::Null);
    assert_eq!(by_node("fix")["target_id"], f.hidden.id.to_string());
    assert_eq!(by_node("dyn")["target_id"], Value::Null);
    assert_eq!(by_node("dyn")["decided_at_run"], true);
    // id（UUID）でない書き方のテーブルは名前を引かない（下見は IR の文字のまま）。
    assert_eq!(by_node("named")["target_name"], Value::Null);
    // ホストは IR のリテラルのまま（小文字にそろえるのは engine）。
    assert_eq!(
        b["effects"]["outbound"][0]["hosts"],
        json!(["api.example.com"])
    );

    // 副作用の全体が engine の純関数（RegistrationService::run_effects）と一致する。
    assert_eq!(
        b["effects"],
        expected_effects(&f.body, &[(f.visible.id, f.visible.name.as_str())])
    );

    // workflow.start の起動先は、同じ名前を 1 つにまとめ、見られるものに id を添える。
    assert_eq!(
        b["starts"],
        json!([
            { "name": f.child_name, "workflow_id": f.child.to_string() },
            { "name": "no-such-flow", "workflow_id": null },
        ])
    );
}

/// 閲覧だけ共有された人: 200 で can_try=false。拒否の監査は残さない（表示のためだけの判定）。
/// 名前は本人の見え方で引く（共有されていないテーブル・起動先は名前も id も入らない）。
#[tokio::test]
async fn viewer_gets_the_preview_but_cannot_try() {
    let Some(env) = setup().await else { return };
    let f = fixture(&env).await;
    share(&env, f.id, "bob", Relation::Viewer).await;
    let (s, b) = get(&env, "bob", &preview_path(&f.id.to_string(), "1")).await;
    assert_eq!(s, StatusCode::OK, "{b}");
    assert_eq!(b["can_try"], false);
    assert_eq!(b["owner"], "alice");
    assert_eq!(b["owned_by_me"], false);
    assert_eq!(b["saved_by_me"], false);
    assert_eq!(b["effects"], expected_effects(&f.body, &[]));
    assert_eq!(
        b["starts"],
        json!([
            { "name": f.child_name, "workflow_id": null },
            { "name": "no-such-flow", "workflow_id": null },
        ])
    );
    assert_eq!(try_run_audits(&env, "bob", f.id).await, 0);

    // テーブルを共有されると、名前が入る。
    let bob = ctx("bob");
    env.fga
        .write_tuple(
            &bob.subject(),
            Relation::Viewer,
            &bob.ns().data_table(&f.visible.id.to_string()),
        )
        .await
        .unwrap();
    let (s, b) = get(&env, "bob", &preview_path(&f.id.to_string(), "1")).await;
    assert_eq!(s, StatusCode::OK, "{b}");
    assert_eq!(
        b["effects"],
        expected_effects(&f.body, &[(f.visible.id, f.visible.name.as_str())])
    );
}

/// 見えない人・別のテナント・無い id・ワークフローでないもの・無い版は、どれも同じ 404。
#[tokio::test]
async fn hidden_missing_foreign_and_non_workflows_are_the_same_404() {
    let Some(env) = setup().await else { return };
    let f = fixture(&env).await;
    let (s, hidden) = get(&env, "dave", &preview_path(&f.id.to_string(), "1")).await;
    assert_eq!(s, StatusCode::NOT_FOUND, "{hidden}");

    let (s, missing) = get(
        &env,
        "alice",
        &preview_path(&Uuid::new_v4().to_string(), "1"),
    )
    .await;
    assert_eq!(s, StatusCode::NOT_FOUND, "{missing}");
    assert_eq!(missing, hidden);

    // 別のテナントで同じ id の利用者（alice）が持つワークフロー。
    let other = ctx_in(&format!("t-{}", Uuid::new_v4().simple()), "alice");
    let foreign_name = unique_name("foreign");
    let foreign = env
        .artifacts
        .create(
            &other,
            NewArtifact {
                kind: ArtifactKind::Workflow,
                name: foreign_name.clone(),
                body: json!({ "ir_version": 1, "name": foreign_name, "nodes": [], "edges": [] }),
            },
            None,
        )
        .await
        .unwrap();
    let (s, b) = get(&env, "alice", &preview_path(&foreign.id.to_string(), "1")).await;
    assert_eq!(s, StatusCode::NOT_FOUND, "{b}");
    assert_eq!(b, hidden);

    // ワークフローでないアーティファクト（スキル）。
    let skill = env
        .artifacts
        .create(
            &ctx("alice"),
            NewArtifact {
                kind: ArtifactKind::Skill,
                name: unique_name("skill"),
                body: json!({ "x": 1 }),
            },
            None,
        )
        .await
        .unwrap();
    let (s, b) = get(&env, "alice", &preview_path(&skill.id.to_string(), "1")).await;
    assert_eq!(s, StatusCode::NOT_FOUND, "{b}");
    assert_eq!(b, hidden);

    // 無い版。
    let (s, b) = get(&env, "alice", &preview_path(&f.id.to_string(), "99")).await;
    assert_eq!(s, StatusCode::NOT_FOUND, "{b}");
    assert_eq!(b, hidden);
}

/// id だけを受ける: 名前（そのワークフローの本当の名前でも）は 400。版を省く・整数でないのも 400。
/// UUID の書き方（大文字・前後の空白）が違っても同じ workflow_id になる。
#[tokio::test]
async fn only_ids_are_accepted_and_the_version_is_required() {
    let Some(env) = setup().await else { return };
    let f = fixture(&env).await;
    for (path, field) in [
        (preview_path(&f.name, "1"), "workflow"),
        ("/workflows/try-preview?version=1".to_string(), "workflow"),
        (
            format!("/workflows/try-preview?workflow={}", f.id),
            "version",
        ),
        (preview_path(&f.id.to_string(), ""), "version"),
        (preview_path(&f.id.to_string(), "latest"), "version"),
    ] {
        let (s, b) = get(&env, "alice", &path).await;
        assert_eq!(s, StatusCode::BAD_REQUEST, "{path}: {b}");
        assert_eq!(b["code"], "invalid_input", "{path}: {b}");
        assert_eq!(b["field"], field, "{path}: {b}");
    }
    let upper = f.id.to_string().to_uppercase();
    let (s, b) = get(&env, "alice", &preview_path(&format!("%20{upper}%20"), "1")).await;
    assert_eq!(s, StatusCode::OK, "{b}");
    assert_eq!(b["workflow_id"], f.id.to_string());
}

/// 共同編集者が保存した版: この版を保存した人は共同編集者で、持ち主とは分けて出す。共同編集者も試運転できる。
#[tokio::test]
async fn a_version_saved_by_a_coeditor_names_the_coeditor() {
    let Some(env) = setup().await else { return };
    let f = fixture(&env).await;
    share(&env, f.id, "carol", Relation::Editor).await;
    let mut v2 = f.body.clone();
    v2["nodes"].as_array_mut().unwrap().push(json!(
        { "id": "send", "type": "http.request", "params": { "url": "https://evil.example.net/x" } }
    ));
    let saved = env
        .artifacts
        .append_version(&ctx("carol"), f.id, v2.clone(), Some(1), None)
        .await
        .unwrap();
    assert_eq!(saved.version, 2);

    let (s, b) = get(&env, "alice", &preview_path(&f.id.to_string(), "2")).await;
    assert_eq!(s, StatusCode::OK, "{b}");
    assert_eq!(b["owner"], "alice");
    assert_eq!(b["owned_by_me"], true);
    assert_eq!(b["saved_by"], "carol");
    assert_eq!(b["saved_by_me"], false);
    assert_eq!(b["version"], 2);
    assert_eq!(b["latest_version"], 2);
    assert_eq!(
        b["effects"],
        expected_effects(&v2, &[(f.visible.id, f.visible.name.as_str())])
    );

    // 前の版は持ち主が保存したまま（版ごとに保存した人を出す）。最新の版は 2。
    let (s, b) = get(&env, "alice", &preview_path(&f.id.to_string(), "1")).await;
    assert_eq!(s, StatusCode::OK, "{b}");
    assert_eq!(b["saved_by"], "alice");
    assert_eq!(b["version"], 1);
    assert_eq!(b["latest_version"], 2);

    // 共同編集者から見ると、持ち主ではないが自分の保存した版で、試運転できる。
    let (s, b) = get(&env, "carol", &preview_path(&f.id.to_string(), "2")).await;
    assert_eq!(s, StatusCode::OK, "{b}");
    assert_eq!(b["can_try"], true);
    assert_eq!(b["owned_by_me"], false);
    assert_eq!(b["saved_by_me"], true);
}

/// カードは id に結び付く（名前を引き直さない・TOCTOU を作らない）。持ち主が消した後に、別の人が同じ名前で
/// 作り直しても、前の id の下見は 404 のままで、新しいワークフローの下見は別の id・別の持ち主として出る。
#[tokio::test]
async fn the_preview_stays_bound_to_the_id_when_the_name_is_reused() {
    let Some(env) = setup().await else { return };
    let f = fixture(&env).await;
    let (s, before) = get(&env, "alice", &preview_path(&f.id.to_string(), "1")).await;
    assert_eq!(s, StatusCode::OK, "{before}");

    env.artifacts
        .delete(&ctx("alice"), f.id, None)
        .await
        .unwrap();
    let reused = env
        .artifacts
        .create(
            &ctx("carol"),
            NewArtifact {
                kind: ArtifactKind::Workflow,
                name: f.name.clone(),
                body: json!({ "ir_version": 1, "name": f.name, "nodes": [], "edges": [] }),
            },
            None,
        )
        .await
        .unwrap();
    assert_ne!(reused.id, f.id);
    share(&env, reused.id, "alice", Relation::Editor).await;

    let (s, gone) = get(&env, "alice", &preview_path(&f.id.to_string(), "1")).await;
    assert_eq!(s, StatusCode::NOT_FOUND, "{gone}");
    let (s, b) = get(&env, "alice", &preview_path(&reused.id.to_string(), "1")).await;
    assert_eq!(s, StatusCode::OK, "{b}");
    assert_eq!(b["workflow_id"], reused.id.to_string());
    assert_eq!(b["name"], f.name);
    assert_eq!(b["owner"], "carol");
    assert_eq!(b["owned_by_me"], false);
    assert_eq!(b["saved_by"], "carol");
    assert_eq!(b["can_try"], true);
    assert_eq!(b["effects"]["writes"], json!([]));
}
