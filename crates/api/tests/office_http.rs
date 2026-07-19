//! /office/sessions と WOPI マウントの HTTP 統合テスト（Task 11.6）。
//!
//! `STORAGE_TEST_DATABASE_URL` がある時のみ実行。検証:
//! - セッション必須（401）・office 無効時はルート不在（404）
//! - 正常系: viewer check → 編集 URL 解決 → トークン発行、そのトークンで
//!   （Session ミドルウェアを通らない）/wopi/files/{id} が同一アプリルータから叩ける
//! - 未対応拡張子・存在しないファイルは 404（存在秘匿）

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
    AuthContext, AuthzClient, AuthzError, Consistency, FgaObject, ObjectType, Principal,
    ReadTupleKey, Relation, Subject,
};
use axum::{
    body::Body,
    http::{header::COOKIE, Request, StatusCode},
};
use http_body_util::BodyExt;
use sqlx::postgres::PgPoolOptions;
use tower::ServiceExt;
use uuid::Uuid;

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

/// discovery を叩かない固定 suite（docx のみ対応）。
struct FixedSuite;

#[async_trait]
impl office::OfficeSuite for FixedSuite {
    fn name(&self) -> &'static str {
        "fixed"
    }
    async fn editor_action_url(&self, ext: &str) -> Result<Option<String>, office::OfficeError> {
        Ok((ext == "docx").then(|| "http://collabora.test/browser/x/cool.html?".to_string()))
    }
    fn supported_extensions(&self) -> &[&'static str] {
        &["docx"]
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
            backend: ObjectStoreBackend::Minio,
            s3: None,
            max_upload_size_bytes: 5 * 1024 * 1024 * 1024,
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
            langfuse: None,
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
        roles: vec![],
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

/// 実 DB の AppState を組む（office 有効／無効を選べる）。DB 未設定なら None。
async fn build_state(with_office: bool) -> Option<(AppState, Arc<dyn SessionStore>)> {
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
    let tabular_svc = Arc::new(tabular::TabularService::new(
        Arc::clone(&storage),
        tabular::RunnerConfig::new("shiki-tabular-runner", Duration::from_secs(5)),
        tabular::Quotas::default(),
    ));
    let office_runtime = with_office.then(|| api::state::OfficeRuntime {
        suite: Arc::new(FixedSuite),
        wopi_base_url: "http://shiki-server:8080".into(),
        wopi: office::WopiState {
            storage: Arc::clone(&storage),
            authz: Arc::new(AllowAll),
            pool: pool.clone(),
            token_key: office::OfficeTokenKey::random(),
            web_origin: Some("http://localhost:3000".into()),
            max_body_bytes: 64 * 1024 * 1024,
        },
    });
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
        chat: None,
        rag_admin,
        office: office_runtime,
        docx_composer: Arc::new(office::DocxComposer::new(
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

/// セッション主体と同一の AuthContext（テスト用のファイル作成に使う）。
fn test_ctx() -> AuthContext {
    AuthContext::new(test_principal(), "acme".into(), "default".into())
}

fn session_post(uri: &str, cookie: &str, csrf: &str, body: serde_json::Value) -> Request<Body> {
    Request::builder()
        .method("POST")
        .uri(uri)
        .header(COOKIE, cookie)
        .header("x-csrf-token", csrf)
        .header("content-type", "application/json")
        .body(Body::from(body.to_string()))
        .unwrap()
}

#[tokio::test]
async fn office_session_issues_token_and_wopi_is_mounted() {
    let Some((state, sessions)) = build_state(true).await else {
        eprintln!("STORAGE_TEST_DATABASE_URL 未設定のためスキップ");
        return;
    };
    let storage = Arc::clone(&state.storage);
    let app = build_router(state);
    let (cookie, csrf) = login(&sessions, "sid-office-1").await;

    // 未認証は 401（Session ポリシー）。
    let res = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/office/sessions")
                .header("content-type", "application/json")
                .body(Body::from(
                    serde_json::json!({"file_id": Uuid::new_v4()}).to_string(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::UNAUTHORIZED);

    // docx ファイルを作成 → セッション発行が成功する。
    let ctx = test_ctx();
    let name = format!("office-{}.docx", Uuid::new_v4());
    let node = storage
        .write_file_internal(
            &ctx,
            None,
            &name,
            format!("docx-bytes:{}", Uuid::new_v4()).as_bytes(),
            "application/vnd.openxmlformats-officedocument.wordprocessingml.document",
            None,
        )
        .await
        .expect("create docx");
    let res = app
        .clone()
        .oneshot(session_post(
            "/office/sessions",
            &cookie,
            csrf,
            serde_json::json!({"file_id": node.id}),
        ))
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::OK);
    let session = body_json(res).await;
    assert_eq!(
        session["action_url"],
        "http://collabora.test/browser/x/cool.html?"
    );
    // WOPISrc は「Collabora から見た shiki-server の URL」。web が iframe URL に付ける。
    assert_eq!(
        session["wopi_src"],
        format!("http://shiki-server:8080/wopi/files/{}", node.id)
    );
    assert!(session["access_token_ttl_ms"].as_u64().unwrap() > 0);
    let token = session["access_token"].as_str().unwrap().to_string();

    // 発行されたトークンで（cookie 無しで）WOPI CheckFileInfo が同一ルータから叩ける
    // ＝WOPI は Session ミドルウェアを通らない別認証面としてマウントされている。
    let res = app
        .clone()
        .oneshot(
            Request::builder()
                .method("GET")
                .uri(format!("/wopi/files/{}?access_token={token}", node.id))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::OK);
    let info = body_json(res).await;
    assert_eq!(info["BaseFileName"], name.as_str());
    assert_eq!(info["UserCanWrite"], true);

    // でたらめなトークンは 401（cookie セッションが有効でも WOPI では使えない）。
    let res = app
        .clone()
        .oneshot(
            Request::builder()
                .method("GET")
                .uri(format!("/wopi/files/{}?access_token=bad.token", node.id))
                .header(COOKIE, &cookie)
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::UNAUTHORIZED);
}

#[tokio::test]
async fn office_session_rejects_unsupported_and_missing_files() {
    let Some((state, sessions)) = build_state(true).await else {
        eprintln!("STORAGE_TEST_DATABASE_URL 未設定のためスキップ");
        return;
    };
    let storage = Arc::clone(&state.storage);
    let app = build_router(state);
    let (cookie, csrf) = login(&sessions, "sid-office-2").await;

    // 存在しないファイルは 404（存在秘匿）。
    let res = app
        .clone()
        .oneshot(session_post(
            "/office/sessions",
            &cookie,
            csrf,
            serde_json::json!({"file_id": Uuid::new_v4()}),
        ))
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::NOT_FOUND);

    // 未対応拡張子（.txt）は 404（編集面が存在しない）。
    let ctx = test_ctx();
    let node = storage
        .write_file_internal(
            &ctx,
            None,
            &format!("plain-{}.txt", Uuid::new_v4()),
            format!("text:{}", Uuid::new_v4()).as_bytes(),
            "text/plain",
            None,
        )
        .await
        .expect("create txt");
    let res = app
        .clone()
        .oneshot(session_post(
            "/office/sessions",
            &cookie,
            csrf,
            serde_json::json!({"file_id": node.id}),
        ))
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::NOT_FOUND);
}

/// #332/#334: /documents（作成）と /documents/export（変換 DL）は office フラグ非依存で
/// 常時配線される。空 markdown は ingestion-worker を呼ばず blank.docx テンプレを返すため、
/// worker 未到達（127.0.0.1:1）でも作成/エクスポートが成立することを検証する。
#[tokio::test]
async fn documents_create_and_export_work_without_office_or_worker() {
    let Some((state, sessions)) = build_state(false).await else {
        eprintln!("STORAGE_TEST_DATABASE_URL 未設定のためスキップ");
        return;
    };
    let app = build_router(state);
    let (cookie, csrf) = login(&sessions, "sid-doc-1").await;

    // 未認証は 401（Session ポリシー）。
    let res = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/documents")
                .header("content-type", "application/json")
                .body(Body::from(serde_json::json!({"name": "x"}).to_string()))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::UNAUTHORIZED);

    // 作成: 空本文 → .docx ノードが作られる（worker 非依存）。
    let res = app
        .clone()
        .oneshot(session_post(
            "/documents",
            &cookie,
            csrf,
            serde_json::json!({"name": "議事録", "parent_id": null}),
        ))
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::OK);
    let node = body_json(res).await;
    assert_eq!(node["name"], "議事録.docx");
    assert_eq!(
        node["content_type"],
        "application/vnd.openxmlformats-officedocument.wordprocessingml.document"
    );

    // エクスポート: 空 markdown → .docx バイナリを attachment で返す（worker 非依存）。
    let res = app
        .clone()
        .oneshot(session_post(
            "/documents/export",
            &cookie,
            csrf,
            serde_json::json!({"name": "レポート", "markdown": ""}),
        ))
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::OK);
    let cd = res
        .headers()
        .get("content-disposition")
        .and_then(|v| v.to_str().ok())
        .unwrap_or_default()
        .to_string();
    assert!(cd.contains("attachment"), "attachment 指定: {cd}");
    assert!(cd.contains("filename*=UTF-8''"), "RFC 5987 名: {cd}");
    let bytes = res.into_body().collect().await.unwrap().to_bytes();
    assert_eq!(&bytes[..2], b"PK", "docx（zip）ヘッダ");
}

