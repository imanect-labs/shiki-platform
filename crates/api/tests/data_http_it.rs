//! 構造化データの HTTP ルートの結合テスト（実 Postgres ＋実 OpenFGA・監査 B2-6 / B2-8）。
//!
//! ルータ（認証 middleware・組織管理者ゲート・CSRF・本文の上限）を通して、次のルートの認可とエラーの形を固定する。
//!
//! - 一括取込（`/data/tables/{id}/imports*`）: editor だけが始められ、CSV / JSON の行を 202 で受ける。
//!   入力の形の誤りは構造化 400（`import_invalid`）、本文の上限超過は構造化 413（`import_too_large`）。
//!   進捗と行エラー（keyset）を editor が引ける。監査に残る。
//! - 管理（`/org-admin/data/*`）: 組織管理者だけ（それ以外は 403）。推定値・計画名を返さない。
//! - 索引（`/data/tables/{id}/indexes`）: viewer が状態を引ける・昇格できる。未知の列は 400、権限なしは 403。
//! - 配信（`/data/tables/{id}/stream`・`/awareness`）: viewer だけ。`Last-Event-ID` の不正は 400。
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

use std::sync::Arc;
use std::time::Duration;

use api::{
    build_router,
    config::*,
    session::{MemorySessionStore, SessionStore},
    state::AppState,
};
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

const TENANT: &str = "default";

/// no-op ObjectStore（BundleStore の AppState 充足用・本テストは bundle 面を叩かない）。
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
        server: ServerConfig {
            host: "0.0.0.0".into(),
            port: 0,
            cors_allowed_origins: vec![],
        },
        database: DatabaseConfig {
            url: db_url.into(),
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
            tenant_id: Some("default".into()),
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
    }
}

