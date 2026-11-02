//! CSV クエリ/パッチ API のルートレベル結合テスト（Task 11P.7・実 Postgres＋RustFS＋隔離ランナー）。
//!
//! `STORAGE_TEST_DATABASE_URL`＋`STORAGE_TEST_S3_ENDPOINT`＋隔離ランナーバイナリが揃う時のみ
//! 実行する（未設定はスキップ）。HTTP ルータ経由で schema/rows/query/patch/save を通し、
//! 楽観ロック 409・SQL 拒否 400 を検証する（routes/tabular.rs のカバレッジ）。
//!
//! CSV の編集と同じく元の文字コードで書き戻す、ドライブのテキストの表示と編集（`/files/{id}/text`・
//! routes/file_text.rs・D-119）も、同じ組み立て（実 Postgres＋RustFS）でここで確かめる。

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::print_stderr,
    clippy::pedantic
)]

use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use api::{
    build_router,
    config::*,
    session::{MemorySessionStore, SessionStore},
    state::AppState,
};
use async_trait::async_trait;
use authz::{
    AuthzClient, AuthzError, Consistency, FgaObject, ObjectType, Principal, ReadTupleKey, Relation,
    Subject,
};
use axum::body::Body;
use axum::http::{header::COOKIE, Request, StatusCode};
use http_body_util::BodyExt;
use serde_json::{json, Value};
use sqlx::postgres::PgPoolOptions;
use tower::ServiceExt;
use uuid::Uuid;

struct AllowAll;

#[async_trait]
impl AuthzClient for AllowAll {
    async fn check(
        &self,
        _: &Subject,
        _: Relation,
        _: &FgaObject,
        _: Consistency,
    ) -> Result<bool, AuthzError> {
        Ok(true)
    }
    async fn write_tuple(
        &self,
        _: &Subject,
        _: Relation,
        _: &FgaObject,
    ) -> Result<bool, AuthzError> {
        Ok(true)
    }
    async fn delete_tuple(
        &self,
        _: &Subject,
        _: Relation,
        _: &FgaObject,
    ) -> Result<bool, AuthzError> {
        Ok(true)
    }
    async fn read_tuples(
        &self,
        _: &FgaObject,
        _: Option<Relation>,
    ) -> Result<Vec<ReadTupleKey>, AuthzError> {
        Ok(vec![])
    }
    async fn list_objects(
        &self,
        _: &Subject,
        _: Relation,
        _: ObjectType,
    ) -> Result<Vec<String>, AuthzError> {
        Ok(vec![])
    }
    async fn delete_object_tuples(&self, _: &FgaObject) -> Result<u32, AuthzError> {
        Ok(0)
    }
    async fn read_subject_objects(
        &self,
        _: &Subject,
        _: ObjectType,
    ) -> Result<Vec<String>, AuthzError> {
        Ok(vec![])
    }
}

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

fn runner_path() -> Option<PathBuf> {
    if let Ok(p) = std::env::var("SHIKI_TABULAR_RUNNER") {
        let p = PathBuf::from(p);
        if p.exists() {
            return Some(p);
        }
    }
    // ワークスペースルートから見た除外クレートの既定ビルド先。
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .parent()?
        .parent()?
        .to_path_buf();
    for profile in ["release", "debug"] {
        let p = root
            .join("crates/tabular/runner/target")
            .join(profile)
            .join("shiki-tabular-runner");
        if p.exists() {
            return Some(p);
        }
    }
    None
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
        membership_reconcile: api::membership_reconcile::MembershipReconcileConfig::default(),
    }
}

struct Env {
    app: axum::Router,
    storage: Arc<storage::StorageService>,
    ctx: authz::AuthContext,
}