/// エクスポートは名前空欄を 400 で弾く（変換前の入力検証）。
#[tokio::test]
async fn documents_export_rejects_empty_name() {
    let Some((state, sessions)) = build_state(false).await else {
        eprintln!("STORAGE_TEST_DATABASE_URL 未設定のためスキップ");
        return;
    };
    let app = build_router(state);
    let (cookie, csrf) = login(&sessions, "sid-doc-2").await;
    let res = app
        .clone()
        .oneshot(session_post(
            "/documents/export",
            &cookie,
            csrf,
            serde_json::json!({"name": "  ", "markdown": "本文"}),
        ))
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::BAD_REQUEST);
}

#[tokio::test]
async fn office_disabled_routes_are_absent() {
    let Some((state, sessions)) = build_state(false).await else {
        eprintln!("STORAGE_TEST_DATABASE_URL 未設定のためスキップ");
        return;
    };
    let app = build_router(state);
    let (cookie, csrf) = login(&sessions, "sid-office-3").await;

    // office 無効: /office/sessions は配線されず 404（セッションが有効でも）。
    let res = app
        .clone()
        .oneshot(session_post(
            "/office/sessions",
            &cookie,
            csrf,
            serde_json::json!({"file_id": Uuid::new_v4()}),
        ))
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::NOT_FOUND);

    // /wopi もマウントされない。
    let res = app
        .clone()
        .oneshot(
            Request::builder()
                .method("GET")
                .uri(format!("/wopi/files/{}?access_token=x.y", Uuid::new_v4()))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::NOT_FOUND);
}