struct Env {
    app: axum::Router,
    pool: sqlx::PgPool,
    fga: Arc<dyn AuthzClient>,
    data: Arc<data::DataStore>,
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

fn ctx(id: &str) -> AuthContext {
    AuthContext::new(principal(id), "acme".into(), TENANT.into())
}

/// 利用者（セッション ID = 利用者 ID）。alice: owner／bob: viewer／carol: 権限なし／admin: 組織管理者。
const USERS: [&str; 4] = ["alice", "bob", "carol", "admin"];

async fn setup() -> Option<Env> {
    let (Ok(db_url), Ok(fga_url)) = (
        std::env::var("STORAGE_TEST_DATABASE_URL"),
        std::env::var("OPENFGA_TEST_URL"),
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
                store_name: format!("shiki-data-http-{}", Uuid::new_v4()),
            },
            &authz::model::default_model(),
        )
        .await
        .expect("fga"),
    );
    let admin = ctx("admin");
    fga.write_tuple(
        &admin.subject(),
        Relation::Admin,
        &admin.ns().organization("acme"),
    )
    .await
    .expect("org admin");
    let storage = Arc::new(storage::StorageService::new(
        pool.clone(),
        Arc::new(NoopStore) as Arc<dyn storage::ObjectStore>,
        Arc::clone(&fga),
        Duration::from_secs(120),
        Duration::from_secs(900),
        64 * 1024 * 1024,
    ));
    let tabular = Arc::new(tabular::TabularService::new(
        Arc::clone(&storage),
        tabular::RunnerConfig::new("/nonexistent/shiki-tabular-runner", Duration::from_secs(5)),
        tabular::Quotas::default(),
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
    let app = build_app(
        &pool,
        config(&db_url),
        storage,
        tabular,
        sessions,
        &fga,
        directory,
        Arc::clone(&data_store),
    );
    Some(Env {
        app,
        pool,
        fga,
        data: data_store,
    })
}

#[allow(clippy::too_many_arguments)]
fn build_app(
    pool: &sqlx::PgPool,
    config: AppConfig,
    storage: Arc<storage::StorageService>,
    tabular: Arc<tabular::TabularService>,
    sessions: Arc<MemorySessionStore>,
    authz: &Arc<dyn AuthzClient>,
    directory: Arc<storage::DirectoryStore>,
    data_store: Arc<data::DataStore>,
) -> axum::Router {
    let artifacts = Arc::new(artifact::ArtifactStore::new(
        pool.clone(),
        Arc::clone(authz),
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
        config: Arc::new(config),
        insights: std::sync::Arc::new(api::insights::InsightsService::new(pool.clone())),
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
        audit: Arc::new(storage::audit::AuditRecorder::new(pool.clone())),
        directory,
        tenants: Arc::new(storage::TenantStore::new(pool.clone())),
        search: None,
        chat: None,
        llm: None,
        rag_admin: Arc::new(rag::RagAdmin::new(pool.clone(), None, None)),
        office: None,
        docx_composer: std::sync::Arc::new(office::DocxComposer::new(
            reqwest::Client::new(),
            "http://127.0.0.1:1",
        )),
    };
    build_router(state)
}

fn req(method: &str, user: &str, path: &str, ct: &str, body: Body) -> Request<Body> {
    Request::builder()
        .method(method)
        .uri(path)
        .header(
            COOKIE,
            format!("shiki_session=sid-{user}.{TENANT}; shiki_csrf=csrf-{user}"),
        )
        .header("x-csrf-token", format!("csrf-{user}"))
        .header("content-type", ct)
        .body(body)
        .unwrap()
}

fn get(user: &str, path: &str) -> Request<Body> {
    req("GET", user, path, "application/json", Body::empty())
}

fn post_json(user: &str, path: &str, v: &Value) -> Request<Body> {
    req(
        "POST",
        user,
        path,
        "application/json",
        Body::from(v.to_string()),
    )
}

fn put_json(user: &str, path: &str, v: &Value) -> Request<Body> {
    req(
        "PUT",
        user,
        path,
        "application/json",
        Body::from(v.to_string()),
    )
}

fn post_csv(user: &str, path: &str, csv: impl Into<Body>) -> Request<Body> {
    req("POST", user, path, "text/csv", csv.into())
}

async fn send(env: &Env, r: Request<Body>) -> (StatusCode, Value) {
    let resp = env.app.clone().oneshot(r).await.unwrap();
    let status = resp.status();
    let bytes = resp.into_body().collect().await.unwrap().to_bytes();
    (
        status,
        serde_json::from_slice(&bytes).unwrap_or(Value::Null),
    )
}

/// alice が owner のテーブル（bob を viewer にする）。
async fn table(env: &Env, name: &str) -> data::DataTable {
    let mut title = FieldDef::new("title", FieldType::Text);
    title.display_name = "件名".into();
    title.required = true;
    title.indexed = true;
    let mut amount = FieldDef::new("amount", FieldType::Number);
    amount.indexed = true;
    let mut tags = FieldDef::new("tags", FieldType::MultiSelect);
    tags.options = vec!["a".into(), "b".into()];
    let memo = FieldDef::new("memo", FieldType::Text);
    let schema = TableSchema {
        fields: vec![title, amount, tags, memo],
        status_field: None,
        row_policy: None,
        field_policy: vec![],
        aggregate_min_rows: None,
        fsm_ref: None,
        rag: None,
    };
    let alice = ctx("alice");
    let t = env
        .data
        .create_table(
            &alice,
            NewDataTable {
                name: format!("{name}-{}", Uuid::new_v4().simple()),
                schema,
            },
            None,
        )
        .await
        .unwrap();
    let bob = ctx("bob");
    env.fga
        .write_tuple(
            &bob.subject(),
            Relation::Viewer,
            &bob.ns().data_table(&t.id.to_string()),
        )
        .await
        .unwrap();
    t
}

/// 取込を最後まで進めて進捗を返す（ジョブの代わりに同期で回す）。
async fn finish(env: &Env, table: Uuid, import_id: Uuid) {
    let step = env
        .data
        .run_bulk_import(TENANT, table, import_id)
        .await
        .unwrap();
    assert!(step.done, "{step:?}");
}

/// 一括取込（監査 B2-6）: CSV / JSON の行で始め、進捗と行エラーを引ける。editor だけ。
#[tokio::test]
async fn bulk_import_routes_start_observe_and_authorize() {
    let Some(env) = setup().await else { return };
    let t = table(&env, "imports").await;
    let base = format!("/data/tables/{}/imports", t.id);

    // CSV（見出しは参照名と表示名・`_key` は行キー）。2 行目の金額は数値でない → その行だけ不合格。
    let csv =
        "_key,件名,amount,tags\nk1,出張,1200,a;b\nk2,宿泊,abc,\nk3,会議,300,\"[\"\"b\"\"]\"\n";
    let (s, b) = send(&env, post_csv("alice", &format!("{base}/csv"), csv)).await;
    assert_eq!(s, StatusCode::ACCEPTED, "{b}");
    let import_id: Uuid = serde_json::from_value(b["import_id"].clone()).unwrap();
    assert_eq!(b["status"]["status"], "validating");
    assert_eq!(b["status"]["rows_total"], 3);
    finish(&env, t.id, import_id).await;
    let (s, st) = send(&env, get("alice", &format!("{base}/{import_id}"))).await;
    assert_eq!(s, StatusCode::OK, "{st}");
    assert_eq!(st["status"], "done");
    assert_eq!(
        (st["rows_applied"].clone(), st["rows_invalid"].clone()),
        (json!(2), json!(1))
    );
    assert_eq!(st["parity_mismatches"], 0, "取込後の parity が一致");
    let text = st.to_string();
    assert!(
        !text.contains("record_int") && !text.contains("table_int"),
        "{text}"
    );
    let (s, errs) = send(&env, get("alice", &format!("{base}/{import_id}/errors"))).await;
    assert_eq!(s, StatusCode::OK, "{errs}");
    assert_eq!(errs["items"].as_array().unwrap().len(), 1);
    assert_eq!(errs["items"][0]["row_no"], 2);
    assert_eq!(errs["items"][0]["key"], "k2");
    assert!(errs["next_after_row"].is_null());
    let rows = env
        .data
        .list_records(
            &ctx("alice"),
            t.id,
            &data::ListRecordsOptions::default(),
            None,
        )
        .await
        .unwrap();
    assert_eq!(rows.items.len(), 2);

    // JSON の行（行キーの重複は形の誤り＝構造化 400）。
    let rows: Vec<Value> = (0..3)
        .map(|i| json!({ "key": format!("j{i}"), "data": { "title": format!("J{i}") } }))
        .collect();
    let (s, b) = send(
        &env,
        post_json(
            "alice",
            &base,
            &json!({ "rows": rows, "index_mode": "immediate", "abort_on_invalid": true }),
        ),
    )
    .await;
    assert_eq!(s, StatusCode::ACCEPTED, "{b}");
    let jid: Uuid = serde_json::from_value(b["import_id"].clone()).unwrap();
    finish(&env, t.id, jid).await;
    let (_, st) = send(&env, get("alice", &format!("{base}/{jid}"))).await;
    assert_eq!(
        (st["status"].clone(), st["rows_applied"].clone()),
        (json!("done"), json!(3))
    );
    let dup = json!({ "rows": [
        { "key": "x", "data": { "title": "1" } },
        { "key": "x", "data": { "title": "2" } }
    ]});
    let (s, b) = send(&env, post_json("alice", &base, &dup)).await;
    assert_eq!(s, StatusCode::BAD_REQUEST, "{b}");
    assert_eq!(b["code"], "import_invalid");
    assert!(b["message"].as_str().unwrap().contains("重複"), "{b}");
    let broken = req(
        "POST",
        "alice",
        &base,
        "application/json",
        Body::from("{\"rows\": ["),
    );
    let (s, b) = send(&env, broken).await;
    assert_eq!(
        (s, b["code"].clone()),
        (StatusCode::BAD_REQUEST, json!("import_invalid"))
    );
    let (s, b) = send(&env, post_json("alice", &base, &json!({ "rows": [] }))).await;
    assert_eq!(
        (s, b["code"].clone()),
        (StatusCode::BAD_REQUEST, json!("import_invalid"))
    );

    // CSV の未知の列は読めない列と同じ構造化 403 `field_unavailable`（列名を返さない・列の存在オラクルを
    // 作らない）。
    let (s, b) = send(
        &env,
        post_csv("alice", &format!("{base}/csv"), "title,nope\nx,y\n"),
    )
    .await;
    assert_eq!(s, StatusCode::FORBIDDEN, "{b}");
    assert_eq!(b["code"], "field_unavailable");
    assert!(!b.to_string().contains("nope"), "{b}");
    // 列の数の違いは利用者向けの文言（CSV ライブラリの英語を出さない）。
    let (s, b) = send(
        &env,
        post_csv("alice", &format!("{base}/csv"), "title,memo\nx\n"),
    )
    .await;
    assert_eq!(s, StatusCode::BAD_REQUEST, "{b}");
    assert!(!b["message"].as_str().unwrap().contains("CSV error"), "{b}");

    // 本文の上限を超えると構造化 413（ステージングに何も入らない）。
    let big = vec![b'x'; api::routes::data_imports::MAX_IMPORT_BODY_BYTES + 1];
    let (s, b) = send(&env, post_csv("alice", &format!("{base}/csv"), big)).await;
    assert_eq!(s, StatusCode::PAYLOAD_TOO_LARGE, "{b}");
    assert_eq!(b["code"], "import_too_large");

    // 認可: viewer（bob・403）・権限なし（carol・テーブルが見えないので存在しないのと同じ 404）は
    // 始められず、進捗も引けない。
    for (who, want) in [
        ("bob", StatusCode::FORBIDDEN),
        ("carol", StatusCode::NOT_FOUND),
    ] {
        let (s, _) = send(&env, post_csv(who, &format!("{base}/csv"), csv)).await;
        assert_eq!(s, want, "{who}");
        let (s, _) = send(&env, post_json(who, &base, &json!({ "rows": rows }))).await;
        assert_eq!(s, want, "{who}");
        let (s, _) = send(&env, get(who, &format!("{base}/{import_id}"))).await;
        assert_eq!(s, want, "{who}");
        let (s, _) = send(&env, get(who, &format!("{base}/{import_id}/errors"))).await;
        assert_eq!(s, want, "{who}");
    }
    // 別のテーブルの取込 ID・存在しない取込は 404。
    let other = table(&env, "imports-other").await;
    let (s, _) = send(
        &env,
        get(
            "alice",
            &format!("/data/tables/{}/imports/{import_id}", other.id),
        ),
    )
    .await;
    assert_eq!(s, StatusCode::NOT_FOUND);
    let (s, _) = send(&env, get("alice", &format!("{base}/{}", Uuid::new_v4()))).await;
    assert_eq!(s, StatusCode::NOT_FOUND);
    // CSRF が無い変更は通らない（セッションのルートと同じ規則）。
    let no_csrf = Request::builder()
        .method("POST")
        .uri(format!("{base}/csv"))
        .header(COOKIE, format!("shiki_session=sid-alice.{TENANT}"))
        .header("content-type", "text/csv")
        .body(Body::from(csv))
        .unwrap();
    let (s, _) = send(&env, no_csrf).await;
    assert_eq!(s, StatusCode::FORBIDDEN);

    // 監査: 開始が主体つきで残る。
    let n: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM audit_log WHERE tenant_id = $1 AND action = 'data.record.bulk_import' \
         AND object_id = $2 AND actor = 'alice'",
    )
    .bind(TENANT)
    .bind(t.id.to_string())
    .fetch_one(&env.pool)
    .await
    .unwrap();
    assert!(n >= 2, "CSV と JSON の開始が監査に残る: {n}");
}

/// 行エラーの keyset ページング（`next_after_row`）。
#[tokio::test]
async fn bulk_import_errors_are_paged_by_row_number() {
    let Some(env) = setup().await else { return };
    let t = table(&env, "import-errors").await;
    let base = format!("/data/tables/{}/imports", t.id);
    // 必須の件名が無い行が 5 つ。
    let csv = "amount\n1\n2\n3\n4\n5\n";
    let (s, b) = send(&env, post_csv("alice", &format!("{base}/csv"), csv)).await;
    assert_eq!(s, StatusCode::ACCEPTED, "{b}");
    let id: Uuid = serde_json::from_value(b["import_id"].clone()).unwrap();
    finish(&env, t.id, id).await;
    let mut seen = Vec::new();
    let mut after = 0;
    loop {
        let (s, page) = send(
            &env,
            get(
                "alice",
                &format!("{base}/{id}/errors?limit=2&after_row={after}"),
            ),
        )
        .await;
        assert_eq!(s, StatusCode::OK, "{page}");
        for r in page["items"].as_array().unwrap() {
            seen.push(r["row_no"].as_i64().unwrap());
        }
        match page["next_after_row"].as_i64() {
            Some(n) => after = n,
            None => break,
        }
    }
    assert_eq!(seen, vec![1, 2, 3, 4, 5]);
}

/// 管理（監査 B2-8）: `/org-admin/data/*` は組織管理者だけ。報告に推定値・計画名を含めない。
#[tokio::test]
async fn org_admin_data_routes_require_org_admin() {
    let Some(env) = setup().await else { return };
    let t = table(&env, "admin").await;
    let opt = format!("/org-admin/data/tables/{}/optimizer", t.id);
    // 管理者でない（owner でも）は 403。
    for who in ["alice", "carol"] {
        for r in [
            get(who, "/org-admin/data/optimizer"),
            get(who, &opt),
            put_json(who, &opt, &json!({ "fixed_plan": true })),
            get(who, "/org-admin/data/settings"),
            put_json(
                who,
                "/org-admin/data/settings",
                &json!({ "count_cap": 100 }),
            ),
        ] {
            let (s, _) = send(&env, r).await;
            assert_eq!(s, StatusCode::FORBIDDEN, "{who}");
        }
    }
    // 環境の状態（単価・較正の前後）。
    let (s, b) = send(&env, get("admin", "/org-admin/data/optimizer")).await;
    assert_eq!(s, StatusCode::OK, "{b}");
    assert!(b["unit_costs"]["c_probe"].as_f64().unwrap() > 0.0, "{b}");
    assert!(b.get("calibration").is_some());
    // 組織管理者でもテーブルの viewer でなければ、テーブルの報告・固定計画は「見つからない」
    // （管理者権限は閲覧権を含意しない・アンビエント権限の禁止・B-P3-6）。
    for r in [
        get("admin", &opt),
        put_json("admin", &opt, &json!({ "fixed_plan": true })),
    ] {
        let (s, _) = send(&env, r).await;
        assert_eq!(s, StatusCode::NOT_FOUND);
    }
    let admin = ctx("admin");
    env.fga
        .write_tuple(
            &admin.subject(),
            Relation::Viewer,
            &admin.ns().data_table(&t.id.to_string()),
        )
        .await
        .unwrap();
    // テーブルの報告と固定計画（監査に残る）。
    let (s, b) = send(&env, get("admin", &opt)).await;
    assert_eq!(s, StatusCode::OK, "{b}");
    assert_eq!(b["fixed_plan"], false);
    for banned in [
        "estimate",
        "strategy",
        "histogram",
        "predicted",
        "record_int",
    ] {
        assert!(!b.to_string().contains(banned), "{banned}: {b}");
    }
    let (s, b) = send(
        &env,
        put_json("admin", &opt, &json!({ "fixed_plan": true })),
    )
    .await;
    assert_eq!((s, b["fixed_plan"].clone()), (StatusCode::OK, json!(true)));
    let n: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM audit_log WHERE tenant_id = $1 AND action = 'data.table.fixed_plan' \
         AND object_id = $2",
    )
    .bind(TENANT)
    .bind(t.id.to_string())
    .fetch_one(&env.pool)
    .await
    .unwrap();
    assert_eq!(n, 1);
    let (s, _) = send(
        &env,
        get(
            "admin",
            &format!("/org-admin/data/tables/{}/optimizer", Uuid::new_v4()),
        ),
    )
    .await;
    assert_eq!(s, StatusCode::NOT_FOUND);
    let (s, b) = send(&env, put_json("admin", &opt, &json!({ "fixed": 1 }))).await;
    assert!(s.is_client_error(), "{s} {b}");
    // 設定（範囲外は 400）。
    let (s, b) = send(&env, get("admin", "/org-admin/data/settings")).await;
    assert_eq!(s, StatusCode::OK, "{b}");
    let (s, _) = send(
        &env,
        put_json(
            "admin",
            "/org-admin/data/settings",
            &json!({ "count_cap": 0 }),
        ),
    )
    .await;
    assert_eq!(s, StatusCode::BAD_REQUEST);
    let (s, b) = send(
        &env,
        put_json(
            "admin",
            "/org-admin/data/settings",
            &json!({ "count_cap": 500, "max_indexed_fields": 30 }),
        ),
    )
    .await;
    assert_eq!(s, StatusCode::OK, "{b}");
    assert_eq!(
        (b["count_cap"].clone(), b["max_indexed_fields"].clone()),
        (json!(500), json!(30))
    );
    // R3-4: 後段の値が範囲外なら、前段の値も変えずに 400（監査も増えない）。
    let audits = |pool: sqlx::PgPool| async move {
        sqlx::query_scalar::<_, i64>(
            "SELECT count(*) FROM audit_log WHERE tenant_id = $1 \
               AND action = 'data.tenant_settings.update'",
        )
        .bind(TENANT)
        .fetch_one(&pool)
        .await
        .unwrap()
    };
    let audits_before = audits(env.pool.clone()).await;
    for bad in [
        json!({ "count_cap": 700, "max_indexed_fields": 0 }),
        json!({ "count_cap": 700, "max_indexed_fields": 40, "max_view_lookup_columns": 1000 }),
    ] {
        let (s, b) = send(&env, put_json("admin", "/org-admin/data/settings", &bad)).await;
        assert_eq!(s, StatusCode::BAD_REQUEST, "{bad} {b}");
    }
    let (s, b) = send(&env, get("admin", "/org-admin/data/settings")).await;
    assert_eq!(s, StatusCode::OK, "{b}");
    assert_eq!(
        (
            b["count_cap"].clone(),
            b["max_indexed_fields"].clone(),
            b["max_view_lookup_columns"].clone()
        ),
        (json!(500), json!(30), json!(null)),
        "範囲外を含む更新は何も変えない"
    );
    assert_eq!(audits(env.pool.clone()).await, audits_before);
    // 3 つを一度に変えると監査は 1 件（変えた項目が並ぶ）。
    let (s, b) = send(
        &env,
        put_json(
            "admin",
            "/org-admin/data/settings",
            &json!({ "count_cap": 600, "max_indexed_fields": 25, "max_view_lookup_columns": 8 }),
        ),
    )
    .await;
    assert_eq!(s, StatusCode::OK, "{b}");
    assert_eq!(b["max_view_lookup_columns"], json!(8));
    assert_eq!(audits(env.pool.clone()).await, audits_before + 1);
    let meta: Value = sqlx::query_scalar(
        "SELECT metadata FROM audit_log WHERE tenant_id = $1 \
           AND action = 'data.tenant_settings.update' ORDER BY id DESC LIMIT 1",
    )
    .bind(TENANT)
    .fetch_one(&env.pool)
    .await
    .unwrap();
    assert_eq!(
        meta,
        json!({ "count_cap": 600, "max_indexed_fields": 25, "max_view_lookup_columns": 8 })
    );
}