async fn setup() -> Option<Env> {
    let Ok(db_url) = std::env::var("STORAGE_TEST_DATABASE_URL") else {
        eprintln!("STORAGE_TEST_DATABASE_URL 未設定のためスキップ");
        return None;
    };
    let Some(runner) = runner_path() else {
        eprintln!("隔離ランナー未ビルドのためスキップ");
        return None;
    };
    let s3_endpoint = std::env::var("STORAGE_TEST_S3_ENDPOINT")
        .unwrap_or_else(|_| "http://localhost:9000".into());
    let access_key =
        std::env::var("STORAGE_TEST_S3_ACCESS_KEY").unwrap_or_else(|_| "minioadmin".into());
    let secret_key =
        std::env::var("STORAGE_TEST_S3_SECRET_KEY").unwrap_or_else(|_| "minioadmin".into());

    let pool = PgPoolOptions::new()
        .max_connections(5)
        .connect(&db_url)
        .await
        .expect("pg");
    sqlx::migrate!("../../migrations")
        .run(&pool)
        .await
        .expect("migrate");

    let authz: Arc<dyn AuthzClient> = Arc::new(AllowAll);
    let s3 = storage::object_store::S3Config {
        internal_endpoint: s3_endpoint.clone(),
        public_endpoint: s3_endpoint,
        bucket: format!("tabtest-{}", Uuid::new_v4().simple()),
        access_key,
        secret_key,
        region: "us-east-1".into(),
        presign_get_ttl_secs: 900,
        presign_put_ttl_secs: 900,
        cors_allowed_origins: vec![],
    };
    let object_store = Arc::new(storage::S3ObjectStore::new(&s3)) as Arc<dyn storage::ObjectStore>;
    object_store.ensure_bucket().await.expect("bucket");
    let storage = Arc::new(storage::StorageService::new(
        pool.clone(),
        object_store,
        Arc::clone(&authz),
        Duration::from_secs(120),
        Duration::from_secs(900),
        64 * 1024 * 1024,
    ));
    let tabular = Arc::new(tabular::TabularService::new(
        Arc::clone(&storage),
        tabular::RunnerConfig::new(
            runner.to_string_lossy().to_string(),
            Duration::from_secs(20),
        ),
        tabular::Quotas::default(),
    ));

    let sessions = Arc::new(MemorySessionStore::new());
    sessions
        .put(
            "default",
            "sid-tab",
            &api::session::SessionRecord {
                principal: principal(),
                tenant_id: "default".into(),
                access_token: "a".into(),
                refresh_token: None,
                id_token: None,
                access_expires_at: chrono::Utc::now().timestamp() + 3600,
                csrf_token: "csrf-tab".into(),
                keycloak_sid: None,
            },
            Duration::from_secs(3600),
        )
        .await
        .unwrap();

    let app = build_app(
        &pool,
        config(&db_url),
        storage.clone(),
        tabular,
        sessions,
        &authz,
    );
    Some(Env {
        app,
        storage,
        ctx: authz::AuthContext::new(principal(), "acme".into(), "default".into()),
    })
}

fn principal() -> Principal {
    Principal {
        kind: authz::PrincipalKind::User,
        id: "alice".into(),
        email: None,
        groups: vec!["/acme".into()],
        roles: vec![],
        tenant_id: None,
    }
}

