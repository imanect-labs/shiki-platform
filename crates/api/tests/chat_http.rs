//! チャット HTTP ルート（Task 3.6）の統合テスト。
//!
//! `STORAGE_TEST_DATABASE_URL` がある時のみ実行（DB 直の ChatStore を AppState に載せる）。
//! セッション Cookie ＋ double-submit CSRF で認証を通し、/threads 系ハンドラの
//! 作成・一覧・取得・送信(202)・共有・キャンセル・認可(401/404/503) を検証する。

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::print_stdout,
    clippy::print_stderr,
    clippy::pedantic
)]

use std::{sync::Arc, time::Duration};

use api::{
    build_router,
    config::*,
    session::{MemorySessionStore, SessionRecord, SessionStore},
    state::AppState,
};
use async_trait::async_trait;
use authz::{
    AuthzClient, AuthzError, Consistency, FgaObject, ObjectType, Principal, ReadTupleKey, Relation,
    Subject,
};
use axum::{
    body::Body,
    http::{header::COOKIE, Request, StatusCode},
};
use http_body_util::BodyExt;
use sqlx::postgres::PgPoolOptions;
use tower::ServiceExt;

struct AllowAll;

#[async_trait]
impl AuthzClient for AllowAll {
    async fn check(
        &self,
        _s: &Subject,
        _r: Relation,
        _o: &FgaObject,
        _c: Consistency,
    ) -> Result<bool, AuthzError> {
        Ok(true)
    }
    async fn write_tuple(
        &self,
        _s: &Subject,
        _r: Relation,
        _o: &FgaObject,
    ) -> Result<bool, AuthzError> {
        Ok(true)
    }
    async fn delete_tuple(
        &self,
        _s: &Subject,
        _r: Relation,
        _o: &FgaObject,
    ) -> Result<bool, AuthzError> {
        Ok(true)
    }
    async fn read_tuples(
        &self,
        _o: &FgaObject,
        _r: Option<Relation>,
    ) -> Result<Vec<ReadTupleKey>, AuthzError> {
        Ok(vec![])
    }
    async fn list_objects(
        &self,
        _s: &Subject,
        _r: Relation,
        _t: ObjectType,
    ) -> Result<Vec<String>, AuthzError> {
        Ok(vec![])
    }
    async fn delete_object_tuples(&self, _o: &FgaObject) -> Result<u32, AuthzError> {
        Ok(0)
    }
    async fn read_subject_objects(
        &self,
        _s: &Subject,
        _t: ObjectType,
    ) -> Result<Vec<String>, AuthzError> {
        Ok(vec![])
    }
}

struct FakeStore;

#[async_trait]
impl storage::object_store::ObjectStore for FakeStore {
    async fn ensure_bucket(&self) -> Result<(), storage::ObjectStoreError> {
        Ok(())
    }
    async fn presign_get_internal(
        &self,
        _key: &str,
        _ttl: Duration,
    ) -> Result<String, storage::ObjectStoreError> {
        Ok("http://fake/internal".into())
    }
    async fn presign_put(
        &self,
        _key: &str,
        _ttl: Duration,
        _len: i64,
    ) -> Result<String, storage::ObjectStoreError> {
        Ok("http://test/put".into())
    }
    async fn presign_get(
        &self,
        _key: &str,
        _ttl: Duration,
        _filename: Option<&str>,
        _content_type: Option<&str>,
    ) -> Result<String, storage::ObjectStoreError> {
        Ok("http://test/get".into())
    }
    async fn read_and_hash(&self, _key: &str) -> Result<(String, u64), storage::ObjectStoreError> {
        Err(storage::ObjectStoreError::NotFound("test".into()))
    }
    async fn put_object(
        &self,
        _key: &str,
        _bytes: Vec<u8>,
        _content_type: &str,
    ) -> Result<(), storage::ObjectStoreError> {
        Ok(())
    }
    async fn get_object(&self, _key: &str) -> Result<Vec<u8>, storage::ObjectStoreError> {
        Err(storage::ObjectStoreError::NotFound("test".into()))
    }
    async fn exists(&self, _key: &str) -> Result<bool, storage::ObjectStoreError> {
        Ok(false)
    }
    async fn copy(&self, _src: &str, _dst: &str) -> Result<(), storage::ObjectStoreError> {
        Ok(())
    }
    async fn delete(&self, _key: &str) -> Result<(), storage::ObjectStoreError> {
        Ok(())
    }
    async fn list_prefix(
        &self,
        _prefix: &str,
        _continuation: Option<&str>,
    ) -> Result<(Vec<String>, Option<String>), storage::ObjectStoreError> {
        Ok((vec![], None))
    }
    async fn delete_batch(&self, _keys: &[String]) -> Result<(), storage::ObjectStoreError> {
        Ok(())
    }
}