/// 索引（監査 B2-8）: viewer が状態を引ける・昇格できる。権限なし・未知の列・存在しないテーブルの形。
#[tokio::test]
async fn data_index_routes_authorize_and_validate() {
    let Some(env) = setup().await else { return };
    let t = table(&env, "indexes").await;
    let path = format!("/data/tables/{}/indexes", t.id);
    let (s, b) = send(&env, get("bob", &path)).await;
    assert_eq!(s, StatusCode::OK, "{b}");
    let text = b.to_string();
    assert!(
        text.contains("\"title\"") && !text.contains("record_int"),
        "{text}"
    );
    let (s, _) = send(&env, get("carol", &path)).await;
    assert_eq!(
        s,
        StatusCode::NOT_FOUND,
        "見えないテーブルは存在しないのと同じ"
    );
    let (s, b) = send(&env, post_json("bob", &path, &json!({ "field": "memo" }))).await;
    assert_eq!(s, StatusCode::OK, "{b}");
    assert_eq!(b["status"], "active", "空のテーブルは即時に有効化: {b}");
    let (s, b) = send(&env, post_json("bob", &path, &json!({ "field": "nope" }))).await;
    assert_eq!(s, StatusCode::FORBIDDEN, "{b}");
    assert_eq!(b["code"], "field_unavailable");
    let (s, _) = send(&env, post_json("carol", &path, &json!({ "field": "memo" }))).await;
    assert_eq!(s, StatusCode::NOT_FOUND);
    let (s, _) = send(
        &env,
        get("alice", &format!("/data/tables/{}/indexes", Uuid::new_v4())),
    )
    .await;
    assert!(
        matches!(s, StatusCode::NOT_FOUND | StatusCode::FORBIDDEN),
        "存在しないテーブル: {s}"
    );
}