/// 最小 AppState を組んでルータを返す（tabular ルートのみ叩く）。
fn build_app(
    pool: &sqlx::PgPool,
    config: AppConfig,
    storage: Arc<storage::StorageService>,
    tabular: Arc<tabular::TabularService>,
    sessions: Arc<MemorySessionStore>,
    authz: &Arc<dyn AuthzClient>,
) -> axum::Router {
    let artifacts = Arc::new(artifact::ArtifactStore::new(
        pool.clone(),
        Arc::clone(authz),
    ));
    let directory = Arc::new(storage::DirectoryStore::new(pool.clone()));
    let data_store = Arc::new(data::DataStore::new(
        pool.clone(),
        Arc::clone(authz),
        Arc::new(api::data_refs::ApiRefResolver {
            directory: Arc::clone(&directory),
            storage: Arc::clone(&storage),
        }),
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

fn get(app: &axum::Router, path: &str) -> Request<Body> {
    let _ = app;
    Request::builder()
        .method("GET")
        .uri(path)
        .header(COOKIE, "shiki_session=sid-tab.default")
        .body(Body::empty())
        .unwrap()
}

fn post(path: &str, body: Value) -> Request<Body> {
    Request::builder()
        .method("POST")
        .uri(path)
        .header(COOKIE, "shiki_session=sid-tab.default; shiki_csrf=csrf-tab")
        .header("content-type", "application/json")
        .header("x-csrf-token", "csrf-tab")
        .body(Body::from(body.to_string()))
        .unwrap()
}

fn put(path: &str, body: Value) -> Request<Body> {
    Request::builder()
        .method("PUT")
        .uri(path)
        .header(COOKIE, "shiki_session=sid-tab.default; shiki_csrf=csrf-tab")
        .header("content-type", "application/json")
        .header("x-csrf-token", "csrf-tab")
        .body(Body::from(body.to_string()))
        .unwrap()
}

async fn json_body(resp: axum::response::Response) -> (StatusCode, Value) {
    let status = resp.status();
    let bytes = resp.into_body().collect().await.unwrap().to_bytes();
    let v = serde_json::from_slice(&bytes).unwrap_or(Value::Null);
    (status, v)
}

#[tokio::test]
async fn tabular_schema_rows_query_patch_flow() {
    let Some(env) = setup().await else { return };
    // CSV を作成（storage 経由）。中身は実行ごとに変える。blob は中身のハッシュで共有され、テストはバケットを実行ごとに
    // 作り直すので、同じ中身だと前の実行の blob の行（前のバケットの本体）を指して読めない（BUG-143）。
    let name = format!("data-{}.csv", Uuid::new_v4());
    let csv = format!("id,score\n1-{},10\n2,30\n3,20\n", Uuid::new_v4().simple());
    let node = env
        .storage
        .write_file_internal(&env.ctx, None, &name, csv.as_bytes(), "text/csv", None)
        .await
        .expect("create csv");
    let id = node.id;

    // schema。
    let (st, body) = json_body(
        env.app
            .clone()
            .oneshot(get(&env.app, &format!("/files/{id}/tabular/schema")))
            .await
            .unwrap(),
    )
    .await;
    assert_eq!(st, StatusCode::OK, "{body:?}");
    assert_eq!(body["columns"], json!(["id", "score"]));
    assert_eq!(body["total_rows"], json!(3));

    // rows。
    let (st, body) = json_body(
        env.app
            .clone()
            .oneshot(get(&env.app, &format!("/files/{id}/tabular/rows?offset=0")))
            .await
            .unwrap(),
    )
    .await;
    assert_eq!(st, StatusCode::OK);
    assert_eq!(body["rows"].as_array().unwrap().len(), 3);

    // query（RO SELECT）。
    let (st, body) = json_body(
        env.app
            .clone()
            .oneshot(post(
                &format!("/files/{id}/tabular/query"),
                json!({"sql": "SELECT id FROM data WHERE CAST(score AS INT) >= 20 ORDER BY id"}),
            ))
            .await
            .unwrap(),
    )
    .await;
    assert_eq!(st, StatusCode::OK, "{body:?}");
    assert_eq!(body["rows"].as_array().unwrap().len(), 2);

    // query（DML は 400 拒否）。
    let (st, _b) = json_body(
        env.app
            .clone()
            .oneshot(post(
                &format!("/files/{id}/tabular/query"),
                json!({"sql": "DROP TABLE data"}),
            ))
            .await
            .unwrap(),
    )
    .await;
    assert_eq!(st, StatusCode::BAD_REQUEST);

    // patch（cell 更新・base_rev=現行）。
    let (st, body) = json_body(
        env.app
            .clone()
            .oneshot(post(
                &format!("/files/{id}/tabular/patch"),
                json!({"base_rev": node.version, "ops": [
                    {"op": "cell_update", "row": 0, "col": 1, "value": "999"}
                ]}),
            ))
            .await
            .unwrap(),
    )
    .await;
    assert_eq!(st, StatusCode::OK, "{body:?}");
    let new_ver = body["version"].as_i64().unwrap();
    assert_eq!(new_ver, node.version + 1);

    // patch（古い base_rev は 409）。
    let (st, _b) = json_body(
        env.app
            .clone()
            .oneshot(post(
                &format!("/files/{id}/tabular/patch"),
                json!({"base_rev": node.version, "ops": [
                    {"op": "cell_update", "row": 0, "col": 1, "value": "x"}
                ]}),
            ))
            .await
            .unwrap(),
    )
    .await;
    assert_eq!(st, StatusCode::CONFLICT);

    // save（新規 CSV）。
    let (st, body) = json_body(
        env.app
            .clone()
            .oneshot(post(
                "/tabular/save",
                json!({"parent_id": null, "name": format!("saved-{}", Uuid::new_v4()), "csv": "a,b\n1,2\n"}),
            ))
            .await
            .unwrap(),
    )
    .await;
    assert_eq!(st, StatusCode::OK, "{body:?}");
    assert!(body["node_id"].is_string());
}

/// CSV の画面（schema・rows・patch）も、Excel が保存する文字コードの CSV を読む（GAP-45）。Shift_JIS の CSV を
/// 日本語の列名で読み、編集は Shift_JIS のまま保存する。読めない CSV と、Shift_JIS に無い文字の編集は、画面が
/// そのまま出せる文言つきの構造化 400（`invalid_input`）で断る。
#[tokio::test]
async fn tabular_reads_shift_jis_and_explains_what_it_cannot_read_or_write() {
    let Some(env) = setup().await else { return };
    // 中身は実行ごとに変える（中身の同じ blob は前の実行のバケットを指すので、同じ DB で流し直すと読めない）。
    let tag = Uuid::new_v4().simple().to_string();
    let csv = format!("経費,金額,番号\n交通費,1200,{tag}\n");
    let (sjis, _, _) = textenc::encoding_rs::SHIFT_JIS.encode(&csv);
    let node = env
        .storage
        .write_file_internal(
            &env.ctx,
            None,
            &format!("経費-{}.csv", Uuid::new_v4()),
            &sjis,
            "application/vnd.ms-excel",
            None,
        )
        .await
        .expect("create csv");
    let latin = env
        .storage
        .write_file_internal(
            &env.ctx,
            None,
            &format!("latin-{}.csv", Uuid::new_v4()),
            &[b"name\ncaf\xe9 ".as_slice(), tag.as_bytes(), b"\n"].concat(),
            "text/csv",
            None,
        )
        .await
        .expect("create csv");
    let call = |req: Request<Body>| {
        let app = env.app.clone();
        async move { json_body(app.oneshot(req).await.unwrap()).await }
    };
    let id = node.id;
    let schema = call(get(&env.app, &format!("/files/{id}/tabular/schema"))).await;
    let rows = call(get(&env.app, &format!("/files/{id}/tabular/rows?offset=0"))).await;
    let patched = call(post(
        &format!("/files/{id}/tabular/patch"),
        json!({"base_rev": node.version, "ops": [
            {"op": "cell_update", "row": 0, "col": 1, "value": "1500"}
        ]}),
    ))
    .await;
    let unmappable = call(post(
        &format!("/files/{id}/tabular/patch"),
        json!({"base_rev": node.version + 1, "ops": [
            {"op": "cell_update", "row": 0, "col": 0, "value": "😀"}
        ]}),
    ))
    .await;
    let (_, saved) = env
        .storage
        .read_file_internal(&env.ctx, id, None)
        .await
        .unwrap();
    let unreadable = call(get(
        &env.app,
        &format!("/files/{}/tabular/schema", latin.id),
    ))
    .await;
    // 片付け（作った CSV）。
    for file in [id, latin.id] {
        env.storage
            .soft_delete_file(&env.ctx, file, None)
            .await
            .unwrap();
    }

    assert_eq!(schema.0, StatusCode::OK, "{:?}", schema.1);
    assert_eq!(schema.1["columns"], json!(["経費", "金額", "番号"]));
    assert_eq!(rows.0, StatusCode::OK, "{:?}", rows.1);
    assert_eq!(rows.1["rows"], json!([["交通費", "1200", tag]]));
    assert_eq!(patched.0, StatusCode::OK, "{:?}", patched.1);
    let edited = textenc::decode_text(&saved).unwrap();
    assert_eq!(
        (edited.encoding.name(), edited.text.as_str()),
        (
            "Shift_JIS",
            format!("経費,金額,番号\n交通費,1500,{tag}\n").as_str()
        ),
        "Shift_JIS のまま保存する"
    );
    assert_eq!(unmappable.0, StatusCode::BAD_REQUEST, "{:?}", unmappable.1);
    assert_eq!(unmappable.1["code"], "invalid_input");
    assert!(
        unmappable.1["message"]
            .as_str()
            .unwrap()
            .contains("「😀」は、この CSV の文字コード（Shift_JIS）では書けません"),
        "{:?}",
        unmappable.1
    );
    assert_eq!(unreadable.0, StatusCode::BAD_REQUEST, "{:?}", unreadable.1);
    assert_eq!(unreadable.1["code"], "invalid_input");
    assert_eq!(
        unreadable.1["message"],
        "この CSV の中身は文章として読めません。UTF-8 か Shift_JIS で保存した CSV を選んでください。"
    );
}

/// Excel の「Unicode テキスト」（UTF-16LE・BOM 付き・タブ区切り・CRLF）の編集（BUG-144）。集計はタブで列を分けて
/// 読む。編集も集計と同じ判定（隔離ランナーの区切りの推定）で区切りを決め、タブ区切り・CRLF・UTF-16LE と BOM の
/// まま書き戻す。以前は区切りが「,」で決まっていたので、1 行が 1 列になり、2 列目の編集は範囲外で断られていた。
#[tokio::test]
async fn tabular_patch_keeps_the_tab_delimiter_of_excel_unicode_text() {
    let Some(env) = setup().await else { return };
    let tag = Uuid::new_v4().simple().to_string();
    let text = format!("品目\t金額\t番号\r\n交通費\t1200\t{tag}\r\n宿泊費\t9800\t2\r\n");
    let utf16 = textenc::encode_like(&text, textenc::encoding_rs::UTF_16LE, true).unwrap();
    let node = env
        .storage
        .write_file_internal(
            &env.ctx,
            None,
            &format!("一覧-{}.csv", Uuid::new_v4()),
            &utf16,
            "text/csv",
            None,
        )
        .await
        .expect("create csv");
    let call = |req: Request<Body>| {
        let app = env.app.clone();
        async move { json_body(app.oneshot(req).await.unwrap()).await }
    };
    let id = node.id;
    let query = call(post(
        &format!("/files/{id}/tabular/query"),
        json!({"sql": "SELECT \"品目\", \"金額\" FROM data ORDER BY \"金額\""}),
    ))
    .await;
    let patched = call(post(
        &format!("/files/{id}/tabular/patch"),
        json!({"base_rev": node.version, "ops": [
            {"op": "cell_update", "row": 0, "col": 1, "value": "1500"},
            {"op": "row_insert", "at": 2, "values": ["日当", "3000", "3"]}
        ]}),
    ))
    .await;
    let (_, saved) = env
        .storage
        .read_file_internal(&env.ctx, id, None)
        .await
        .unwrap();
    // 片付け（作った CSV）。
    env.storage
        .soft_delete_file(&env.ctx, id, None)
        .await
        .unwrap();

    assert_eq!(query.0, StatusCode::OK, "{:?}", query.1);
    assert_eq!(query.1["columns"], json!(["品目", "金額"]));
    assert_eq!(patched.0, StatusCode::OK, "{:?}", patched.1);
    let edited = textenc::decode_text(&saved).unwrap();
    assert_eq!(
        (edited.encoding.name(), edited.bom, edited.text.as_str()),
        (
            "UTF-16LE",
            true,
            format!(
                "品目\t金額\t番号\r\n交通費\t1500\t{tag}\r\n宿泊費\t9800\t2\r\n日当\t3000\t3\r\n"
            )
            .as_str()
        ),
        "タブ区切り・CRLF・UTF-16LE と BOM のまま保存する"
    );
}

/// 新規 CSV の保存で同じ名前があるとき: 省略は今のまま 409（code は name_taken・BUG-150）で、`add_number` はノートと
/// 同じ規則（`名前 (2).csv`）で空いている名前に保存する（ドライブの新規作成・D-112）。(50) まで埋まれば 409 の
/// name_taken。
#[tokio::test]
async fn tabular_save_numbers_taken_names_when_asked() {
    let Some(env) = setup().await else { return };
    let folder = env
        .storage
        .create_folder(
            &env.ctx,
            None,
            &format!("csv-name-{}", Uuid::new_v4().simple()),
            None,
        )
        .await
        .unwrap();
    let save = |extra: Value| {
        let mut body = json!({
            "parent_id": folder.id,
            "name": "無題のスプレッドシート",
            "csv": "列1,列2\n,\n",
        });
        if let (Value::Object(b), Value::Object(e)) = (&mut body, extra) {
            b.extend(e);
        }
        let app = env.app.clone();
        async move { json_body(app.oneshot(post("/tabular/save", body)).await.unwrap()).await }
    };

    let mut names = vec![];
    for _ in 0..2 {
        let (st, body) = save(json!({ "on_name_taken": "add_number" })).await;
        assert_eq!(st, StatusCode::OK, "{body:?}");
        names.push(body["name"].as_str().unwrap().to_string());
    }
    assert_eq!(
        names,
        [
            "無題のスプレッドシート.csv",
            storage::numbered_name("無題のスプレッドシート.csv", 2).as_str()
        ]
    );
    assert_eq!(names[1], "無題のスプレッドシート (2).csv");
    // 省略と fail は今のまま 409。名前の衝突は code の name_taken で言い、編集の重なり（rev の衝突）と分ける
    // （画面が「CSV が他の編集で更新されました」と取り違えない・BUG-150）。
    for extra in [json!({}), json!({ "on_name_taken": "fail" })] {
        let (st, body) = save(extra).await;
        assert_eq!(st, StatusCode::CONFLICT, "{body:?}");
        assert_eq!(body["code"], "name_taken", "{body:?}");
        assert!(body.get("current_rev").is_none(), "{body:?}");
    }
    // 番号を付けて保存は (50) まで埋まると、名前の衝突として断る（黙って上書きしない）。
    for n in 3..=storage::MAX_NUMBERED_NAME {
        let (st, body) = save(json!({ "on_name_taken": "add_number" })).await;
        assert_eq!(st, StatusCode::OK, "{n}: {body:?}");
        assert_eq!(
            body["name"],
            storage::numbered_name("無題のスプレッドシート.csv", n)
        );
    }
    let (st, body) = save(json!({ "on_name_taken": "add_number" })).await;
    assert_eq!(st, StatusCode::CONFLICT, "{body:?}");
    assert_eq!(body["code"], "name_taken", "{body:?}");

    env.storage
        .soft_delete_folder(&env.ctx, folder.id, None)
        .await
        .unwrap();
}

/// ドライブのテキストの表示と編集（BUG-141・D-119）。文字コードはサーバーが決め、保存は元の文字コードと BOM の
/// まま書き戻す（以前の画面は Shift_JIS のファイルを BOM の無い UTF-8 で保存していた）。改行コードは送られた
/// とおり。元の文字コードに無い文字・文字コードを決められないファイルの保存・開いた後のほかの保存は、中身を変えずに
/// 断る。
#[tokio::test]
async fn drive_text_edit_writes_back_in_the_original_encoding() {
    use textenc::encoding_rs::{SHIFT_JIS, UTF_16LE, UTF_8};
    let Some(env) = setup().await else { return };
    // 中身は実行ごとに変える（中身の同じ blob は前の実行のバケットを指すので、同じ DB で流し直すと読めない）。
    let tag = Uuid::new_v4().simple().to_string();
    let memo = format!("交通費の精算\r\n期限は月末\r\n{tag}\r\n");
    let (sjis, _, _) = SHIFT_JIS.encode(&memo);
    let bom_csv = format!("品目,金額\n交通費,1200\n{tag},1\n");
    let mut files = Vec::new();
    for (name, bytes, ct) in [
        ("メモ.txt", sjis.to_vec(), "text/plain"),
        (
            "表.csv",
            textenc::encode_like(&bom_csv, UTF_8, true).unwrap(),
            "text/csv",
        ),
        (
            "unicode.txt",
            textenc::encode_like(&memo, UTF_16LE, true).unwrap(),
            "text/plain",
        ),
        (
            "画像.txt",
            [b"\x89PNG\r\n\x1a\n\x00".as_slice(), tag.as_bytes()].concat(),
            "text/plain",
        ),
        (
            "latin.txt",
            [b"caf\xe9 ".as_slice(), tag.as_bytes()].concat(),
            "text/plain",
        ),
        (
            "大きい.txt",
            [tag.as_bytes(), &vec![b'a'; 2 * 1024 * 1024 + 1]].concat(),
            "text/plain",
        ),
    ] {
        let node = env
            .storage
            .write_file_internal(&env.ctx, None, &format!("{tag}-{name}"), &bytes, ct, None)
            .await
            .expect("create file");
        files.push(node);
    }
    let call = |req: Request<Body>| {
        let app = env.app.clone();
        async move { json_body(app.oneshot(req).await.unwrap()).await }
    };
    let text = |i: usize| call(get(&env.app, &format!("/files/{}/text", files[i].id)));
    let save = |i: usize, text: &str, base: i64| {
        call(put(
            &format!("/files/{}/text", files[i].id),
            json!({ "text": text, "base_version": base }),
        ))
    };
    let raw = |i: usize| {
        let storage = Arc::clone(&env.storage);
        let ctx = env.ctx.clone();
        let id = files[i].id;
        async move { storage.read_file_internal(&ctx, id, None).await.unwrap() }
    };

    let opened = text(0).await;
    let edited = format!("交通費の精算（4\u{301C}6月）\r\n期限は月末\r\n{tag}\r\n");
    let saved = save(0, &edited, files[0].version).await;
    let (after_save, sjis_saved) = raw(0).await;
    let stale = save(0, "古い版の上に保存", files[0].version).await;
    let unmappable = save(0, "絵文字😀", after_save.version).await;
    let (after_refusal, sjis_after_refusal) = raw(0).await;
    let csv_opened = text(1).await;
    let csv_saved = save(
        1,
        &format!("品目,金額\n交通費,1500\n{tag},1\n"),
        files[1].version,
    )
    .await;
    let (_, csv_bytes) = raw(1).await;
    let utf16_opened = text(2).await;
    let utf16_text = format!("ユニコードの本文\r\n{tag}\r\n");
    let utf16_saved = save(2, &utf16_text, files[2].version).await;
    let (_, utf16_bytes) = raw(2).await;
    let binary = text(3).await;
    let latin = text(4).await;
    let latin_saved = save(4, "café", files[4].version).await;
    let too_large = text(5).await;
    // 片付け（作ったファイル）。
    for node in &files {
        env.storage
            .soft_delete_file(&env.ctx, node.id, None)
            .await
            .unwrap();
    }

    assert_eq!(opened.0, StatusCode::OK, "{:?}", opened.1);
    assert_eq!(
        (
            &opened.1["text"],
            &opened.1["encoding"],
            &opened.1["bom"],
            &opened.1["version"]
        ),
        (
            &json!(memo),
            &json!("Shift_JIS"),
            &json!(false),
            &json!(files[0].version)
        ),
        "Shift_JIS のファイルを文字化けせずに読む（改行コードはそのまま）"
    );
    assert_eq!(saved.0, StatusCode::OK, "{:?}", saved.1);
    assert_eq!(saved.1["version"], files[0].version + 1);
    let decoded = textenc::decode_text(&sjis_saved).unwrap();
    assert_eq!(
        (decoded.encoding.name(), decoded.bom, decoded.text.as_str()),
        (
            "Shift_JIS",
            false,
            format!("交通費の精算（4\u{FF5E}6月）\r\n期限は月末\r\n{tag}\r\n").as_str()
        ),
        "Shift_JIS のまま保存する（〜 は ～ にする）"
    );
    assert_eq!(stale.0, StatusCode::CONFLICT, "{:?}", stale.1);
    assert_eq!(stale.1["code"], "version_conflict");
    assert_eq!(stale.1["current_version"], after_save.version);
    assert_eq!(unmappable.0, StatusCode::BAD_REQUEST, "{:?}", unmappable.1);
    assert_eq!(unmappable.1["code"], "invalid_input");
    assert_eq!(
        unmappable.1["message"],
        "「😀」は、このファイルの文字コード（Shift_JIS）では保存できません。別の文字に置き換えてください。"
    );
    assert_eq!(
        after_refusal.version, after_save.version,
        "断ったときは書かない"
    );
    assert_eq!(sjis_after_refusal, sjis_saved);

    assert_eq!(csv_opened.0, StatusCode::OK, "{:?}", csv_opened.1);
    assert_eq!(
        (
            &csv_opened.1["text"],
            &csv_opened.1["encoding"],
            &csv_opened.1["bom"]
        ),
        (&json!(bom_csv), &json!("UTF-8"), &json!(true)),
        "BOM は中身に含めず、あったことを返す"
    );
    assert_eq!(csv_saved.0, StatusCode::OK, "{:?}", csv_saved.1);
    assert_eq!(
        csv_bytes,
        textenc::encode_like(&format!("品目,金額\n交通費,1500\n{tag},1\n"), UTF_8, true).unwrap(),
        "BOM を付けたまま保存する"
    );
    assert_eq!(
        utf16_opened.1["encoding"], "UTF-16LE",
        "{:?}",
        utf16_opened.1
    );
    assert_eq!(utf16_saved.0, StatusCode::OK, "{:?}", utf16_saved.1);
    assert_eq!(
        utf16_bytes,
        textenc::encode_like(&utf16_text, UTF_16LE, true).unwrap()
    );
    assert_eq!(binary.0, StatusCode::BAD_REQUEST, "{:?}", binary.1);
    assert_eq!(
        binary.1["message"],
        "テキストではないため表示できません。ダウンロードして、お使いのアプリで開いてください。"
    );
    // 文字コードを決められないファイルは見るだけ（置き換え文字で読む）。保存は断る。
    assert_eq!(latin.0, StatusCode::OK, "{:?}", latin.1);
    assert_eq!(latin.1["encoding"], Value::Null);
    assert!(latin.1["text"]
        .as_str()
        .unwrap()
        .starts_with("caf\u{FFFD} "));
    assert_eq!(
        latin_saved.0,
        StatusCode::BAD_REQUEST,
        "{:?}",
        latin_saved.1
    );
    assert!(latin_saved.1["message"]
        .as_str()
        .unwrap()
        .starts_with("このファイルの文字コードを決められないため、保存できません。"));
    assert_eq!(too_large.0, StatusCode::BAD_REQUEST, "{:?}", too_large.1);
    assert!(too_large.1["message"]
        .as_str()
        .unwrap()
        .starts_with("2 MB を超えるテキストは表示できません。"));
}

/// MS-DOS の頃の帳票（末尾に EOF の印の 0x1A・ログの色の指定の ESC）を含む Shift_JIS のテキストも、ドライブで表示・
/// 保存できる（以前の画面でも見られた。UTF-8 と同じく NUL だけで「テキストではない」とする）。保存は Shift_JIS の
/// まま、0x1A も残して書き戻す。
#[tokio::test]
async fn drive_text_reads_shift_jis_with_a_dos_eof_mark() {
    use textenc::encoding_rs::SHIFT_JIS;
    let Some(env) = setup().await else { return };
    let tag = Uuid::new_v4().simple().to_string();
    let report = format!("売上日報\r\n\u{1B}[31m在庫少\u{1B}[0m\r\n{tag}\r\n\u{1A}");
    let (sjis, _, _) = SHIFT_JIS.encode(&report);
    let node = env
        .storage
        .write_file_internal(
            &env.ctx,
            None,
            &format!("{tag}-日報.txt"),
            &sjis,
            "text/plain",
            None,
        )
        .await
        .expect("create file");
    let call = |req: Request<Body>| {
        let app = env.app.clone();
        async move { json_body(app.oneshot(req).await.unwrap()).await }
    };
    let opened = call(get(&env.app, &format!("/files/{}/text", node.id))).await;
    let edited = format!("売上日報（修正）\r\n\u{1B}[31m在庫少\u{1B}[0m\r\n{tag}\r\n\u{1A}");
    let saved = call(put(
        &format!("/files/{}/text", node.id),
        json!({ "text": edited, "base_version": node.version }),
    ))
    .await;
    let (_, bytes) = env
        .storage
        .read_file_internal(&env.ctx, node.id, None)
        .await
        .unwrap();
    // 片付け（作ったファイル）。
    env.storage
        .soft_delete_file(&env.ctx, node.id, None)
        .await
        .unwrap();

    assert_eq!(opened.0, StatusCode::OK, "{:?}", opened.1);
    assert_eq!(
        (&opened.1["text"], &opened.1["encoding"]),
        (&json!(report), &json!("Shift_JIS"))
    );
    assert_eq!(saved.0, StatusCode::OK, "{:?}", saved.1);
    let (want, _, _) = SHIFT_JIS.encode(&edited);
    assert_eq!(
        bytes,
        want.to_vec(),
        "Shift_JIS のまま、0x1A も残して保存する"
    );
    assert_eq!(bytes.last(), Some(&0x1A));
}
