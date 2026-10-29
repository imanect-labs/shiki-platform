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

/// AllowAll が返す共有タプル（tenant `default` の名前空間）。alice は同テナント・同 org の
/// ディレクトリに居る／mallory は別テナントにだけ居る／ghost は未登録／eng は同テナントのロール。
const SHARE_FIXTURE: &[(&str, &str)] = &[
    ("user:default|share741-alice", "viewer"),
    ("user:default|share741-alice", "editor"),
    ("user:default|share741-mallory", "viewer"),
    ("user:default|share741-ghost", "viewer"),
    ("role:default|share741-eng#member", "editor"),
];

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
    /// 共有一覧（ノード/アーティファクト/スレッド）が読む直接タプル。どのオブジェクトにも
    /// [`SHARE_FIXTURE`] の共有が書かれている体にする（#741 の表示名解決の検証用）。
    async fn read_tuples(
        &self,
        o: &FgaObject,
        _r: Option<Relation>,
    ) -> Result<Vec<ReadTupleKey>, AuthzError> {
        Ok(SHARE_FIXTURE
            .iter()
            .map(|(user, relation)| ReadTupleKey {
                user: (*user).into(),
                relation: (*relation).into(),
                object: o.as_str().into(),
            })
            .collect())
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
        insights: std::sync::Arc::new(api::insights::InsightsService::new(pool.clone())),
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

/// 共有一覧の各エントリが表示名を正しく持つか（#741）。同テナント・同 org の user は名前と email、
/// 別テナントにだけ居る user・未登録 user は両方 null、ロールは名前のみ（email null）。
fn assert_share_names(entries: &serde_json::Value) {
    let entries = entries.as_array().expect("shares array");
    assert_eq!(entries.len(), SHARE_FIXTURE.len(), "{entries:?}");
    let by_id = |id: &str| -> Vec<&serde_json::Value> {
        entries.iter().filter(|e| e["target"]["id"] == id).collect()
    };
    let alice = by_id("share741-alice");
    assert_eq!(alice.len(), 2, "viewer/editor の 2 行");
    for a in alice {
        assert_eq!(a["target"]["type"], "user");
        assert_eq!(a["display_name"], "Alice 741");
        assert_eq!(a["email"], "share741-alice@acme.example");
    }
    for id in ["share741-mallory", "share741-ghost"] {
        let e = by_id(id);
        assert_eq!(e.len(), 1);
        assert!(
            e[0]["display_name"].is_null(),
            "越境/未登録は名前なし: {:?}",
            e[0]
        );
        assert!(
            e[0]["email"].is_null(),
            "越境/未登録は email なし: {:?}",
            e[0]
        );
    }
    let eng = by_id("share741-eng");
    assert_eq!(eng.len(), 1);
    assert_eq!(eng[0]["target"]["type"], "role");
    assert_eq!(eng[0]["display_name"], "技術部 741");
    assert!(eng[0]["email"].is_null());
}

/// #741: ノード・アーティファクト・スレッドの共有一覧が、呼び出し元テナント（＋ org）の
/// ディレクトリから解決した表示名/email を同じ応答で返す（別テナントの名前は返さない）。
#[tokio::test]
async fn share_lists_include_directory_names_tenant_scoped() {
    let Some((state, sessions)) = build_state(true).await else {
        eprintln!("STORAGE_TEST_DATABASE_URL 未設定のためスキップ");
        return;
    };
    // セッションの主体は tenant `default`・org `acme`（groups `/acme`）。
    state
        .directory
        .upsert_user(
            "share741-alice",
            "default",
            "acme",
            "share741-alice@acme.example",
            "Alice 741",
        )
        .await
        .expect("seed alice");
    // 同じ id・同じ org 文字列でも別テナントの行は解決に使わない。
    state
        .directory
        .upsert_user(
            "share741-mallory",
            "other-741",
            "acme",
            "share741-mallory@other.example",
            "Mallory 741",
        )
        .await
        .expect("seed mallory");
    state
        .directory
        .upsert_role("share741-eng", "default", "acme", "技術部 741")
        .await
        .expect("seed role");
    let ctx = authz::AuthContext::new(test_principal(), "acme".into(), "default".into());
    let art = state
        .artifacts
        .create(
            &ctx,
            artifact::NewArtifact {
                kind: artifact::ArtifactKind::Skill,
                name: format!("share741-{}", uuid::Uuid::new_v4().simple()),
                body: serde_json::json!({}),
            },
            None,
        )
        .await
        .expect("artifact");

    let (cookie, csrf) = login(&sessions, "sid-share-741").await;
    let app = build_router(state);
    let post = |uri: &str, body: String| {
        Request::builder()
            .method("POST")
            .uri(uri)
            .header(COOKIE, &cookie)
            .header("x-csrf-token", csrf)
            .header("content-type", "application/json")
            .body(Body::from(body))
            .unwrap()
    };
    let get = |uri: String| {
        Request::builder()
            .uri(uri)
            .header(COOKIE, &cookie)
            .body(Body::empty())
            .unwrap()
    };

    let resp = app
        .clone()
        .oneshot(post(
            "/threads",
            r#"{"title":"share741","agent_mode":false}"#.into(),
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let tid = body_json(resp).await["id"].as_str().unwrap().to_string();

    let folder_name = format!("share741-{}", uuid::Uuid::new_v4().simple());
    let resp = app
        .clone()
        .oneshot(post(
            "/folders",
            serde_json::json!({ "name": folder_name }).to_string(),
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let fid = body_json(resp).await["id"].as_str().unwrap().to_string();

    let resp = app
        .clone()
        .oneshot(get(format!("/threads/{tid}/shares")))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    assert_share_names(&body_json(resp).await["shares"]);

    let resp = app
        .clone()
        .oneshot(get(format!("/nodes/{fid}/shares")))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    assert_share_names(&body_json(resp).await);

    let resp = app
        .oneshot(get(format!("/artifacts/{}/shares", art.id)))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    assert_share_names(&body_json(resp).await);
}

/// 発話ごとのツール指定（#734）: 未知名・指定できないツール（破壊系・内部用）は 400（理由つき）、
/// 指定は重複を除いて run に記録される。
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

    // 語彙にはあるが利用者が指定できないツール（shell 等）も 400（ユーザー指定へ格上げしない）。
    for bad in ["shell", "fs_delete", "emit_workflow", "save_note"] {
        let resp = app
            .clone()
            .oneshot(post(
                messages.clone(),
                format!(r#"{{"text":"x","tools":["web_search","{bad}"]}}"#),
            ))
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::BAD_REQUEST, "{bad}");
        let detail = body_json(resp).await["detail"]
            .as_str()
            .unwrap()
            .to_string();
        assert!(
            detail.contains(bad) && detail.contains("指定できないツール"),
            "{bad}: {detail}"
        );
    }

    // 有効な指定は 202 で受理され、重複を除いて投入順のまま run に記録される。
    let resp = app
        .clone()
        .oneshot(post(
            messages,
            r#"{"text":"x","tools":["web_search","doc_search","web_search"]}"#.into(),
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
    assert_eq!(stored, serde_json::json!(["web_search", "doc_search"]));
}

/// サイドバー履歴の行操作: 改名・ピン留め（`?pinned=true` の一覧）・削除（以後 404）。
#[tokio::test]
async fn chat_thread_rename_pin_delete_over_http() {
    let Some((state, sessions)) = build_state(true).await else {
        eprintln!("STORAGE_TEST_DATABASE_URL 未設定のためスキップ");
        return;
    };
    let (cookie, csrf) = login(&sessions, "sid-chat-manage").await;
    let app = build_router(state);
    let send = |method: &str, uri: String, body: Option<&str>| {
        let mut req = Request::builder()
            .method(method)
            .uri(uri)
            .header(COOKIE, &cookie);
        if method != "GET" {
            req = req.header("x-csrf-token", csrf);
        }
        let req = match body {
            Some(b) => req
                .header("content-type", "application/json")
                .body(Body::from(b.to_string())),
            None => req.body(Body::empty()),
        };
        app.clone().oneshot(req.unwrap())
    };

    let resp = send("POST", "/threads".into(), Some(r#"{"title":"仮"}"#))
        .await
        .unwrap();
    let tid = body_json(resp).await["id"].as_str().unwrap().to_string();

    // 改名・ピン留め（204）→ 取得に反映。
    let resp = send(
        "PATCH",
        format!("/threads/{tid}"),
        Some(r#"{"title":"  新しい名前 ","pinned":true}"#),
    )
    .await
    .unwrap();
    assert_eq!(resp.status(), StatusCode::NO_CONTENT);
    let got = body_json(send("GET", format!("/threads/{tid}"), None).await.unwrap()).await;
    assert_eq!(got["title"], "新しい名前");
    assert!(got["pinned_at"].is_string(), "ピン留め時刻が付く: {got}");

    let pinned = body_json(
        send("GET", "/threads?pinned=true".into(), None)
            .await
            .unwrap(),
    )
    .await;
    assert!(pinned["threads"]
        .as_array()
        .unwrap()
        .iter()
        .any(|t| t["id"] == tid.as_str()));
    assert!(pinned["next_cursor"].is_null());

    // 項目なし・空タイトルは 400。
    for body in [r"{}", r#"{"title":"   "}"#] {
        let resp = send("PATCH", format!("/threads/{tid}"), Some(body))
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::BAD_REQUEST, "{body}");
    }

    // 削除（204）→ 取得・メッセージ・再削除は 404。
    let resp = send("DELETE", format!("/threads/{tid}"), None)
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::NO_CONTENT);
    for (method, uri) in [
        ("GET", format!("/threads/{tid}")),
        ("GET", format!("/threads/{tid}/messages")),
        ("DELETE", format!("/threads/{tid}")),
    ] {
        let resp = send(method, uri.clone(), None).await.unwrap();
        assert_eq!(resp.status(), StatusCode::NOT_FOUND, "{method} {uri}");
    }
    let pinned = body_json(
        send("GET", "/threads?pinned=true".into(), None)
            .await
            .unwrap(),
    )
    .await;
    assert!(!pinned["threads"]
        .as_array()
        .unwrap()
        .iter()
        .any(|t| t["id"] == tid.as_str()));
}

/// カスタム指示（#900）: 本人の分の読み書き・上限超過の 400（画面へ出す `detail` 付き）・
/// 未認証 401・CSRF 無しの書き込み拒否・chat 無効の 503。
#[tokio::test]
async fn custom_instructions_round_trip_over_http() {
    let Some((state, sessions)) = build_state(true).await else {
        eprintln!("STORAGE_TEST_DATABASE_URL 未設定のためスキップ");
        return;
    };
    let (cookie, csrf) = login(&sessions, "sid-custom-instr").await;
    let app = build_router(state);
    let put = |body: serde_json::Value, with_csrf: bool| {
        let mut req = Request::builder()
            .method("PUT")
            .uri("/me/custom-instructions")
            .header(COOKIE, &cookie)
            .header("content-type", "application/json");
        if with_csrf {
            req = req.header("x-csrf-token", csrf);
        }
        req.body(Body::from(body.to_string())).unwrap()
    };
    let get = || {
        Request::builder()
            .uri("/me/custom-instructions")
            .header(COOKIE, &cookie)
            .body(Body::empty())
            .unwrap()
    };

    // 未認証は 401。
    let resp = app
        .clone()
        .oneshot(
            Request::builder()
                .uri("/me/custom-instructions")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::UNAUTHORIZED);

    // 保存（前後の空白を整えた値が返り、読み直しても同じ）。
    let resp = app
        .clone()
        .oneshot(put(
            serde_json::json!({ "about": "  法人営業  ", "style": "結論を先に", "tone": "formal" }),
            true,
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let saved = body_json(resp).await;
    assert_eq!(
        saved,
        serde_json::json!({ "about": "法人営業", "style": "結論を先に", "tone": "formal" })
    );
    let resp = app.clone().oneshot(get()).await.unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    assert_eq!(body_json(resp).await, saved);

    // CSRF 無しの書き込みは通らない（値も変わらない）。
    let resp = app
        .clone()
        .oneshot(put(serde_json::json!({ "about": "上書き" }), false))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::FORBIDDEN);

    // 上限超過は 400 で、画面へ出す文言を `detail` に載せる（値は変わらない）。
    let resp = app
        .clone()
        .oneshot(put(
            serde_json::json!({ "about": "あ".repeat(1501), "tone": "default" }),
            true,
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
    let detail = body_json(resp).await["detail"]
        .as_str()
        .unwrap_or_default()
        .to_string();
    assert!(detail.contains("1500 文字以内"), "{detail}");
    // 未知の口調は受け付けない。
    let resp = app
        .clone()
        .oneshot(put(serde_json::json!({ "tone": "loud" }), true))
        .await
        .unwrap();
    assert!(resp.status().is_client_error(), "{}", resp.status());
    let resp = app.clone().oneshot(get()).await.unwrap();
    assert_eq!(body_json(resp).await, saved);

    // 全部空にすると未設定に戻る（欠けたフィールドは空の既定値）。
    let resp = app
        .clone()
        .oneshot(put(serde_json::json!({}), true))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let empty = serde_json::json!({ "about": "", "style": "", "tone": "default" });
    assert_eq!(body_json(resp).await, empty);
    let resp = app.oneshot(get()).await.unwrap();
    assert_eq!(body_json(resp).await, empty);

    // chat 無効なら 503。
    let Some((state, sessions)) = build_state(false).await else {
        return;
    };
    let (cookie, _) = login(&sessions, "sid-custom-instr-off").await;
    let resp = build_router(state)
        .oneshot(
            Request::builder()
                .uri("/me/custom-instructions")
                .header(COOKIE, &cookie)
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::SERVICE_UNAVAILABLE);
}

/// ユーザー設定（#952）: 本人の分の読み書き・欠けた項目は既定値・不正なモデル ID の 400（画面へ出す
/// `detail` 付き）・未知の値の拒否・未認証 401・CSRF 無しの書き込み拒否・chat 無効の 503。
///
/// このファイルのテストは同じ利用者（tenant `default`）を共有して並列に走るので、スレッド作成に効く
/// 既定の承認モードと org の全自動キャップには触れない（そちらは `chat` の preferences_it が
/// テナントを分けて確かめる）。最後に既定値へ戻す。
#[tokio::test]
async fn preferences_round_trip_over_http() {
    let Some((state, sessions)) = build_state(true).await else {
        eprintln!("STORAGE_TEST_DATABASE_URL 未設定のためスキップ");
        return;
    };
    let (cookie, csrf) = login(&sessions, "sid-preferences").await;
    let app = build_router(state);
    let put = |body: serde_json::Value, with_csrf: bool| {
        let mut req = Request::builder()
            .method("PUT")
            .uri("/me/preferences")
            .header(COOKIE, &cookie)
            .header("content-type", "application/json");
        if with_csrf {
            req = req.header("x-csrf-token", csrf);
        }
        req.body(Body::from(body.to_string())).unwrap()
    };
    let get = || {
        Request::builder()
            .uri("/me/preferences")
            .header(COOKIE, &cookie)
            .body(Body::empty())
            .unwrap()
    };

    // 未認証は 401。
    let resp = app
        .clone()
        .oneshot(
            Request::builder()
                .uri("/me/preferences")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::UNAUTHORIZED);

    // 保存（欠けた項目は既定値で埋まり、モデル ID は前後の空白を落とす。読み直しても同じ）。
    let resp = app
        .clone()
        .oneshot(put(
            serde_json::json!({
                "submit_key": "mod_enter",
                "default_model": { "model": " claude-sonnet ", "effort": "high" },
                "expand_reasoning": true,
                "time_format": "relative",
            }),
            true,
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let saved = body_json(resp).await;
    assert_eq!(
        saved["preferences"],
        serde_json::json!({
            "submit_key": "mod_enter",
            "default_model": { "model": "claude-sonnet", "effort": "high" },
            "web_search_by_default": false,
            "default_autonomous_mode": "require_approval",
            "expand_reasoning": true,
            "time_format": "relative",
        })
    );
    assert_eq!(saved["bypass_allowed"], true);
    let resp = app.clone().oneshot(get()).await.unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    assert_eq!(body_json(resp).await, saved);

    // CSRF 無しの書き込みは通らない。
    let resp = app
        .clone()
        .oneshot(put(serde_json::json!({}), false))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::FORBIDDEN);
    // 未知の値は受け付けない。
    let resp = app
        .clone()
        .oneshot(put(serde_json::json!({ "submit_key": "space" }), true))
        .await
        .unwrap();
    assert!(resp.status().is_client_error(), "{}", resp.status());
    // 長すぎるモデル ID は 400 で、画面へ出す文言を `detail` に載せる。
    let resp = app
        .clone()
        .oneshot(put(
            serde_json::json!({ "default_model": { "model": "m".repeat(201), "effort": "low" } }),
            true,
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
    let detail = body_json(resp).await["detail"]
        .as_str()
        .unwrap_or_default()
        .to_string();
    assert!(detail.contains("200 文字以内"), "{detail}");
    // 拒否した書き込みで値は変わらない。
    let resp = app.clone().oneshot(get()).await.unwrap();
    assert_eq!(body_json(resp).await, saved);

    // 既定値へ戻す（欠けた項目はすべて既定値）。
    let resp = app
        .clone()
        .oneshot(put(serde_json::json!({}), true))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    assert_eq!(
        body_json(resp).await["preferences"],
        serde_json::json!({
            "submit_key": "enter",
            "default_model": null,
            "web_search_by_default": false,
            "default_autonomous_mode": "require_approval",
            "expand_reasoning": false,
            "time_format": "absolute",
        })
    );

    // chat 無効なら 503。
    let Some((state, sessions)) = build_state(false).await else {
        return;
    };
    let (cookie, _) = login(&sessions, "sid-preferences-off").await;
    let resp = build_router(state)
        .oneshot(
            Request::builder()
                .uri("/me/preferences")
                .header(COOKIE, &cookie)
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::SERVICE_UNAVAILABLE);
}

/// #965: ホームの「今日のこと」の `/me/approvals` と `/me/usage`。
/// 承認待ちは自分が actor の run が載り、未認証は 401。利用量は LLM 未構成なら 503、構成済みなら
/// 本人の当月の利用額と上限（上限なしは null）を返す。
#[tokio::test]
async fn today_endpoints_over_http() {
    let Some((mut state, sessions)) = build_state(true).await else {
        eprintln!("STORAGE_TEST_DATABASE_URL 未設定のためスキップ");
        return;
    };
    let db_url = std::env::var("STORAGE_TEST_DATABASE_URL").unwrap();
    let pool = PgPoolOptions::new().connect(&db_url).await.unwrap();
    let (cookie, csrf) = login(&sessions, "sid-today").await;
    let get = |uri: &str| {
        Request::builder()
            .uri(uri)
            .header(COOKIE, &cookie)
            .body(Body::empty())
            .unwrap()
    };

    // LLM 未構成の利用量は 503（承認待ちは chat だけで答えられる）。
    let app = build_router(state.clone());
    let resp = app.clone().oneshot(get("/me/usage")).await.unwrap();
    assert_eq!(resp.status(), StatusCode::SERVICE_UNAVAILABLE);
    let resp = app
        .clone()
        .oneshot(
            Request::builder()
                .uri("/me/approvals")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::UNAUTHORIZED);

    // スレッドを作って発話し、その run を承認待ちにする（ワーカーが書く状態を SQL で再現）。
    let resp = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/threads")
                .header(COOKIE, &cookie)
                .header("x-csrf-token", csrf)
                .header("content-type", "application/json")
                .body(Body::from(r#"{"title":"承認のテスト","agent_mode":true}"#))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let tid = body_json(resp).await["id"].as_str().unwrap().to_string();
    let resp = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri(format!("/threads/{tid}/messages"))
                .header(COOKIE, &cookie)
                .header("x-csrf-token", csrf)
                .header("content-type", "application/json")
                .body(Body::from(r#"{"text":"移動して"}"#))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::ACCEPTED);
    let run_id: uuid::Uuid = body_json(resp).await["run_id"]
        .as_str()
        .unwrap()
        .parse()
        .unwrap();
    sqlx::query(
        "UPDATE generation_run SET status = 'waiting_approval', \
             pending_approval = jsonb_build_object('tool_call_id', 'tc-home', \
                 'tool_name', 'drive.move', 'requested_at', now()) \
         WHERE run_id = $1",
    )
    .bind(run_id)
    .execute(&pool)
    .await
    .unwrap();

    let resp = app.clone().oneshot(get("/me/approvals")).await.unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let body = body_json(resp).await;
    let item = body["items"]
        .as_array()
        .unwrap()
        .iter()
        .find(|i| i["run_id"] == serde_json::json!(run_id.to_string()))
        .unwrap_or_else(|| panic!("承認待ちの run が載る: {body}"));
    assert_eq!(item["thread_id"], serde_json::json!(tid));
    assert_eq!(item["thread_title"], serde_json::json!("承認のテスト"));
    assert_eq!(item["tool_name"], serde_json::json!("drive.move"));
    assert!(item["requested_at"].is_string());
    // 後続のテストに承認待ちを残さない。
    sqlx::query(
        "UPDATE generation_run SET status = 'cancelled', pending_approval = NULL WHERE run_id = $1",
    )
    .bind(run_id)
    .execute(&pool)
    .await
    .unwrap();

    let gw = llm_gateway::LlmGateway::build(
        pool.clone(),
        reqwest::Client::new(),
        llm_gateway::GatewayConfig::default(),
    )
    .unwrap();
    state.llm = Some(Arc::new(gw));
    let app = build_router(state);
    let resp = app.clone().oneshot(get("/me/usage")).await.unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let body = body_json(resp).await;
    assert!(
        body["used_usd_micros"].is_null(),
        "上限が無い環境では集計しない: {body}"
    );
    assert!(
        body["limit_usd_micros"].is_null(),
        "上限が無い環境では null: {body}"
    );
    assert!(body["period_start"].is_string() && body["period_end"].is_string());
}

/// #965: 検索フォルダ付きでスレッドを作る。作成者が見られるフォルダなら名前ごと保存して返し、
/// 存在しない id は 404（孤児スレッドを作らない）。
#[tokio::test]
async fn create_thread_with_search_folder() {
    let Some((state, sessions)) = build_state(true).await else {
        eprintln!("STORAGE_TEST_DATABASE_URL 未設定のためスキップ");
        return;
    };
    let (cookie, csrf) = login(&sessions, "sid-search-folder").await;
    let app = build_router(state);
    let post = |uri: &str, body: serde_json::Value| {
        Request::builder()
            .method("POST")
            .uri(uri)
            .header(COOKIE, &cookie)
            .header("x-csrf-token", csrf)
            .header("content-type", "application/json")
            .body(Body::from(body.to_string()))
            .unwrap()
    };

    let folder_name = format!("search-{}", uuid::Uuid::new_v4().simple());
    let resp = app
        .clone()
        .oneshot(post("/folders", serde_json::json!({ "name": folder_name })))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let fid = body_json(resp).await["id"].as_str().unwrap().to_string();

    let resp = app
        .clone()
        .oneshot(post(
            "/threads",
            serde_json::json!({ "title": "フォルダで探す", "search_folder_id": fid }),
        ))
        .await
        .unwrap();
    let status = resp.status();
    let body = body_json(resp).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["search_folder_id"], serde_json::json!(fid));
    assert_eq!(body["search_folder_name"], serde_json::json!(folder_name));
    let tid = body["id"].as_str().unwrap().to_string();

    // 読み直しても残る。
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
    assert_eq!(
        body_json(resp).await["search_folder_name"],
        serde_json::json!(folder_name)
    );

    // 入れ子のフォルダも選べる（パンくずの末尾が自身）。
    let resp = app
        .clone()
        .oneshot(post(
            "/folders",
            serde_json::json!({ "name": "下の階層", "parent_id": fid }),
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let child = body_json(resp).await["id"].as_str().unwrap().to_string();
    let resp = app
        .clone()
        .oneshot(post(
            "/threads",
            serde_json::json!({ "title": "下の階層で探す", "search_folder_id": child }),
        ))
        .await
        .unwrap();
    let status = resp.status();
    let body = body_json(resp).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["search_folder_name"], serde_json::json!("下の階層"));

    // 存在しないフォルダは 404。
    let resp = app
        .clone()
        .oneshot(post(
            "/threads",
            serde_json::json!({ "title": "x", "search_folder_id": uuid::Uuid::new_v4() }),
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::NOT_FOUND);
}