/// 配信（監査 B2-8）: viewer だけが購読・awareness を送れる。`Last-Event-ID` の不正は 400。
#[tokio::test]
async fn data_stream_routes_authorize_and_validate() {
    let Some(env) = setup().await else { return };
    let t = table(&env, "stream").await;
    let rec = env
        .data
        .create_record(&ctx("alice"), t.id, json!({ "title": "x" }), None)
        .await
        .unwrap();
    let stream = format!("/data/tables/{}/stream", t.id);
    // 購読（本文は読み切らずにヘッダだけ確かめる）。
    let resp = env.app.clone().oneshot(get("bob", &stream)).await.unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    assert!(resp
        .headers()
        .get("content-type")
        .and_then(|v| v.to_str().ok())
        .is_some_and(|v| v.starts_with("text/event-stream")));
    drop(resp);
    let (s, _) = send(&env, get("carol", &stream)).await;
    assert_eq!(
        s,
        StatusCode::NOT_FOUND,
        "見えないテーブルは存在しないのと同じ"
    );
    let mut bad = get("bob", &stream);
    bad.headers_mut()
        .insert("last-event-id", "x".parse().unwrap());
    let (s, b) = send(&env, bad).await;
    assert_eq!(s, StatusCode::BAD_REQUEST);
    assert_eq!(b["code"], "cursor_invalid", "{b}");
    // awareness: viewer は 204・権限なしは 403・未知の列は 400・存在しないレコードは 404。
    let aw = format!("/data/tables/{}/awareness", t.id);
    let (s, b) = send(
        &env,
        post_json(
            "bob",
            &aw,
            &json!({ "record_id": rec.id, "field": "title" }),
        ),
    )
    .await;
    assert_eq!(s, StatusCode::NO_CONTENT, "{b}");
    let (s, _) = send(
        &env,
        post_json(
            "carol",
            &aw,
            &json!({ "record_id": rec.id, "field": "title" }),
        ),
    )
    .await;
    assert_eq!(s, StatusCode::NOT_FOUND);
    let (s, b) = send(
        &env,
        post_json("bob", &aw, &json!({ "record_id": rec.id, "field": "nope" })),
    )
    .await;
    assert_eq!(s, StatusCode::FORBIDDEN, "{b}");
    assert_eq!(b["code"], "field_unavailable");
    let (s, _) = send(
        &env,
        post_json(
            "bob",
            &aw,
            &json!({ "record_id": Uuid::new_v4(), "field": "title" }),
        ),
    )
    .await;
    assert_eq!(s, StatusCode::NOT_FOUND);
}

