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
        workflow_approvals: Arc::new(workflow_engine::ApprovalStore::new(pool.clone())),
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

/// Excel が保存する文字コードの CSV を取り込む（GAP-45）。Shift_JIS（日本語の Windows の既定）・BOM 付きの
/// UTF-8・UTF-16LE（BOM 付き）の CSV が、文字化けせずに同じレコードになる。文字コードを決められない CSV は、
/// 取込を始めずに直し方を言う構造化 400（`import_invalid`）。
#[tokio::test]
async fn bulk_import_reads_csv_in_the_encodings_excel_writes() {
    use textenc::encoding_rs::{SHIFT_JIS, UTF_16LE, UTF_8};
    let Some(env) = setup().await else { return };
    let csv = "件名,amount\n交通費,1200\n宿泊費,8000\n";
    let mut tables = Vec::new();
    let mut seen = Vec::new();
    for (encoding, bom) in [(SHIFT_JIS, false), (UTF_8, true), (UTF_16LE, true)] {
        let t = table(&env, "import-encodings").await;
        tables.push(t.id);
        let base = format!("/data/tables/{}/imports", t.id);
        let bytes = textenc::encode_like(csv, encoding, bom).unwrap();
        let (s, b) = send(&env, post_csv("alice", &format!("{base}/csv"), bytes)).await;
        assert_eq!(s, StatusCode::ACCEPTED, "{}: {b}", encoding.name());
        let id: Uuid = serde_json::from_value(b["import_id"].clone()).unwrap();
        finish(&env, t.id, id).await;
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
        let mut titles: Vec<(String, Value)> = rows
            .items
            .iter()
            .map(|r| {
                (
                    r.data["title"].as_str().unwrap_or_default().to_string(),
                    r.data["amount"].clone(),
                )
            })
            .collect();
        titles.sort_by(|a, b| a.0.cmp(&b.0));
        seen.push((encoding.name(), titles));
    }
    // 文字コードを決められない CSV（西欧の windows-1252 の é）は取り込まない。
    let latin = table(&env, "import-encodings-latin").await;
    tables.push(latin.id);
    let (s, refused) = send(
        &env,
        post_csv(
            "alice",
            &format!("/data/tables/{}/imports/csv", latin.id),
            b"title\ncaf\xe9\n".to_vec(),
        ),
    )
    .await;
    // 片付け（作ったテーブル）。
    for id in tables {
        env.data
            .delete_table(&ctx("alice"), id, None)
            .await
            .unwrap();
    }

    for (name, titles) in seen {
        assert_eq!(
            titles,
            vec![
                ("交通費".to_string(), json!(1200)),
                ("宿泊費".to_string(), json!(8000))
            ],
            "{name}"
        );
    }
    assert_eq!(s, StatusCode::BAD_REQUEST, "{refused}");
    assert_eq!(refused["code"], "import_invalid");
    assert_eq!(
        refused["message"],
        "CSV の文字コードを読み取れません。UTF-8 か Shift_JIS で保存してください"
    );
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

/// このテストで作ったテーブルの行を消す（索引・レコード・テーブル行。共有の固定テナントなので、テナントごとは消さない）。
async fn purge_table(env: &Env, table: Uuid) {
    let keys: Option<(i32, i64)> = sqlx::query_as(
        "SELECT tenant_int, table_int FROM data_table WHERE tenant_id = $1 AND id = $2",
    )
    .bind(TENANT)
    .bind(table)
    .fetch_optional(&env.pool)
    .await
    .unwrap();
    if let Some((ti, tb)) = keys {
        for t in [
            "data_index_text",
            "data_index_num",
            "data_index_search",
            "data_index_composite",
            "data_unique",
            "data_link",
            "data_sort_set",
            "data_index_state",
            "data_index_backfill",
            "data_index_purge",
            "data_derived_dep",
            "data_derived_pending",
            "data_record",
        ] {
            sqlx::query(&format!(
                "DELETE FROM {t} WHERE tenant_int = $1 AND table_int = $2"
            ))
            .bind(ti)
            .bind(tb)
            .execute(&env.pool)
            .await
            .unwrap_or_else(|e| panic!("{t}: {e}"));
        }
    }
    sqlx::query("DELETE FROM data_table WHERE tenant_id = $1 AND id = $2")
        .bind(TENANT)
        .bind(table)
        .execute(&env.pool)
        .await
        .unwrap();
    env.fga
        .delete_object_tuples(&ctx("alice").ns().data_table(&table.to_string()))
        .await
        .unwrap();
}

/// テーブルの「追加だけ」の共有（D-113・GAP-49）: 持ち主が carol に追加だけを渡すと、carol の一覧と名前の検索に
/// 印つきで出て、定義を読め、レコードを作れ、自分の行だけを読める（件数・集計・履歴も）。書き換え・削除は 403、
/// 他人の行は 404。閲覧に変えると全行が見え、取り消すと表が 404 に戻る。作成は監査に残る。
#[tokio::test]
async fn table_add_only_share_creates_and_reads_only_own_records() {
    let Some(env) = setup().await else { return };
    let dir = storage::DirectoryStore::new(env.pool.clone());
    dir.upsert_user("carol", TENANT, "acme", "carol@acme.example", "Carol")
        .await
        .unwrap();
    let t = table(&env, "add-only").await; // alice: owner／bob: viewer
    let base = format!("/data/tables/{}", t.id);
    let shares = format!("{base}/shares");
    let (st, a) = send(
        &env,
        post_json(
            "alice",
            &format!("{base}/records"),
            &json!({ "data": { "title": "alice-secret", "amount": 900 } }),
        ),
    )
    .await;
    assert_eq!(st, StatusCode::CREATED, "{a}");
    let a_id = a["id"].as_str().unwrap().to_string();

    // 共有前の carol には表が無いのと同じ（作成も 404）。
    let (st, _) = send(
        &env,
        post_json(
            "carol",
            &format!("{base}/records"),
            &json!({ "data": { "title": "x" } }),
        ),
    )
    .await;
    assert_eq!(st, StatusCode::NOT_FOUND);

    let carol_submitter =
        json!({ "target": { "type": "user", "id": "carol" }, "role": "submitter" });
    let (st, body) = send(&env, put_json("alice", &shares, &carol_submitter)).await;
    assert_eq!(st, StatusCode::NO_CONTENT, "{body}");
    let (st, body) = send(&env, get("alice", &shares)).await;
    assert_eq!(st, StatusCode::OK);
    assert!(
        body.as_array()
            .unwrap()
            .iter()
            .any(|e| e["target"]["id"] == json!("carol")
                && e["role"] == json!("submitter")
                && e["display_name"] == json!("Carol")),
        "{body}"
    );

    // 一覧・名前の検索・定義・画面の文脈。
    let name_q = &t.name[..12];
    for path in [
        "/data/tables".to_string(),
        format!("/data/tables?q={name_q}"),
    ] {
        let (st, body) = send(&env, get("carol", &path)).await;
        assert_eq!(st, StatusCode::OK, "{path}: {body}");
        let item = body["items"]
            .as_array()
            .unwrap()
            .iter()
            .find(|i| i["id"] == json!(t.id))
            .unwrap_or_else(|| panic!("{path} に出ない: {body}"))
            .clone();
        assert_eq!(item["add_only"], json!(true), "{item}");
    }
    let (st, def) = send(&env, get("carol", &base)).await;
    assert_eq!(st, StatusCode::OK);
    assert_eq!(def["add_only"], json!(true));
    let (_, bob_def) = send(&env, get("bob", &base)).await;
    assert!(
        bob_def.get("add_only").is_none(),
        "閲覧者には印が無い: {bob_def}"
    );
    let (st, vc) = send(&env, get("carol", &format!("{base}/viewer-context"))).await;
    assert_eq!(st, StatusCode::OK);
    assert_eq!(
        (
            &vc["can_add"],
            &vc["add_only"],
            &vc["can_edit"],
            &vc["can_manage"]
        ),
        (&json!(true), &json!(true), &json!(false), &json!(false)),
        "{vc}"
    );

    // 作成 → 自分の行だけ。
    let (st, mine) = send(
        &env,
        post_json(
            "carol",
            &format!("{base}/records"),
            &json!({ "data": { "title": "carol-own", "amount": 30 } }),
        ),
    )
    .await;
    assert_eq!(st, StatusCode::CREATED, "{mine}");
    let mine_id = mine["id"].as_str().unwrap().to_string();
    assert_eq!(mine["owner"], json!("carol"));
    let (st, list) = send(&env, get("carol", &format!("{base}/records"))).await;
    assert_eq!(st, StatusCode::OK);
    let titles: Vec<&Value> = list["items"]
        .as_array()
        .unwrap()
        .iter()
        .map(|i| &i["data"]["title"])
        .collect();
    assert_eq!(titles, vec![&json!("carol-own")], "{list}");
    let (_, count) = send(&env, get("carol", &format!("{base}/records/count"))).await;
    assert_eq!(count["count"], json!(1), "{count}");
    let (st, agg) = send(
        &env,
        post_json(
            "carol",
            &format!("{base}/query"),
            &json!({ "aggregate": [{ "metric": "count" }] }),
        ),
    )
    .await;
    assert_eq!(st, StatusCode::OK, "{agg}");
    // 集計も自分の行だけが材料で、件数の少ない集計は伏せる規則（既定の K）がそのまま効く（他人の行で K を満たさない）。
    assert_eq!(agg["suppressed"], json!(true), "{agg}");
    assert_eq!(agg["groups"][0]["values"][0], Value::Null, "{agg}");
    for (path, want) in [
        (format!("{base}/records/{mine_id}"), StatusCode::OK),
        (format!("{base}/records/{a_id}"), StatusCode::NOT_FOUND),
        (
            format!("{base}/records/{}", Uuid::new_v4()),
            StatusCode::NOT_FOUND,
        ),
        (
            format!("{base}/records/{mine_id}/revisions"),
            StatusCode::OK,
        ),
        (
            format!("{base}/records/{a_id}/revisions"),
            StatusCode::NOT_FOUND,
        ),
    ] {
        assert_eq!(send(&env, get("carol", &path)).await.0, want, "{path}");
    }
    // 書き換え・削除は自分の行でも 403（表の権限で断る）。
    for id in [&mine_id, &a_id] {
        let (st, _) = send(
            &env,
            req(
                "PATCH",
                "carol",
                &format!("{base}/records/{id}"),
                "application/json",
                Body::from(json!({ "patch": { "title": "x" }, "expected_rev": 1 }).to_string()),
            ),
        )
        .await;
        assert_eq!(st, StatusCode::FORBIDDEN, "patch {id}");
        let (st, _) = send(
            &env,
            req(
                "DELETE",
                "carol",
                &format!("{base}/records/{id}?expected_rev=1"),
                "application/json",
                Body::empty(),
            ),
        )
        .await;
        assert_eq!(st, StatusCode::FORBIDDEN, "delete {id}");
    }
    // 作成は監査に残る（追加だけの人でも）。
    let (n,): (i64,) = sqlx::query_as(
        "SELECT count(*) FROM audit_log WHERE tenant_id = $1 AND actor = 'carol' \
           AND action = 'data.record.create' AND decision = 'allow' AND object_id = $2",
    )
    .bind(TENANT)
    .bind(&mine_id)
    .fetch_one(&env.pool)
    .await
    .unwrap();
    assert_eq!(n, 1, "作成の監査");

    // 閲覧に変えると全行が見える（追加の操作は消える）。取り消すと表が無いのと同じに戻る。
    let carol_viewer = json!({ "target": { "type": "user", "id": "carol" }, "role": "viewer" });
    assert_eq!(
        send(&env, put_json("alice", &shares, &carol_viewer))
            .await
            .0,
        StatusCode::NO_CONTENT
    );
    let del = |body: &Value| {
        req(
            "DELETE",
            "alice",
            &shares,
            "application/json",
            Body::from(body.to_string()),
        )
    };
    assert_eq!(
        send(&env, del(&carol_submitter)).await.0,
        StatusCode::NO_CONTENT
    );
    let (_, list) = send(&env, get("carol", &format!("{base}/records"))).await;
    assert_eq!(list["items"].as_array().unwrap().len(), 2, "{list}");
    let (_, vc) = send(&env, get("carol", &format!("{base}/viewer-context"))).await;
    assert_eq!(
        (&vc["can_add"], &vc["add_only"]),
        (&json!(false), &json!(false)),
        "{vc}"
    );
    assert_eq!(
        send(&env, del(&carol_viewer)).await.0,
        StatusCode::NO_CONTENT
    );
    assert_eq!(
        send(&env, get("carol", &base)).await.0,
        StatusCode::NOT_FOUND
    );
    assert_eq!(
        send(&env, get("carol", &format!("{base}/records/{mine_id}")))
            .await
            .0,
        StatusCode::NOT_FOUND
    );
    purge_table(&env, t.id).await;
}

/// 申請のテーブル（状態の列・申請者と承認者のメンバーの列）。alice が持ち主、bob が編集者（承認する人）。
async fn approval_table(env: &Env, name: &str) -> data::DataTable {
    let mut title = FieldDef::new("title", FieldType::Text);
    title.display_name = "件名".into();
    let mut status = FieldDef::new("status", FieldType::Select);
    status.display_name = "状態".into();
    status.options = vec!["申請中".into(), "承認".into(), "却下".into()];
    let mut applicant = FieldDef::new("applicant", FieldType::UserRef);
    applicant.display_name = "申請者".into();
    let mut approver = FieldDef::new("approver", FieldType::UserRef);
    approver.display_name = "承認者".into();
    approver.indexed = true;
    let schema = TableSchema {
        fields: vec![title, status, applicant, approver],
        status_field: None,
        row_policy: None,
        field_policy: vec![],
        aggregate_min_rows: None,
        fsm_ref: None,
        rag: None,
    };
    let t = env
        .data
        .create_table(
            &ctx("alice"),
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
            Relation::Editor,
            &bob.ns().data_table(&t.id.to_string()),
        )
        .await
        .unwrap();
    t
}

/// 承認・却下は「承認者の列の人で、申請した本人は除く」（D-115 の既定の形）。
fn approval_fsm(states: &[&str], moves: &[(&str, &str)]) -> Value {
    let who = json!({ "all": [
        { "field_cmp": { "field": "approver", "op": "eq", "value": "user_id" } },
        { "field_cmp": { "field": "applicant", "op": "ne", "value": "user_id" } }
    ] });
    json!({
        "states": states,
        "transitions": moves.iter().map(|(f, t)| json!({ "from": f, "to": t, "actor": who })).collect::<Vec<_>>(),
    })
}

/// このテストで作った状態の進め方の定義（artifact）を消す（版は外部キーで一緒に消える）。
async fn purge_fsm_artifacts(env: &Env, table: Uuid) {
    let ids: Vec<Uuid> = sqlx::query_scalar(
        "SELECT id FROM artifact WHERE tenant_id = $1 AND kind = 'fsm' AND name LIKE $2",
    )
    .bind(TENANT)
    .bind(format!("状態の進め方-{table}-%"))
    .fetch_all(&env.pool)
    .await
    .unwrap();
    purge_artifacts(env, &ids).await;
}

async fn purge_artifacts(env: &Env, ids: &[Uuid]) {
    for id in ids {
        env.fga
            .delete_object_tuples(&ctx("alice").ns().artifact(&id.to_string()))
            .await
            .unwrap();
    }
    sqlx::query("DELETE FROM artifact WHERE tenant_id = $1 AND id = ANY($2)")
        .bind(TENANT)
        .bind(ids)
        .execute(&env.pool)
        .await
        .unwrap();
}

/// 指す状態の進め方を同じ定義の別の版へ替えるときも、指す人がその定義を読めることを確かめる。定義の共有を外された
/// 持ち主が、新しい版を指してテーブル側の参照として中身（進められる人の条件）を読む経路を作らない。
#[tokio::test]
async fn repinning_another_version_needs_to_see_the_definition() {
    let Some(env) = setup().await else { return };
    let t = approval_table(&env, "repin").await;
    // bob が自分のテーブルに作った定義（v1）を alice に見せ、alice が自分のテーブルで指す。
    let mut st = FieldDef::new("phase", FieldType::Select);
    st.options = vec!["申請中".into(), "承認".into(), "却下".into()];
    let bobs = env
        .data
        .create_table(
            &ctx("bob"),
            NewDataTable {
                name: format!("bobs-repin-{}", Uuid::new_v4().simple()),
                schema: TableSchema {
                    fields: vec![st],
                    status_field: Some("phase".into()),
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
    let mut fsm = approval_fsm(&["申請中", "承認", "却下"], &[("申請中", "承認")]);
    fsm["transitions"][0]["actor"] = json!("public");
    let (s, made) = send(
        &env,
        post_json("bob", "/data/fsms", &json!({ "name": format!("bob-repin-{}", Uuid::new_v4().simple()), "table_id": bobs.id, "body": fsm })),
    )
    .await;
    assert_eq!(s, StatusCode::CREATED, "{made}");
    let artifact_id: Uuid = serde_json::from_value(made["id"].clone()).unwrap();
    let viewer = (
        ctx("alice").subject(),
        ctx("alice").ns().artifact(&artifact_id.to_string()),
    );
    env.fga
        .write_tuple(&viewer.0, Relation::Viewer, &viewer.1)
        .await
        .unwrap();
    let pin = |table: &Value, version: i64| {
        let mut next = table["schema"].clone();
        next["status_field"] = json!("status");
        next["fsm_ref"] = json!({ "artifact_id": artifact_id, "version": version });
        json!({ "schema": next, "expected_schema_version": table["schema_version"] })
    };
    let schema_url = format!("/data/tables/{}/schema", t.id);
    let (_, table) = send(&env, get("alice", &format!("/data/tables/{}", t.id))).await;
    let (shared, pinned) = send(&env, put_json("alice", &schema_url, &pin(&table, 1))).await;
    // 共有を外した後で bob が v2 を積む。alice が v2 を指そうとすると断る（中身を読ませない）。
    env.fga
        .delete_tuple(&viewer.0, Relation::Viewer, &viewer.1)
        .await
        .unwrap();
    let artifacts = artifact::ArtifactStore::new(env.pool.clone(), Arc::clone(&env.fga));
    let v2 = artifacts
        .append_version(&ctx("bob"), artifact_id, fsm.clone(), None, None)
        .await
        .unwrap();
    let (unshared, e) = send(
        &env,
        put_json("alice", &schema_url, &pin(&pinned, v2.version)),
    )
    .await;
    purge_artifacts(&env, &[artifact_id]).await;
    purge_table(&env, t.id).await;
    purge_table(&env, bobs.id).await;

    assert_eq!(shared, StatusCode::OK, "{pinned}");
    assert_eq!(unshared, StatusCode::BAD_REQUEST, "{e}");
    assert_eq!(e["code"], "fsm_invalid", "{e}");
}

/// 書き込みの決まり（最初の状態を過ぎたら申請者は書き換えられない）を付けると、途中から申請者の列を足したテーブルの
/// 既存のレコード（状態が最初の状態でなく、申請者の列が空）は誰も書き換えられなくなる。保存は数を言って 409
/// `fsm_locks_records` で止め、確かめたら保存する。決まりを外す保存・同じ形のまま保存し直す保存は止めない。
#[tokio::test]
async fn table_fsm_write_lock_asks_before_locking_existing_records() {
    let Some(env) = setup().await else { return };
    let t = approval_table(&env, "locks").await;
    let fsm_url = format!("/data/tables/{}/fsm", t.id);
    let rec_url = format!("/data/tables/{}/records", t.id);
    // 状態の進め方を作る前のレコード: 承認済みで申請者が空（2 件）・申請中で申請者が空・承認済みで申請者あり。
    let dir = storage::DirectoryStore::new(env.pool.clone());
    dir.upsert_user("alice", TENANT, "acme", "alice@acme.example", "alice")
        .await
        .unwrap();
    for data in [
        json!({ "title": "前からある 1", "status": "承認" }),
        json!({ "title": "前からある 2", "status": "却下" }),
        json!({ "title": "まだ申請中", "status": "申請中" }),
        json!({ "title": "申請者あり", "status": "承認", "applicant": "alice" }),
    ] {
        let (s, v) = send(&env, post_json("alice", &rec_url, &json!({ "data": data }))).await;
        assert_eq!(s, StatusCode::CREATED, "{v}");
    }
    let (_, v) = send(&env, get("alice", &fsm_url)).await;
    let body = approval_fsm(
        &["申請中", "承認", "却下"],
        &[("申請中", "承認"), ("申請中", "却下")],
    );
    let save = |sv: &Value, lock: Value, extra: Value| {
        let mut req = json!({
            "status_field": "status",
            "body": body,
            "applicant_write_lock": lock,
            "expected_schema_version": sv,
        });
        if let (Some(r), Some(e)) = (req.as_object_mut(), extra.as_object()) {
            r.extend(e.clone());
        }
        req
    };
    let (s, locked) = send(
        &env,
        put_json(
            "alice",
            &fsm_url,
            &save(&v["schema_version"], json!("applicant"), json!({})),
        ),
    )
    .await;
    // 確かめると保存する（決まりは付く）。
    let (s2, saved) = send(
        &env,
        put_json(
            "alice",
            &fsm_url,
            &save(
                &v["schema_version"],
                json!("applicant"),
                json!({ "confirm_locked": true }),
            ),
        ),
    )
    .await;
    // 同じ形のまま保存し直すのは止めない（効きが変わらない）。
    let (s3, again) = send(
        &env,
        put_json(
            "alice",
            &fsm_url,
            &save(&saved["schema_version"], json!("applicant"), json!({})),
        ),
    )
    .await;
    // 決まりを外す保存も止めない。
    let (s4, off) = send(
        &env,
        put_json(
            "alice",
            &fsm_url,
            &save(&again["schema_version"], Value::Null, json!({})),
        ),
    )
    .await;
    purge_fsm_artifacts(&env, t.id).await;
    purge_table(&env, t.id).await;

    assert_eq!(s, StatusCode::CONFLICT, "{locked}");
    assert_eq!(locked["code"], "fsm_locks_records", "{locked}");
    assert_eq!(
        (locked["count"].clone(), locked["more"].clone()),
        (json!(2), json!(false))
    );
    assert_eq!(
        locked["message"],
        "申請者の列が空で、最初の状態を過ぎたレコードが 2 件あります。保存すると、これらのレコードは誰も書き換えたり削除したりできなくなります。"
    );
    assert_eq!(s2, StatusCode::OK, "{saved}");
    assert!(
        saved["schema"]["row_policy"]["write"].is_object(),
        "{saved}"
    );
    assert_eq!(s3, StatusCode::OK, "{again}");
    assert_eq!(s4, StatusCode::OK, "{off}");
    assert!(off["schema"]["row_policy"].is_null(), "{off}");
}

/// テーブルの設定の「状態の進め方」（GAP-44・D-84・D-115）: 持ち主だけが読み書きでき、保存は状態の列の選択肢・
/// 使う版・書き込みの決まりを 1 回で替える。既定の形では、申請した本人は自分の申請を承認できず、最初の状態を
/// 過ぎると書き換えられない。承認を待っているレコードが判断できなくなる変更は確認が要り、列の編集だけで状態を
/// 変える改訂と、読めない定義を指す改訂は断る。
#[tokio::test]
async fn table_fsm_routes_save_read_and_keep_applicants_from_approving() {
    let Some(env) = setup().await else { return };
    // メンバーの列の値はディレクトリに居る人だけ。
    let dir = storage::DirectoryStore::new(env.pool.clone());
    for u in ["alice", "bob"] {
        dir.upsert_user(u, TENANT, "acme", &format!("{u}@acme.example"), u)
            .await
            .unwrap();
    }
    let t = approval_table(&env, "states").await;
    let fsm_url = format!("/data/tables/{}/fsm", t.id);
    let rec_url = format!("/data/tables/{}/records", t.id);
    let mut cleanup_artifacts: Vec<Uuid> = Vec::new();

    // 持ち主でない（編集者の bob）は読めも保存もできない（403）。見えない carol には 404。
    assert_eq!(
        send(&env, get("bob", &fsm_url)).await.0,
        StatusCode::FORBIDDEN
    );
    assert_eq!(
        send(&env, get("carol", &fsm_url)).await.0,
        StatusCode::NOT_FOUND
    );
    let (s, v) = send(&env, get("alice", &fsm_url)).await;
    assert_eq!(s, StatusCode::OK, "{v}");
    assert!(v["fsm"].is_null() && v["status_field"].is_null(), "{v}");
    let version = v["schema_version"].as_i64().unwrap();
    let body = approval_fsm(
        &["申請中", "承認", "却下"],
        &[("申請中", "承認"), ("申請中", "却下")],
    );
    let save = |b: &Value, sv: i64, extra: Value| {
        let mut req = json!({
            "status_field": "status",
            "body": b,
            "applicant_write_lock": "applicant",
            "expected_schema_version": sv,
        });
        if let (Some(r), Some(e)) = (req.as_object_mut(), extra.as_object()) {
            r.extend(e.clone());
        }
        req
    };
    assert_eq!(
        send(
            &env,
            put_json("bob", &fsm_url, &save(&body, version, json!({})))
        )
        .await
        .0,
        StatusCode::FORBIDDEN
    );

    // 保存の前の誤り: 同じ組の進め方を 2 つ・同じ状態へ進める・状態の列に選択肢の列でない列。
    for (bad, field) in [
        (
            approval_fsm(
                &["申請中", "承認", "却下"],
                &[("申請中", "承認"), ("申請中", "承認")],
            ),
            "status",
        ),
        (
            approval_fsm(&["申請中", "承認", "却下"], &[("申請中", "申請中")]),
            "status",
        ),
        (body.clone(), "title"),
    ] {
        let mut req = save(&bad, version, json!({}));
        req["status_field"] = json!(field);
        let (s, e) = send(&env, put_json("alice", &fsm_url, &req)).await;
        assert_eq!(s, StatusCode::BAD_REQUEST, "{e}");
        assert_eq!(e["code"], "fsm_invalid", "{e}");
    }

    // 作る: 状態の列の選択肢・指す版・書き込みの決まり（申請者は最初の状態のあいだだけ）が入る。
    let (s, saved) = send(
        &env,
        put_json("alice", &fsm_url, &save(&body, version, json!({}))),
    )
    .await;
    assert_eq!(s, StatusCode::OK, "{saved}");
    let schema = &saved["schema"];
    assert_eq!(schema["status_field"], "status");
    let pinned = schema["fsm_ref"]["artifact_id"]
        .as_str()
        .unwrap()
        .to_string();
    assert_eq!(schema["fsm_ref"]["version"], 1);
    let field = |k: &str| {
        schema["fields"]
            .as_array()
            .unwrap()
            .iter()
            .find(|f| f["key"] == k)
            .cloned()
            .unwrap()
    };
    assert_eq!(
        field("status")["options"],
        json!(["申請中", "承認", "却下"])
    );
    // 行の決まりが使う列は索引つきになる。
    assert_eq!(
        (
            field("status")["indexed"].clone(),
            field("applicant")["indexed"].clone()
        ),
        (json!(true), json!(true))
    );
    assert_eq!(schema["row_policy"]["read"], "public", "{schema}");
    let (s, v) = send(&env, get("alice", &fsm_url)).await;
    assert_eq!(s, StatusCode::OK, "{v}");
    assert_eq!(v["fsm"]["body"], body);
    assert_eq!(v["applicant_write_lock"], "applicant");
    let version = v["schema_version"].as_i64().unwrap();

    // 既定のままでは、申請した本人は承認者でも自分の申請を承認できない（403）。ほかの人の申請は承認できる。
    let (s, own) = send(
        &env,
        post_json("alice", &rec_url, &json!({ "data": { "title": "自分の出張", "applicant": "alice", "approver": "alice" } })),
    )
    .await;
    assert_eq!(s, StatusCode::CREATED, "{own}");
    assert_eq!(own["data"]["status"], "申請中");
    let own_id = own["id"].as_str().unwrap().to_string();
    let (s, e) = send(
        &env,
        post_json(
            "alice",
            &format!("{rec_url}/{own_id}/transition"),
            &json!({ "to": "承認", "expected_rev": own["rev"] }),
        ),
    )
    .await;
    assert_eq!(s, StatusCode::FORBIDDEN, "{e}");
    let (s, other) = send(
        &env,
        post_json(
            "alice",
            &rec_url,
            &json!({ "data": { "title": "出張", "applicant": "alice", "approver": "bob" } }),
        ),
    )
    .await;
    assert_eq!(s, StatusCode::CREATED, "{other}");
    let other_id = other["id"].as_str().unwrap().to_string();
    // 最初の状態のあいだは、申請者が書き換えられる。
    let (s, other) = send(
        &env,
        req(
            "PATCH",
            "alice",
            &format!("{rec_url}/{other_id}"),
            "application/json",
            Body::from(
                json!({ "patch": { "title": "出張（直した）" }, "expected_rev": other["rev"] })
                    .to_string(),
            ),
        ),
    )
    .await;
    assert_eq!(s, StatusCode::OK, "{other}");
    let (s, done) = send(
        &env,
        post_json(
            "bob",
            &format!("{rec_url}/{other_id}/transition"),
            &json!({ "to": "承認", "expected_rev": other["rev"] }),
        ),
    )
    .await;
    assert_eq!(s, StatusCode::OK, "{done}");
    assert_eq!(done["data"]["status"], "承認");
    // 最初の状態を過ぎたら、申請者は書き換えられない（承認者の列も変えられない）。申請者でない bob は書ける。
    let patch = |user: &str, rev: &Value, title: &str| {
        req(
            "PATCH",
            user,
            &format!("{rec_url}/{other_id}"),
            "application/json",
            Body::from(
                json!({ "patch": { "title": title, "approver": "alice" }, "expected_rev": rev })
                    .to_string(),
            ),
        )
    };
    let (s, e) = send(&env, patch("alice", &done["rev"], "書き換え")).await;
    assert_eq!(s, StatusCode::FORBIDDEN, "{e}");
    let (s, e) = send(&env, patch("bob", &done["rev"], "経理が直した")).await;
    assert_eq!(s, StatusCode::OK, "{e}");

    // 列の編集（スキーマの改訂）だけで状態の列の選択肢を変えるのは断る（状態の進め方で変える）。
    let (_, table_now) = send(&env, get("alice", &format!("/data/tables/{}", t.id))).await;
    let mut next = table_now["schema"].clone();
    for f in next["fields"].as_array_mut().unwrap() {
        if f["key"] == "status" {
            f["options"] = json!(["申請中", "承認", "却下", "保留"]);
        }
    }
    let (s, e) = send(
        &env,
        put_json(
            "alice",
            &format!("/data/tables/{}/schema", t.id),
            &json!({ "schema": next, "expected_schema_version": table_now["schema_version"] }),
        ),
    )
    .await;
    assert_eq!(s, StatusCode::BAD_REQUEST, "{e}");
    assert_eq!(
        (e["code"].clone(), e["field"].clone()),
        (json!("fsm_invalid"), json!("status")),
        "{e}"
    );

    // 読めない定義（bob が自分のテーブルに作ったもの）を自分のテーブルに付けるのは断る（中身を読ませない）。
    let mut st = FieldDef::new("phase", FieldType::Select);
    st.options = vec!["申請中".into(), "承認".into(), "却下".into()];
    let bobs = env
        .data
        .create_table(
            &ctx("bob"),
            NewDataTable {
                name: format!("bobs-{}", Uuid::new_v4().simple()),
                schema: TableSchema {
                    fields: vec![st],
                    status_field: Some("phase".into()),
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
    let mut bobs_fsm = approval_fsm(&["申請中", "承認", "却下"], &[("申請中", "承認")]);
    bobs_fsm["transitions"][0]["actor"] = json!("public");
    let (s, made) = send(
        &env,
        post_json("bob", "/data/fsms", &json!({ "name": format!("bob-{}", Uuid::new_v4().simple()), "table_id": bobs.id, "body": bobs_fsm })),
    )
    .await;
    assert_eq!(s, StatusCode::CREATED, "{made}");
    let bobs_artifact: Uuid = serde_json::from_value(made["id"].clone()).unwrap();
    cleanup_artifacts.push(bobs_artifact);
    let mut stolen = table_now["schema"].clone();
    stolen["fsm_ref"] = json!({ "artifact_id": bobs_artifact, "version": 1 });
    let (s, e) = send(
        &env,
        put_json(
            "alice",
            &format!("/data/tables/{}/schema", t.id),
            &json!({ "schema": stolen, "expected_schema_version": table_now["schema_version"] }),
        ),
    )
    .await;
    assert_eq!(s, StatusCode::BAD_REQUEST, "{e}");
    assert_eq!(e["code"], "fsm_invalid", "{e}");

    // 状態の名前を変えて保存し直すと、同じ定義に版を積み、選択肢が変わる（使っているレコードがある「承認」は残す）。
    let renamed = approval_fsm(
        &["申請中", "承認", "差し戻し"],
        &[("申請中", "承認"), ("申請中", "差し戻し")],
    );
    let (s, saved) = send(
        &env,
        put_json("alice", &fsm_url, &save(&renamed, version, json!({}))),
    )
    .await;
    assert_eq!(s, StatusCode::OK, "{saved}");
    assert_eq!(saved["schema"]["fsm_ref"]["artifact_id"], json!(pinned));
    assert_eq!(saved["schema"]["fsm_ref"]["version"], 2);
    let version = saved["schema_version"].as_i64().unwrap();
    // 使っているレコードがある状態（「承認」）を消すと、データと合わないので断る。
    let gone = approval_fsm(&["申請中", "差し戻し"], &[("申請中", "差し戻し")]);
    let (s, e) = send(
        &env,
        put_json("alice", &fsm_url, &save(&gone, version, json!({}))),
    )
    .await;
    assert_eq!(s, StatusCode::BAD_REQUEST, "{e}");
    assert_eq!(e["code"], "schema_conflicts_with_data", "{e}");

    // 承認を待っているレコード（いまの状態 → 判断の行き先）と、このテーブルの状態を使う保存済みのワークフロー。
    let run_id = Uuid::new_v4();
    let workflow_id = Uuid::new_v4();
    sqlx::query(
        "INSERT INTO workflow_run (tenant_id, run_id, org, workflow_id, version, trigger_kind, principal, status) \
         VALUES ($1, $2, 'acme', $3, 1, 'event', $3::text, 'running')",
    )
    .bind(TENANT)
    .bind(run_id)
    .bind(workflow_id)
    .execute(&env.pool)
    .await
    .unwrap();
    sqlx::query(
        "INSERT INTO workflow_approval (tenant_id, approval_id, org, workflow_id, run_id, step_path, node_id, \
           table_id, record_id, title, decisions, from_state, suspend_rev) \
         VALUES ($1, $2, 'acme', $3, $4, 'ok', 'ok', $5, $6::uuid, '経費の承認', $7, '申請中', 1)",
    )
    .bind(TENANT)
    .bind(Uuid::new_v4())
    .bind(workflow_id)
    .bind(run_id)
    .bind(t.id)
    .bind(&own_id)
    .bind(json!([
        { "kind": "approve", "to": "承認", "label": "承認する", "comment": "optional" },
        { "kind": "return", "to": "差し戻し", "label": "差し戻す", "comment": "optional" }
    ]))
    .execute(&env.pool)
    .await
    .unwrap();
    let artifacts = artifact::ArtifactStore::new(env.pool.clone(), Arc::clone(&env.fga));
    let wf = artifacts
        .create(
            &ctx("alice"),
            artifact::NewArtifact {
                kind: artifact::ArtifactKind::Workflow,
                name: format!("expense-{}", Uuid::new_v4().simple()),
                body: json!({
                    "ir_version": 1, "name": "expense", "display_name": "経費の承認", "triggers": [],
                    "nodes": [{ "id": "ok", "type": "human.approval",
                                "params": { "record": { "table": t.id }, "approve": { "to": "承認" },
                                            "return": { "to": "差し戻し" } } }],
                    "edges": []
                }),
            },
            None,
        )
        .await
        .unwrap();
    cleanup_artifacts.push(wf.id);
    let (s, v) = send(&env, get("alice", &fsm_url)).await;
    assert_eq!(s, StatusCode::OK, "{v}");
    assert_eq!(
        v["pending_approvals"],
        json!([{ "from": "申請中", "to": "差し戻し", "count": 1 }, { "from": "申請中", "to": "承認", "count": 1 }]),
        "{v}"
    );
    assert_eq!(
        v["workflows"],
        json!([{ "id": wf.id, "name": "経費の承認", "states": ["差し戻し", "承認"] }]),
        "{v}"
    );
    // 承認を待っているレコードが判断できなくなる（「申請中」→「承認」を消す）変更は、確認が無ければ 409。
    let narrowed = approval_fsm(&["申請中", "承認", "差し戻し"], &[("申請中", "差し戻し")]);
    let (s, e) = send(
        &env,
        put_json("alice", &fsm_url, &save(&narrowed, version, json!({}))),
    )
    .await;
    assert_eq!(s, StatusCode::CONFLICT, "{e}");
    assert_eq!(e["code"], "fsm_in_use", "{e}");
    assert_eq!(
        e["pending"],
        json!([{ "from": "申請中", "to": "承認", "count": 1 }]),
        "{e}"
    );
    let (s, saved) = send(
        &env,
        put_json(
            "alice",
            &fsm_url,
            &save(&narrowed, version, json!({ "confirm_pending": true })),
        ),
    )
    .await;
    assert_eq!(s, StatusCode::OK, "{saved}");
    // 書き込みの決まりを外すと、元の（行の決まりの無い）形に戻る。
    let mut off = save(
        &narrowed,
        saved["schema_version"].as_i64().unwrap(),
        json!({ "confirm_pending": true }),
    );
    off["applicant_write_lock"] = Value::Null;
    let (s, saved) = send(&env, put_json("alice", &fsm_url, &off)).await;
    assert_eq!(s, StatusCode::OK, "{saved}");
    assert!(saved["schema"]["row_policy"].is_null(), "{saved}");

    sqlx::query("DELETE FROM workflow_run WHERE tenant_id = $1 AND run_id = $2")
        .bind(TENANT)
        .bind(run_id)
        .execute(&env.pool)
        .await
        .unwrap();
    purge_artifacts(&env, &cleanup_artifacts).await;
    purge_fsm_artifacts(&env, t.id).await;
    purge_table(&env, t.id).await;
    purge_table(&env, bobs.id).await;
}

/// 作成者の列（D-122・BUG-148）: 列を足すと前からあるレコードも作成者で埋まって見え、だれも書けない（作成・更新・
/// CSV の取込で 400）。状態の進め方の「申請した本人は除く」と書き込みの決まりを作成者の列で付けると、申請者の列
/// （メンバーの列）をほかの人にしても自分の申請は承認できない。申請者の列が空のレコードで止める確かめ（409
/// `fsm_locks_records`）は出ない（作成者は空にならない）。あわせて、定義を読めない編集者にも閲覧者の文脈で
/// 最初の状態が出る（BUG-154）、追加だけの人が状態を進めると 403（BUG-155）。
#[tokio::test]
async fn created_by_column_is_server_filled_and_keeps_creators_from_approving() {
    let Some(env) = setup().await else { return };
    let dir = storage::DirectoryStore::new(env.pool.clone());
    for u in ["alice", "bob", "carol"] {
        dir.upsert_user(u, TENANT, "acme", &format!("{u}@acme.example"), u)
            .await
            .unwrap();
    }
    let t = approval_table(&env, "created-by").await; // alice: 持ち主／bob: 編集者
    let base = format!("/data/tables/{}", t.id);
    let rec_url = format!("{base}/records");
    let fsm_url = format!("{base}/fsm");

    // 列を足す前のレコード（承認済み・申請者の列は空）。
    let (s, before) = send(
        &env,
        post_json(
            "alice",
            &rec_url,
            &json!({ "data": { "title": "前からある", "status": "承認" } }),
        ),
    )
    .await;
    assert_eq!(s, StatusCode::CREATED, "{before}");
    // 作成者の列を足す（索引の宣言は要らない・indexed は立って返る）。
    let (_, table_now) = send(&env, get("alice", &base)).await;
    let mut next = table_now["schema"].clone();
    next["fields"]
        .as_array_mut()
        .unwrap()
        .push(json!({ "key": "made_by", "display_name": "作成者", "type": "created_by" }));
    let (s, saved) = send(
        &env,
        put_json(
            "alice",
            &format!("{base}/schema"),
            &json!({ "schema": next, "expected_schema_version": table_now["schema_version"] }),
        ),
    )
    .await;
    assert_eq!(s, StatusCode::OK, "{saved}");
    let made_by = saved["schema"]["fields"]
        .as_array()
        .unwrap()
        .iter()
        .find(|f| f["key"] == "made_by")
        .cloned()
        .unwrap();
    assert_eq!(
        (made_by["type"].clone(), made_by["indexed"].clone()),
        (json!("created_by"), json!(true))
    );
    let before_id = before["id"].as_str().unwrap();
    let (s, got) = send(&env, get("bob", &format!("{rec_url}/{before_id}"))).await;
    assert_eq!(s, StatusCode::OK, "{got}");
    assert_eq!(
        got["data"]["made_by"], "alice",
        "前からあるレコードも作成者で埋まる"
    );
    // 必須・重複なしは付けられない。
    let mut bad = saved["schema"].clone();
    for f in bad["fields"].as_array_mut().unwrap() {
        if f["key"] == "made_by" {
            f["required"] = json!(true);
        }
    }
    let (s, e) = send(
        &env,
        put_json(
            "alice",
            &format!("{base}/schema"),
            &json!({ "schema": bad, "expected_schema_version": saved["schema_version"] }),
        ),
    )
    .await;
    assert_eq!(s, StatusCode::BAD_REQUEST, "{e}");
    assert_eq!(e["code"], "schema_invalid", "{e}");

    // 書けない: 作成・更新・CSV の取込は 400（黙って捨てない）。
    let (s, e) = send(
        &env,
        post_json(
            "bob",
            &rec_url,
            &json!({ "data": { "title": "x", "made_by": "alice" } }),
        ),
    )
    .await;
    assert_eq!(s, StatusCode::BAD_REQUEST, "{e}");
    assert_eq!(
        (e["code"].clone(), e["field"].clone()),
        (json!("invalid_input"), json!("made_by")),
        "{e}"
    );
    assert_eq!(
        e["message"],
        "「作成者」には書き込めません。作成者の列には、レコードを追加した人が自動で入ります",
        "{e}"
    );
    let (s, e) = send(
        &env,
        req(
            "PATCH",
            "alice",
            &format!("{rec_url}/{before_id}"),
            "application/json",
            Body::from(
                json!({ "patch": { "made_by": "bob" }, "expected_rev": before["rev"] }).to_string(),
            ),
        ),
    )
    .await;
    assert_eq!(s, StatusCode::BAD_REQUEST, "{e}");
    assert_eq!(e["field"], "made_by", "{e}");
    let (s, e) = send(
        &env,
        post_csv(
            "bob",
            &format!("{base}/imports/csv"),
            "title,made_by\n偽,alice\n",
        ),
    )
    .await;
    assert_eq!(s, StatusCode::BAD_REQUEST, "{e}");
    assert!(
        e.to_string()
            .contains("作成者の列には、レコードを追加した人が自動で入ります"),
        "{e}"
    );

    // 状態の進め方: 承認・却下は「承認者の列の人で、作成者は除く」。書き込みの決まりも作成者の列で付ける。
    // 申請者の列が空で最初の状態を過ぎたレコード（前からある）があっても、作成者の列なら確かめは出ない。
    let (_, v) = send(&env, get("alice", &fsm_url)).await;
    let who = json!({ "all": [
        { "field_cmp": { "field": "approver", "op": "eq", "value": "user_id" } },
        { "field_cmp": { "field": "made_by", "op": "ne", "value": "user_id" } }
    ] });
    let body = json!({
        "states": ["申請中", "承認", "却下"],
        "transitions": [
            { "from": "申請中", "to": "承認", "actor": who },
            { "from": "申請中", "to": "却下", "actor": who }
        ],
    });
    let (s, saved) = send(
        &env,
        put_json(
            "alice",
            &fsm_url,
            &json!({
                "status_field": "status",
                "body": body,
                "applicant_write_lock": "made_by",
                "expected_schema_version": v["schema_version"],
            }),
        ),
    )
    .await;
    assert_eq!(s, StatusCode::OK, "{saved}");
    let (_, v) = send(&env, get("alice", &fsm_url)).await;
    assert_eq!(v["applicant_write_lock"], "made_by", "{v}");

    // bob が申請者の列を alice にして申請し、承認者の列も自分にする。作成者は bob なので、自分では承認できない。
    let (s, own) = send(
        &env,
        post_json(
            "bob",
            &rec_url,
            &json!({ "data": { "title": "bob の出張", "applicant": "alice", "approver": "bob" } }),
        ),
    )
    .await;
    assert_eq!(s, StatusCode::CREATED, "{own}");
    assert_eq!(own["data"]["made_by"], "bob");
    let own_id = own["id"].as_str().unwrap().to_string();
    let (s, cand) = send(&env, get("bob", &format!("{rec_url}/{own_id}/transitions"))).await;
    assert_eq!(s, StatusCode::OK, "{cand}");
    assert_eq!(cand["transitions"], json!([]), "{cand}");
    let (s, e) = send(
        &env,
        post_json(
            "bob",
            &format!("{rec_url}/{own_id}/transition"),
            &json!({ "to": "承認", "expected_rev": own["rev"] }),
        ),
    )
    .await;
    assert_eq!(s, StatusCode::FORBIDDEN, "{e}");
    // alice の申請（承認者は bob）は bob が承認できる。
    let (s, other) = send(
        &env,
        post_json(
            "alice",
            &rec_url,
            &json!({ "data": { "title": "alice の出張", "applicant": "alice", "approver": "bob" } }),
        ),
    )
    .await;
    assert_eq!(s, StatusCode::CREATED, "{other}");
    let other_id = other["id"].as_str().unwrap().to_string();
    let (s, done) = send(
        &env,
        post_json(
            "bob",
            &format!("{rec_url}/{other_id}/transition"),
            &json!({ "to": "承認", "expected_rev": other["rev"] }),
        ),
    )
    .await;
    assert_eq!(s, StatusCode::OK, "{done}");
    // 最初の状態を過ぎたら、作成者（alice）は書き換えられない。作成者でない bob は書ける。
    let patch = |user: &str, rev: &Value| {
        req(
            "PATCH",
            user,
            &format!("{rec_url}/{other_id}"),
            "application/json",
            Body::from(json!({ "patch": { "title": "直した" }, "expected_rev": rev }).to_string()),
        )
    };
    let (s, e) = send(&env, patch("alice", &done["rev"])).await;
    assert_eq!(s, StatusCode::FORBIDDEN, "{e}");
    let (s, e) = send(&env, patch("bob", &done["rev"])).await;
    assert_eq!(s, StatusCode::OK, "{e}");
    // 作成者の列で絞り込める（表示名ではなく ID）。
    let (s, q) = send(
        &env,
        post_json(
            "alice",
            &format!("{base}/query"),
            &json!({ "filter": { "cmp": { "field": "made_by", "op": "eq", "value": "bob" } } }),
        ),
    )
    .await;
    assert_eq!(s, StatusCode::OK, "{q}");
    let titles: Vec<&str> = q["items"]
        .as_array()
        .unwrap()
        .iter()
        .map(|it| it["data"]["title"].as_str().unwrap())
        .collect();
    assert_eq!(titles, vec!["bob の出張"], "{q}");

    // BUG-154: 定義を読めない編集者（bob）にも、閲覧者の文脈で最初の状態が出る。
    let (s, vc) = send(&env, get("bob", &format!("{base}/viewer-context"))).await;
    assert_eq!(s, StatusCode::OK, "{vc}");
    assert_eq!(vc["initial_state"], "申請中", "{vc}");
    // BUG-155: 追加だけの人（carol）が状態を進めると、書き換えと同じ 403（無いレコードでも同じ）。
    let (s, b) = send(
        &env,
        put_json(
            "alice",
            &format!("{base}/shares"),
            &json!({ "target": { "type": "user", "id": "carol" }, "role": "submitter" }),
        ),
    )
    .await;
    assert_eq!(s, StatusCode::NO_CONTENT, "{b}");
    let (s, mine) = send(
        &env,
        post_json(
            "carol",
            &rec_url,
            &json!({ "data": { "title": "carol の申請", "approver": "bob" } }),
        ),
    )
    .await;
    assert_eq!(s, StatusCode::CREATED, "{mine}");
    let mine_id = mine["id"].as_str().unwrap().to_string();
    let missing = Uuid::new_v4().to_string();
    let mut moved = Vec::new();
    for id in [&mine_id, &missing] {
        let (s, _) = send(
            &env,
            post_json(
                "carol",
                &format!("{rec_url}/{id}/transition"),
                &json!({ "to": "承認", "expected_rev": 1 }),
            ),
        )
        .await;
        moved.push(s);
    }

    purge_fsm_artifacts(&env, t.id).await;
    purge_table(&env, t.id).await;
    assert_eq!(moved, vec![StatusCode::FORBIDDEN, StatusCode::FORBIDDEN]);
}

/// D-127・GAP-52: 「追加だけ」の申請者（carol）は、差し戻された自分の申請を直して出し直せる（HTTP）。状態の進め方で
/// 差し戻し → 申請中を作成者の列の人に許し、書き込みの決まり（作成者の列）を付けると、carol は差し戻しのあいだだけ
/// 自分のレコードを直して申請中へ進められる。申請中・承認の後の書き換え、削除は 403。画面の手がかり
/// （閲覧者の文脈の `own_records`）と進め先の候補も同じ判定。
#[tokio::test]
async fn add_only_applicant_fixes_and_resubmits_a_sent_back_request() {
    let Some(env) = setup().await else { return };
    let dir = storage::DirectoryStore::new(env.pool.clone());
    for u in ["alice", "bob", "carol"] {
        dir.upsert_user(u, TENANT, "acme", &format!("{u}@acme.example"), u)
            .await
            .unwrap();
    }
    let t = approval_table(&env, "resubmit").await; // alice: 持ち主／bob: 編集者（承認者）
    let base = format!("/data/tables/{}", t.id);
    let rec_url = format!("{base}/records");
    let fsm_url = format!("{base}/fsm");
    let body = async {
        // 作成者の列と「差し戻し」の状態を足す。
        let (_, now) = send(&env, get("alice", &base)).await;
        let mut next = now["schema"].clone();
        let fields = next["fields"].as_array_mut().unwrap();
        fields.push(json!({ "key": "made_by", "display_name": "作成者", "type": "created_by" }));
        for f in fields.iter_mut() {
            if f["key"] == "status" {
                f["options"] = json!(["申請中", "承認", "却下", "差し戻し"]);
            }
        }
        let (s, saved) = send(
            &env,
            put_json(
                "alice",
                &format!("{base}/schema"),
                &json!({ "schema": next, "expected_schema_version": now["schema_version"] }),
            ),
        )
        .await;
        assert_eq!(s, StatusCode::OK, "{saved}");
        // 承認者（作成者は除く）が判断し、差し戻しからは作成者が出し直す。書き込みの決まりは作成者の列。
        let who = json!({ "all": [
            { "field_cmp": { "field": "approver", "op": "eq", "value": "user_id" } },
            { "field_cmp": { "field": "made_by", "op": "ne", "value": "user_id" } }
        ] });
        let creator =
            json!({ "field_cmp": { "field": "made_by", "op": "eq", "value": "user_id" } });
        let (s, saved) = send(
            &env,
            put_json(
                "alice",
                &fsm_url,
                &json!({
                    "status_field": "status",
                    "body": {
                        "states": ["申請中", "承認", "却下", "差し戻し"],
                        "transitions": [
                            { "from": "申請中", "to": "承認", "actor": who },
                            { "from": "申請中", "to": "却下", "actor": who },
                            { "from": "申請中", "to": "差し戻し", "actor": who },
                            { "from": "差し戻し", "to": "申請中", "actor": creator }
                        ],
                    },
                    "applicant_write_lock": "made_by",
                    "expected_schema_version": saved["schema_version"],
                }),
            ),
        )
        .await;
        assert_eq!(s, StatusCode::OK, "{saved}");
        let (s, b) = send(
            &env,
            put_json(
                "alice",
                &format!("{base}/shares"),
                &json!({ "target": { "type": "user", "id": "carol" }, "role": "submitter" }),
            ),
        )
        .await;
        assert_eq!(s, StatusCode::NO_CONTENT, "{b}");

        // carol（追加だけ）が申請する。画面の手がかり: 差し戻しから進め、差し戻しのあいだ直せる。
        let (s, rec) = send(
            &env,
            post_json(
                "carol",
                &rec_url,
                &json!({ "data": { "title": "carol の出張", "approver": "bob" } }),
            ),
        )
        .await;
        assert_eq!(s, StatusCode::CREATED, "{rec}");
        let id = rec["id"].as_str().unwrap().to_string();
        let (_, vc) = send(&env, get("carol", &format!("{base}/viewer-context"))).await;
        assert_eq!(
            vc["own_records"],
            json!({ "move_from": ["差し戻し"], "edit_in": ["差し戻し"] }),
            "{vc}"
        );
        let (_, bob_vc) = send(&env, get("bob", &format!("{base}/viewer-context"))).await;
        assert!(bob_vc.get("own_records").is_none(), "{bob_vc}");
        let patch = |rev: &Value| {
            req(
                "PATCH",
                "carol",
                &format!("{rec_url}/{id}"),
                "application/json",
                Body::from(
                    json!({ "patch": { "title": "carol の出張（直した）" }, "expected_rev": rev })
                        .to_string(),
                ),
            )
        };
        let mv = |user: &str, to: &str, rev: &Value| {
            post_json(
                user,
                &format!("{rec_url}/{id}/transition"),
                &json!({ "to": to, "expected_rev": rev }),
            )
        };
        // 申請中: 直せない・進められない。
        let (s, e) = send(&env, patch(&rec["rev"])).await;
        assert_eq!(s, StatusCode::FORBIDDEN, "申請中の書き換え: {e}");
        let (s, cand) = send(&env, get("carol", &format!("{rec_url}/{id}/transitions"))).await;
        assert_eq!(
            (s, cand["transitions"].clone()),
            (StatusCode::OK, json!([])),
            "{cand}"
        );
        // bob が差し戻す → carol は直して出し直せる。
        let (s, back) = send(&env, mv("bob", "差し戻し", &rec["rev"])).await;
        assert_eq!(s, StatusCode::OK, "{back}");
        let (s, cand) = send(&env, get("carol", &format!("{rec_url}/{id}/transitions"))).await;
        assert_eq!(
            (s, cand["transitions"].clone()),
            (StatusCode::OK, json!(["申請中"])),
            "{cand}"
        );
        let (s, e) = send(
            &env,
            req(
                "DELETE",
                "carol",
                &format!("{rec_url}/{id}?expected_rev={}", back["rev"]),
                "application/json",
                Body::empty(),
            ),
        )
        .await;
        assert_eq!(s, StatusCode::FORBIDDEN, "削除: {e}");
        let (s, fixed) = send(&env, patch(&back["rev"])).await;
        assert_eq!(s, StatusCode::OK, "差し戻しの書き換え: {fixed}");
        assert_eq!(fixed["data"]["title"], "carol の出張（直した）");
        assert_eq!(fixed["data"]["made_by"], "carol");
        let (s, again) = send(&env, mv("carol", "申請中", &fixed["rev"])).await;
        assert_eq!(s, StatusCode::OK, "出し直し: {again}");
        assert_eq!(again["data"]["status"], "申請中");
        let (s, e) = send(&env, patch(&again["rev"])).await;
        assert_eq!(s, StatusCode::FORBIDDEN, "出し直した後: {e}");
        // bob が承認する。承認の後も carol は何もできない。
        let (s, done) = send(&env, mv("bob", "承認", &again["rev"])).await;
        assert_eq!(s, StatusCode::OK, "{done}");
        let (s, e) = send(&env, patch(&done["rev"])).await;
        assert_eq!(s, StatusCode::FORBIDDEN, "承認の後: {e}");
        // 作成の監査・状態を進めた監査は carol で残る。
        let (n,): (i64,) = sqlx::query_as(
            "SELECT count(*) FROM audit_log WHERE tenant_id = $1 AND actor = 'carol' \
               AND action IN ('data.record.update', 'data.record.transition') AND decision = 'allow' \
               AND object_id = $2",
        )
        .bind(TENANT)
        .bind(&id)
        .fetch_one(&env.pool)
        .await
        .unwrap();
        assert_eq!(n, 2, "直した・出し直したの監査");
    };
    // 落ちても片付けてから結果を返す。
    let outcome = futures::FutureExt::catch_unwind(std::panic::AssertUnwindSafe(body)).await;
    purge_fsm_artifacts(&env, t.id).await;
    purge_table(&env, t.id).await;
    if let Err(e) = outcome {
        std::panic::resume_unwind(e);
    }
}