fn base_config(db_url: &str) -> AppConfig {
    AppConfig {
        server: ServerConfig {
            host: "0.0.0.0".into(),
            port: 0,
            cors_allowed_origins: vec![],
        },
        database: DatabaseConfig {
            url: db_url.into(),
            max_connections: 4,
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
        workflow: api::workflow_runtime::WorkflowConfig::default(),
        gateway: api::config::GatewayConfig::default(),
        tabular: api::config::TabularConfig::default(),
        office: api::config::OfficeConfig::default(),
    }
}

fn test_principal() -> Principal {
    Principal {
        kind: authz::PrincipalKind::User,
        id: "00000000-0000-0000-0000-000000000001".into(),
        email: Some("alice@acme.example".into()),
        groups: vec!["/acme".into()],
        roles: vec!["engineering".into()],
        tenant_id: None,
    }
}

fn session_record(csrf: &str) -> SessionRecord {
    SessionRecord {
        principal: test_principal(),
        tenant_id: "default".into(),
        access_token: "access".into(),
        refresh_token: None,
        id_token: None,
        access_expires_at: chrono::Utc::now().timestamp() + 3600,
        csrf_token: csrf.into(),
        keycloak_sid: None,
    }
}

/// 実 DB の AppState を組む（chat 有効／無効を選べる）。DB 未設定なら None。
async fn build_state(with_chat: bool) -> Option<(AppState, Arc<dyn SessionStore>)> {
    let db_url = std::env::var("STORAGE_TEST_DATABASE_URL").ok()?;
    let pool = PgPoolOptions::new()
        .max_connections(4)
        .connect(&db_url)
        .await
        .expect("connect test DB");
    sqlx::migrate!("../../migrations")
        .run(&pool)
        .await
        .expect("migrate");
    let config = base_config(&db_url);
    let jwks = Arc::new(api::middleware::JwksCache::new(
        reqwest::Client::new(),
        config.auth.effective_jwks_uri(),
        Duration::from_secs(300),
    ));
    let storage = Arc::new(storage::StorageService::new(
        pool.clone(),
        Arc::new(FakeStore),
        Arc::new(AllowAll),
        Duration::from_secs(120),
        Duration::from_secs(900),
        5 * 1024 * 1024 * 1024,
    ));
    let directory = Arc::new(storage::DirectoryStore::new(pool.clone()));
    let tenants = Arc::new(storage::TenantStore::new(pool.clone()));
    let rag_admin = Arc::new(rag::RagAdmin::new(pool.clone(), None, None));
    let chat = if with_chat {
        Some(Arc::new(
            chat::ChatStore::connect(pool.clone(), Arc::new(AllowAll), None)
                .await
                .expect("chat store"),
        ))
    } else {
        None
    };
    let sessions: Arc<dyn SessionStore> = Arc::new(MemorySessionStore::new());
    let artifacts = Arc::new(artifact::ArtifactStore::new(
        pool.clone(),
        Arc::new(AllowAll),
    ));
    let workflows = Arc::new(workflow_engine::WorkflowStore::new(Arc::clone(&artifacts)));
    let ui_validator = Arc::new(gui::SpecValidator::new(
        Arc::clone(&artifacts),
        pool.clone(),
    ));
    let ui_specs = Arc::new(gui::UiSpecStore::new(Arc::clone(&artifacts), ui_validator));
    let ui_actions = Arc::new(gui::ActionDispatcher::new(
        storage::audit::AuditRecorder::new(pool.clone()),
    ));
    let skills = Arc::new(gui::SkillStore::new(Arc::clone(&artifacts)));
    let mini_apps = Arc::new(gui::MiniAppStore::new(Arc::clone(&artifacts), pool.clone()));
    let data_store = Arc::new(data::DataStore::new(
        pool.clone(),
        Arc::new(AllowAll),
        Arc::new(api::data_refs::ApiRefResolver {
            directory: Arc::clone(&directory),
            storage: Arc::clone(&storage),
        }),
    ));
    let data_views = Arc::new(data::DataViewStore::new(
        Arc::clone(&artifacts),
        (*data_store).clone(),
    ));
    let fsms = Arc::new(data::FsmStore::new(
        Arc::clone(&artifacts),
        (*data_store).clone(),
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
        Arc::new(AllowAll),
        None,
        vec![],
    ));
    let skill_installs = Arc::new(app_platform::SkillInstallService::new(
        pool.clone(),
        app_platform::Registry::new(pool.clone()),
        app_platform::TrustedKeyStore::new(pool.clone()),
        Arc::clone(&artifacts),
        Arc::new(AllowAll),
    ));
    let bundles = Arc::new(app_platform::BundleStore::new(
        Arc::new(FakeStore),
        Arc::new(AllowAll),
        storage::audit::AuditRecorder::new(pool.clone()),
    ));
    let app_usage = Arc::new(app_platform::AppUsageStore::new(
        pool.clone(),
        Arc::new(AllowAll),
    ));
    let workflow_registration = Arc::new(workflow_engine::RegistrationService::new(
        pool.clone(),
        workflow_engine::DelegationStore::new(pool.clone(), Arc::new(AllowAll)),
    ));
    let audit_rec = Arc::new(storage::audit::AuditRecorder::new(pool.clone()));
    let workflow_summaries = Arc::new(workflow_engine::WorkflowSummaryStore::new(pool.clone()));
    let workflow_layout = Arc::new(workflow_engine::EditorLayoutStore::new(pool.clone()));
    let collab_hub = Arc::new(collab::CollabHub::new(
        pool.clone(),
        Arc::new(AllowAll),
        Arc::clone(&storage),
    ));
    let tabular_svc = std::sync::Arc::new(tabular::TabularService::new(
        std::sync::Arc::clone(&storage),
        tabular::RunnerConfig::new("shiki-tabular-runner", std::time::Duration::from_secs(5)),
        tabular::Quotas::default(),
    ));
    let state = AppState {
        config: Arc::new(config),
        db: api::state::ReadinessProbe::new(pool),
        authz: Arc::new(AllowAll),
        jwks,
        sessions: sessions.clone(),
        http: reqwest::Client::new(),
        storage,
        collab: collab_hub,
        tabular: tabular_svc,
        artifacts,
        data: data_store,
        data_views,
        fsms,
        mini_app_code,
        installs,
        skill_installs,
        bundles,
        app_usage,
        ui_specs,
        ui_actions,
        skills,
        mini_apps,
        secrets: None,
        workflows,
        workflow_launcher: None,
        workflow_registration,
        workflow_summaries,
        workflow_layout,
        audit: audit_rec,
        workflow_runs: None,
        directory,
        tenants,
        search: None,
        chat,
        llm: None,
        rag_admin,
        office: None,
        docx_composer: std::sync::Arc::new(office::DocxComposer::new(
            reqwest::Client::new(),
            "http://127.0.0.1:1",
        )),
    };
    Some((state, sessions))
}

/// 有効セッションを 1 つ入れ、Cookie 文字列と CSRF を返す。
async fn login(sessions: &Arc<dyn SessionStore>, sid: &str) -> (String, &'static str) {
    let csrf = "csrf-tok";
    sessions
        .put(
            "default",
            sid,
            &session_record(csrf),
            Duration::from_secs(3600),
        )
        .await
        .unwrap();
    (
        format!("shiki_session={sid}.default; shiki_csrf={csrf}"),
        csrf,
    )
}

async fn body_json(resp: axum::response::Response) -> serde_json::Value {
    let bytes = resp.into_body().collect().await.unwrap().to_bytes();
    serde_json::from_slice(&bytes).unwrap_or(serde_json::Value::Null)
}

#[tokio::test]
async fn chat_thread_lifecycle_over_http() {
    let Some((state, sessions)) = build_state(true).await else {
        eprintln!("STORAGE_TEST_DATABASE_URL 未設定のためスキップ");
        return;
    };
    let (cookie, csrf) = login(&sessions, "sid-chat-1").await;
    let app = build_router(state);

    // 作成（200）。
    let resp = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/threads")
                .header(COOKIE, &cookie)
                .header("x-csrf-token", csrf)
                .header("content-type", "application/json")
                .body(Body::from(r#"{"title":"e2e","agent_mode":false}"#))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let thread = body_json(resp).await;
    let tid = thread["id"].as_str().unwrap().to_string();

    // 一覧（200・作成分を含む）。
    let resp = app
        .clone()
        .oneshot(
            Request::builder()
                .uri("/threads")
                .header(COOKIE, &cookie)
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let list = body_json(resp).await;
    assert!(list["threads"]
        .as_array()
        .unwrap()
        .iter()
        .any(|t| t["id"] == tid.as_str()));

    // 取得（200）。
    let resp = app
        .clone()
        .oneshot(
            Request::builder()
                .uri(format!("/threads/{tid}"))
                .header(COOKIE, &cookie)
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);

    // 送信（202・接続非依存）。
    let resp = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri(format!("/threads/{tid}/messages"))
                .header(COOKIE, &cookie)
                .header("x-csrf-token", csrf)
                .header("content-type", "application/json")
                .body(Body::from(r#"{"text":"こんにちは"}"#))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::ACCEPTED);
    let posted = body_json(resp).await;
    let run_id = posted["run_id"].as_str().unwrap().to_string();

    // メッセージ一覧（200・user+assistant プレースホルダ）。
    let resp = app
        .clone()
        .oneshot(
            Request::builder()
                .uri(format!("/threads/{tid}/messages"))
                .header(COOKIE, &cookie)
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let msgs = body_json(resp).await;
    assert_eq!(msgs["messages"].as_array().unwrap().len(), 2);

    // 共有（204）→ 一覧（200）→ 解除（204）。
    let share_body = r#"{"target":{"type":"user","id":"bob"},"role":"viewer"}"#;
    let resp = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri(format!("/threads/{tid}/shares"))
                .header(COOKIE, &cookie)
                .header("x-csrf-token", csrf)
                .header("content-type", "application/json")
                .body(Body::from(share_body))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::NO_CONTENT);

    let resp = app
        .clone()
        .oneshot(
            Request::builder()
                .uri(format!("/threads/{tid}/shares"))
                .header(COOKIE, &cookie)
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let shares = body_json(resp).await;
    // 応答形状のみ検証（この統合テストの AllowAll モックはタプルを永続しないため件数は不問）。
    assert!(shares["shares"].is_array());

    let resp = app
        .clone()
        .oneshot(
            Request::builder()
                .method("DELETE")
                .uri(format!("/threads/{tid}/shares"))
                .header(COOKIE, &cookie)
                .header("x-csrf-token", csrf)
                .header("content-type", "application/json")
                .body(Body::from(share_body))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::NO_CONTENT);

    // キャンセル（204）。
    let resp = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri(format!("/threads/{tid}/runs/{run_id}/cancel"))
                .header(COOKIE, &cookie)
                .header("x-csrf-token", csrf)
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::NO_CONTENT);
}

#[tokio::test]
async fn chat_authz_and_availability_edges() {
    // 未認証は 401。
    let Some((state, sessions)) = build_state(true).await else {
        return;
    };
    let (cookie, _csrf) = login(&sessions, "sid-chat-2").await;
    let app = build_router(state);

    let resp = app
        .clone()
        .oneshot(
            Request::builder()
                .uri("/threads")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::UNAUTHORIZED);

    // 存在しないスレッドは 404。
    let missing = uuid::Uuid::new_v4();
    let resp = app
        .oneshot(
            Request::builder()
                .uri(format!("/threads/{missing}"))
                .header(COOKIE, &cookie)
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::NOT_FOUND);

    // chat 無効なら 503。
    let Some((state, sessions)) = build_state(false).await else {
        return;
    };
    let (cookie, csrf) = login(&sessions, "sid-chat-3").await;
    let app = build_router(state);
    let resp = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/threads")
                .header(COOKIE, &cookie)
                .header("x-csrf-token", csrf)
                .header("content-type", "application/json")
                .body(Body::from("{}"))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::SERVICE_UNAVAILABLE);
}

/// 投稿時のモデル指定（#553）: 組織で使えないモデルは 400、指定・Auto は run に記録される。
#[tokio::test]
async fn chat_post_resolves_model_choice() {
    let Some((mut state, sessions)) = build_state(true).await else {
        eprintln!("STORAGE_TEST_DATABASE_URL 未設定のためスキップ");
        return;
    };
    let db_url = std::env::var("STORAGE_TEST_DATABASE_URL").unwrap();
    let pool = PgPoolOptions::new().connect(&db_url).await.unwrap();
    // stub プロバイダに 2 モデル（"fast" は思考強度を受ける・"local" は受けない）。
    let gw = llm_gateway::LlmGateway::build(
        pool.clone(),
        reqwest::Client::new(),
        llm_gateway::GatewayConfig {
            catalog: llm_gateway::ModelCatalog {
                default_model: "fast".into(),
                models: vec![
                    llm_gateway::ModelEntry {
                        id: "fast".into(),
                        supports_effort: Some(true),
                        ..Default::default()
                    },
                    llm_gateway::ModelEntry {
                        id: "local".into(),
                        ..Default::default()
                    },
                ],
            },
            ..Default::default()
        },
    )
    .unwrap();
    state.llm = Some(Arc::new(gw));
    let (cookie, csrf) = login(&sessions, "sid-chat-model").await;
    let app = build_router(state);

    let post = |uri: String, body: String| {
        Request::builder()
            .method("POST")
            .uri(uri)
            .header(COOKIE, &cookie)
            .header("x-csrf-token", csrf)
            .header("content-type", "application/json")
            .body(Body::from(body))
            .unwrap()
    };
    let resp = app
        .clone()
        .oneshot(post("/threads".into(), r#"{"title":"model"}"#.into()))
        .await
        .unwrap();
    let tid = body_json(resp).await["id"].as_str().unwrap().to_string();
    let messages = format!("/threads/{tid}/messages");

    // カタログに無いモデルは 400（理由つき）。
    let resp = app
        .clone()
        .oneshot(post(
            messages.clone(),
            r#"{"text":"x","model":"ghost"}"#.into(),
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
    assert!(body_json(resp).await["detail"]
        .as_str()
        .unwrap()
        .contains("ghost"));

    // 選んだモデル＋思考強度は run に記録される。
    let resp = app
        .clone()
        .oneshot(post(
            messages.clone(),
            r#"{"text":"x","model":"fast","effort":"high"}"#.into(),
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::ACCEPTED);
    let run_id: uuid::Uuid = body_json(resp).await["run_id"]
        .as_str()
        .unwrap()
        .parse()
        .unwrap();
    let row: (Option<String>, Option<String>, bool) =
        sqlx::query_as("SELECT model, effort, model_auto FROM generation_run WHERE run_id = $1")
            .bind(run_id)
            .fetch_one(&pool)
            .await
            .unwrap();
    assert_eq!(row, (Some("fast".into()), Some("high".into()), false));

    // 思考強度に対応しないモデルは effort を記録しない。省略は Auto（ここでは既定モデルへ）。
    let resp = app
        .clone()
        .oneshot(post(
            messages.clone(),
            r#"{"text":"x","model":"local","effort":"high"}"#.into(),
        ))
        .await
        .unwrap();
    let run_id: uuid::Uuid = body_json(resp).await["run_id"]
        .as_str()
        .unwrap()
        .parse()
        .unwrap();
    let row: (Option<String>, Option<String>, bool) =
        sqlx::query_as("SELECT model, effort, model_auto FROM generation_run WHERE run_id = $1")
            .bind(run_id)
            .fetch_one(&pool)
            .await
            .unwrap();
    assert_eq!(row, (Some("local".into()), None, false));

    let resp = app
        .clone()
        .oneshot(post(messages, r#"{"text":"x"}"#.into()))
        .await
        .unwrap();
    let run_id: uuid::Uuid = body_json(resp).await["run_id"]
        .as_str()
        .unwrap()
        .parse()
        .unwrap();
    let row: (Option<String>, Option<String>, bool) =
        sqlx::query_as("SELECT model, effort, model_auto FROM generation_run WHERE run_id = $1")
            .bind(run_id)
            .fetch_one(&pool)
            .await
            .unwrap();
    assert_eq!(row, (Some("fast".into()), Some("low".into()), true));
}

/// 発話ごとのツール指定（#734）: 未知名・件数超過は 400（理由つき）、指定は重複を除いて run に記録される。
#[tokio::test]
async fn chat_post_records_requested_tools() {
    let Some((state, sessions)) = build_state(true).await else {
        eprintln!("STORAGE_TEST_DATABASE_URL 未設定のためスキップ");
        return;
    };
    let db_url = std::env::var("STORAGE_TEST_DATABASE_URL").unwrap();
    let pool = PgPoolOptions::new().connect(&db_url).await.unwrap();
    let (cookie, csrf) = login(&sessions, "sid-chat-tools").await;
    let app = build_router(state);

    let post = |uri: String, body: String| {
        Request::builder()
            .method("POST")
            .uri(uri)
            .header(COOKIE, &cookie)
            .header("x-csrf-token", csrf)
            .header("content-type", "application/json")
            .body(Body::from(body))
            .unwrap()
    };
    let resp = app
        .clone()
        .oneshot(post("/threads".into(), r#"{"title":"tools"}"#.into()))
        .await
        .unwrap();
    let tid = body_json(resp).await["id"].as_str().unwrap().to_string();
    let messages = format!("/threads/{tid}/messages");

    // 語彙に無い名前は 400（Json 抽出の 422 ではなく、理由つきの検証エラー）。
    let resp = app
        .clone()
        .oneshot(post(
            messages.clone(),
            r#"{"text":"x","tools":["web_search","rm_rf"]}"#.into(),
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
    let detail = body_json(resp).await["detail"]
        .as_str()
        .unwrap()
        .to_string();
    assert!(
        detail.contains("rm_rf") && detail.contains("未知のツール名"),
        "{detail}"
    );

    // 重複を除いて上限超過は 400。
    let resp = app
        .clone()
        .oneshot(post(
            messages.clone(),
            r#"{"text":"x","tools":["skill","skill_search","doc_search","doc_read","web_search","web_fetch","code_interpreter","fs_list","fs_read"]}"#.into(),
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
    assert!(body_json(resp).await["detail"]
        .as_str()
        .unwrap()
        .contains("件まで"));

    // 有効な指定は 202 で受理され、重複を除いて投入順のまま run に記録される。
    let resp = app
        .clone()
        .oneshot(post(
            messages,
            r#"{"text":"x","tools":["web_search","csv.query","web_search"]}"#.into(),
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::ACCEPTED);
    let run_id: uuid::Uuid = body_json(resp).await["run_id"]
        .as_str()
        .unwrap()
        .parse()
        .unwrap();
    let stored: serde_json::Value =
        sqlx::query_scalar("SELECT requested_tools FROM generation_run WHERE run_id = $1")
            .bind(run_id)
            .fetch_one(&pool)
            .await
            .unwrap();
    assert_eq!(stored, serde_json::json!(["web_search", "csv.query"]));
}