/// 409 `unique_violation` は読める列を名指しする（`field` に参照名・`message` に表示名・値は含めない）。
/// ユーザーの列の絞り込みは ID の形式だけを見る（ディレクトリに居ない退職者の ID は 200・表示名は 400）。
#[tokio::test]
async fn unique_conflicts_name_the_column_and_user_filters_check_the_id_format() {
    let Some(env) = setup().await else { return };
    let mut code = FieldDef::new("code", FieldType::Text);
    code.unique = true;
    code.display_name = "社員番号".into();
    let mut who = FieldDef::new("who", FieldType::UserRef);
    who.indexed = true;
    let alice = ctx("alice");
    let t = env
        .data
        .create_table(
            &alice,
            NewDataTable {
                name: format!("unique-{}", Uuid::new_v4().simple()),
                schema: TableSchema {
                    fields: vec![FieldDef::new("title", FieldType::Text), code, who],
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
        .unwrap();
    let records = format!("/data/tables/{}/records", t.id);
    let body = json!({ "data": { "title": "a", "code": "E-100" } });
    let (s, b) = send(&env, post_json("alice", &records, &body)).await;
    assert_eq!(s, StatusCode::CREATED, "{b}");
    let (s, b) = send(&env, post_json("alice", &records, &body)).await;
    assert_eq!(s, StatusCode::CONFLICT, "{b}");
    assert_eq!(b["code"], "unique_violation", "{b}");
    assert_eq!(b["field"], "code", "{b}");
    let message = b["message"].as_str().unwrap();
    assert!(message.contains("社員番号"), "{b}");
    assert!(!b.to_string().contains("E-100"), "値を含めない: {b}");
    let mut keys: Vec<&str> = b.as_object().unwrap().keys().map(String::as_str).collect();
    keys.sort_unstable();
    assert_eq!(keys, ["code", "field", "message", "status", "title"], "{b}");

    let query = format!("/data/tables/{}/query", t.id);
    let filter =
        |v: &str| json!({ "filter": { "cmp": { "field": "who", "op": "eq", "value": v } } });
    // ディレクトリに居ない（退職した）人の ID でも形式が正しければ 200。
    let (s, b) = send(
        &env,
        post_json("alice", &query, &filter("departed-user-42")),
    )
    .await;
    assert_eq!(s, StatusCode::OK, "{b}");
    assert_eq!(b["items"].as_array().map(Vec::len), Some(0), "{b}");
    // 表示名は 400 `value_type_mismatch`（列は書き手が名指ししたもの）。
    let (s, b) = send(&env, post_json("alice", &query, &filter("山田 太郎"))).await;
    assert_eq!(s, StatusCode::BAD_REQUEST, "{b}");
    assert_eq!(b["code"], "value_type_mismatch", "{b}");
    assert_eq!(b["field"], "who", "{b}");
    // 形の誤りも他の本文と同じ「本文（JSON）を読めません: …」（汎用の文言にしない・3 回目の検証 3-B-6）。
    let (s, b) = send(&env, post_json("alice", &query, &json!({ "limit": "abc" }))).await;
    assert_eq!(s, StatusCode::BAD_REQUEST, "{b}");
    assert_eq!(b["code"], "invalid_input", "{b}");
    let m = b["message"].as_str().unwrap();
    assert!(m.starts_with("本文（JSON）を読めません"), "{b}");
    assert!(
        !m.contains("expected") && !m.contains("invalid type"),
        "{b}"
    );
}

/// 上限を超えた取込の本文は**残りを読み捨ててから** 413 を返す（検証 A P3）。送信の途中で応答して接続を
/// 閉じると、前段のプロキシ（web の BFF `/api`）が本文の送信で EPIPE になり 413 ではなく 500 を返した。
/// 実際の TCP で上限 + 8 MiB を送り切り、413 の応答を読めることを確かめる。
#[tokio::test]
async fn oversized_import_is_drained_before_413_over_a_real_socket() {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    let Some(env) = setup().await else { return };
    let t = table(&env, "imports-drain").await;
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let app = env.app.clone();
    let server = tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    });
    let total = api::routes::data_imports::MAX_IMPORT_BODY_BYTES + 8 * 1024 * 1024;
    let mut sock = tokio::net::TcpStream::connect(addr).await.unwrap();
    let head = format!(
        "POST /data/tables/{}/imports/csv HTTP/1.1\r\nhost: {addr}\r\n\
         cookie: shiki_session=sid-alice.{TENANT}; shiki_csrf=csrf-alice\r\n\
         x-csrf-token: csrf-alice\r\ncontent-type: text/csv\r\ncontent-length: {total}\r\n\r\n",
        t.id
    );
    sock.write_all(head.as_bytes()).await.unwrap();
    let chunk = vec![b'x'; 256 * 1024];
    let mut sent = 0usize;
    while sent < total {
        let n = chunk.len().min(total - sent);
        sock.write_all(&chunk[..n])
            .await
            .unwrap_or_else(|e| panic!("本文を送り切れない（{sent} / {total} バイトで {e}）"));
        sent += n;
    }
    sock.flush().await.unwrap();
    let mut resp = Vec::new();
    let mut buf = [0u8; 4096];
    loop {
        let n = tokio::time::timeout(Duration::from_secs(30), sock.read(&mut buf))
            .await
            .expect("応答が来ない")
            .unwrap();
        if n == 0 {
            break;
        }
        resp.extend_from_slice(&buf[..n]);
        let text = String::from_utf8_lossy(&resp);
        if text.contains("\r\n\r\n") && text.contains("import_too_large") {
            break;
        }
    }
    let text = String::from_utf8_lossy(&resp);
    assert!(text.starts_with("HTTP/1.1 413"), "{text}");
    assert!(text.contains("\"import_too_large\""), "{text}");
    server.abort();
}

/// `POST /directory/users/names`（グリッドの `user_ref` 列の表示名）: 同テナント・同 org の
/// ユーザーだけを返し、別テナント・別 org・未登録・表示名が空の id は含めない。件数上限と認証も見る。
#[tokio::test]
async fn resolve_user_names_is_tenant_and_org_scoped() {
    let Some(env) = setup().await else { return };
    let dir = storage::DirectoryStore::new(env.pool.clone());
    let tag = Uuid::new_v4().simple().to_string();
    let id = |s: &str| format!("names-{tag}-{s}");
    dir.upsert_user(&id("ann"), TENANT, "acme", "ann@acme.example", "Ann")
        .await
        .unwrap();
    dir.upsert_user(&id("ben"), TENANT, "acme", "ben@acme.example", "Ben")
        .await
        .unwrap();
    // 同じ id でも別テナントの行は使わない／同テナントでも別 org の人は出さない。
    dir.upsert_user(
        &id("ann"),
        "other-tenant",
        "acme",
        "x@other.example",
        "Mallory",
    )
    .await
    .unwrap();
    dir.upsert_user(&id("eve"), TENANT, "globex", "eve@globex.example", "Eve")
        .await
        .unwrap();
    dir.upsert_user(&id("nobody"), TENANT, "acme", "n@acme.example", "")
        .await
        .unwrap();

    let ids =
        json!({ "ids": [id("ann"), id("ben"), id("ann"), id("eve"), id("nobody"), id("ghost")] });
    let (st, body) = send(&env, post_json("bob", "/directory/users/names", &ids)).await;
    assert_eq!(st, StatusCode::OK, "{body}");
    assert_eq!(
        body,
        json!({ "names": { id("ann"): "Ann", id("ben"): "Ben" } }),
        "同テナント・同 org・表示名ありの id だけを返す"
    );

    let (st, body) = send(
        &env,
        post_json("bob", "/directory/users/names", &json!({ "ids": [] })),
    )
    .await;
    assert_eq!(st, StatusCode::OK);
    assert_eq!(body, json!({ "names": {} }));

    let too_many: Vec<String> = (0..=api::routes::directory::MAX_RESOLVE_USER_NAMES)
        .map(|i| format!("u{i}"))
        .collect();
    let (st, _) = send(
        &env,
        post_json("bob", "/directory/users/names", &json!({ "ids": too_many })),
    )
    .await;
    assert_eq!(st, StatusCode::BAD_REQUEST, "上限を超える id は 400");

    let anon = Request::builder()
        .method("POST")
        .uri("/directory/users/names")
        .header("content-type", "application/json")
        .body(Body::from(ids.to_string()))
        .unwrap();
    let (st, body) = send(&env, anon).await;
    assert_eq!(st, StatusCode::UNAUTHORIZED, "未認証は 401");
    assert!(!body.to_string().contains("Ann"));
}

/// テーブルの共有（`/data/tables/{id}/shares`・#864）: owner だけが付与・解除・一覧でき、一覧は相手の表示名・
/// メールを添える。付与した相手は一覧・取得で見え、解除で即時に 404 に戻る。owner 以外は 403、見えない人は 404、
/// 同じテナントに居ない相手・役割の語彙外は 400。
#[tokio::test]
async fn table_share_routes_grant_list_and_revoke() {
    let Some(env) = setup().await else { return };
    let dir = storage::DirectoryStore::new(env.pool.clone());
    dir.upsert_user("carol", TENANT, "acme", "carol@acme.example", "Carol")
        .await
        .unwrap();
    let role = format!("sales-{}", Uuid::new_v4().simple());
    dir.upsert_role(&role, TENANT, "acme", "営業部")
        .await
        .unwrap();
    // 別テナントにだけ居るユーザー（共有先にできない）。
    dir.upsert_user(
        "mallory-864",
        "other-tenant",
        "acme",
        "m@x.example",
        "Mallory",
    )
    .await
    .unwrap();
    let t = table(&env, "share").await; // alice: owner／bob: viewer
    let base = format!("/data/tables/{}", t.id);
    let shares = format!("{base}/shares");
    let carol_viewer = json!({ "target": { "type": "user", "id": "carol" }, "role": "viewer" });

    // 共有前の carol には見えない（共有の操作も 404）。
    assert_eq!(
        send(&env, get("carol", &base)).await.0,
        StatusCode::NOT_FOUND
    );
    assert_eq!(
        send(&env, put_json("carol", &shares, &carol_viewer))
            .await
            .0,
        StatusCode::NOT_FOUND
    );
    assert_eq!(
        send(&env, get("carol", &shares)).await.0,
        StatusCode::NOT_FOUND
    );
    // bob（viewer・owner でない）は 403。
    assert_eq!(
        send(&env, put_json("bob", &shares, &carol_viewer)).await.0,
        StatusCode::FORBIDDEN
    );
    assert_eq!(
        send(&env, get("bob", &shares)).await.0,
        StatusCode::FORBIDDEN
    );

    // alice が carol（viewer）と部署（editor）へ共有する。
    let (st, body) = send(&env, put_json("alice", &shares, &carol_viewer)).await;
    assert_eq!(st, StatusCode::NO_CONTENT, "{body}");
    let role_editor = json!({ "target": { "type": "role", "id": role }, "role": "editor" });
    let (st, body) = send(&env, put_json("alice", &shares, &role_editor)).await;
    assert_eq!(st, StatusCode::NO_CONTENT, "{body}");
    assert_eq!(send(&env, get("carol", &base)).await.0, StatusCode::OK);
    let (st, body) = send(&env, get("carol", "/data/tables")).await;
    assert_eq!(st, StatusCode::OK);
    assert!(
        body["items"]
            .as_array()
            .unwrap()
            .iter()
            .any(|i| i["id"] == json!(t.id)),
        "共有されたテーブルが一覧に出る: {body}"
    );

    let (st, body) = send(&env, get("alice", &shares)).await;
    assert_eq!(st, StatusCode::OK, "{body}");
    let items = body.as_array().unwrap();
    let carol = items
        .iter()
        .find(|e| e["target"] == json!({ "type": "user", "id": "carol" }))
        .unwrap_or_else(|| panic!("carol が一覧に無い: {body}"));
    assert_eq!(carol["role"], json!("viewer"));
    assert_eq!(carol["display_name"], json!("Carol"));
    assert_eq!(carol["email"], json!("carol@acme.example"));
    let dept = items
        .iter()
        .find(|e| e["target"] == json!({ "type": "role", "id": role }))
        .unwrap_or_else(|| panic!("部署が一覧に無い: {body}"));
    assert_eq!(dept["role"], json!("editor"));
    assert_eq!(dept["display_name"], json!("営業部"));
    assert!(
        items.iter().all(|e| e["target"]["id"] != json!("alice")),
        "owner は一覧に出ない: {body}"
    );

    // 共有先の誤り: 存在しない・別テナント・形の不正は 400、役割の語彙外（owner）も 400 系。
    for target in [
        json!({ "type": "user", "id": "ghost-864" }),
        json!({ "type": "user", "id": "mallory-864" }),
        json!({ "type": "role", "id": "no-such-role-864" }),
        json!({ "type": "user", "id": "carol|x" }),
    ] {
        let (st, body) = send(
            &env,
            put_json(
                "alice",
                &shares,
                &json!({ "target": target, "role": "viewer" }),
            ),
        )
        .await;
        assert_eq!(st, StatusCode::BAD_REQUEST, "{target}: {body}");
    }
    let (st, _) = send(
        &env,
        put_json(
            "alice",
            &shares,
            &json!({ "target": { "type": "user", "id": "carol" }, "role": "owner" }),
        ),
    )
    .await;
    assert!(st.is_client_error() && st != StatusCode::NOT_FOUND, "{st}");

    // 解除で即時に 404 に戻る（冪等）。
    for _ in 0..2 {
        let (st, body) = send(
            &env,
            req(
                "DELETE",
                "alice",
                &shares,
                "application/json",
                Body::from(carol_viewer.to_string()),
            ),
        )
        .await;
        assert_eq!(st, StatusCode::NO_CONTENT, "{body}");
    }
    assert_eq!(
        send(&env, get("carol", &base)).await.0,
        StatusCode::NOT_FOUND
    );

    let rows: Vec<(String, String)> = sqlx::query_as(
        "SELECT action, decision FROM audit_log WHERE tenant_id = $1 \
           AND object_type = 'data_table' AND object_id = $2 AND action LIKE 'data.table.%share%' \
         ORDER BY id",
    )
    .bind(TENANT)
    .bind(t.id.to_string())
    .fetch_all(&env.pool)
    .await
    .unwrap();
    for (action, decision) in [
        ("data.table.share", "allow"),
        ("data.table.unshare", "allow"),
        ("data.table.share", "deny"),
        ("data.table.shares.list", "deny"),
    ] {
        assert!(
            rows.iter().any(|(a, d)| a == action && d == decision),
            "{action} {decision} の監査行: {rows:?}"
        );
    }
}
