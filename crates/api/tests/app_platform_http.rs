//! ミニアプリ／レジストリ API ハンドラの HTTP 統合テスト（Task 9.1 / 9.13a）。
//!
//! セッション Cookie 認証 → マニフェスト create/get/update/publish を実 Postgres で通し、
//! ルート宣言・抽出・ステータスコード・語彙照合 400 を検証する（`STORAGE_TEST_DATABASE_URL`）。

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::print_stdout,
    clippy::print_stderr,
    clippy::pedantic,
    clippy::cognitive_complexity
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
use sqlx::PgPool;
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
        _o: ObjectType,
    ) -> Result<Vec<String>, AuthzError> {
        Ok(vec![])
    }
    async fn delete_object_tuples(&self, _o: &FgaObject) -> Result<u32, AuthzError> {
        Ok(0)
    }
    async fn read_subject_objects(
        &self,
        _s: &Subject,
        _o: ObjectType,
    ) -> Result<Vec<String>, AuthzError> {
        Ok(vec![])
    }
}

/// check だけが失敗する認可（OpenFGA の一時的な障害・BatchCheck も check を通る）。
struct CheckFails;

#[async_trait]
impl AuthzClient for CheckFails {
    async fn check(
        &self,
        _s: &Subject,
        _r: Relation,
        _o: &FgaObject,
        _c: Consistency,
    ) -> Result<bool, AuthzError> {
        Err(AuthzError::Unexpected {
            status: 503,
            body: "unavailable".into(),
        })
    }
    async fn write_tuple(
        &self,
        s: &Subject,
        r: Relation,
        o: &FgaObject,
    ) -> Result<bool, AuthzError> {
        AllowAll.write_tuple(s, r, o).await
    }
    async fn delete_tuple(
        &self,
        s: &Subject,
        r: Relation,
        o: &FgaObject,
    ) -> Result<bool, AuthzError> {
        AllowAll.delete_tuple(s, r, o).await
    }
    async fn read_tuples(
        &self,
        o: &FgaObject,
        r: Option<Relation>,
    ) -> Result<Vec<ReadTupleKey>, AuthzError> {
        AllowAll.read_tuples(o, r).await
    }
    async fn list_objects(
        &self,
        s: &Subject,
        r: Relation,
        o: ObjectType,
    ) -> Result<Vec<String>, AuthzError> {
        AllowAll.list_objects(s, r, o).await
    }
    async fn delete_object_tuples(&self, o: &FgaObject) -> Result<u32, AuthzError> {
        AllowAll.delete_object_tuples(o).await
    }
    async fn read_subject_objects(
        &self,
        s: &Subject,
        o: ObjectType,
    ) -> Result<Vec<String>, AuthzError> {
        AllowAll.read_subject_objects(s, o).await
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
        _k: &str,
        _t: Duration,
    ) -> Result<String, storage::ObjectStoreError> {
        Ok("http://fake/internal".into())
    }
    async fn presign_put(
        &self,
        _k: &str,
        _t: Duration,
        _l: i64,
    ) -> Result<String, storage::ObjectStoreError> {
        Ok("http://test/put".into())
    }
    async fn presign_get(
        &self,
        _k: &str,
        _t: Duration,
        _f: Option<&str>,
        _c: Option<&str>,
    ) -> Result<String, storage::ObjectStoreError> {
        Ok("http://test/get".into())
    }
    async fn read_and_hash(&self, _k: &str) -> Result<(String, u64), storage::ObjectStoreError> {
        Err(storage::ObjectStoreError::NotFound("test".into()))
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
        Err(storage::ObjectStoreError::NotFound("test".into()))
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

async fn setup() -> Option<PgPool> {
    let url = shiki_testenv::var("STORAGE_TEST_DATABASE_URL").ok()?;
    let pool = PgPoolOptions::new()
        .max_connections(5)
        .connect(&url)
        .await
        .expect("pg");
    sqlx::migrate!("../../migrations")
        .run(&pool)
        .await
        .expect("migrate");
    Some(pool)
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
            backend: LlmBackend::Vllm,
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

fn state_with(pool: PgPool, sessions: Arc<dyn SessionStore>) -> AppState {
    // config.database.url は接続に使わない（実 pool を直接注入する）ためプレースホルダ。
    let config = base_config("postgres://localhost/none");
    let db = pool;
    let jwks = Arc::new(api::middleware::JwksCache::new(
        reqwest::Client::new(),
        config.auth.effective_jwks_uri(),
        Duration::from_secs(300),
    ));
    let storage = Arc::new(storage::StorageService::new(
        db.clone(),
        Arc::new(FakeStore),
        Arc::new(AllowAll),
        Duration::from_secs(120),
        Duration::from_secs(900),
        5 * 1024 * 1024 * 1024,
    ));
    let directory = Arc::new(storage::DirectoryStore::new(db.clone()));
    let tenants = Arc::new(storage::TenantStore::new(db.clone()));
    let rag_admin = Arc::new(rag::RagAdmin::new(db.clone(), None, None));
    let artifacts = Arc::new(artifact::ArtifactStore::new(db.clone(), Arc::new(AllowAll)));
    let workflows = Arc::new(workflow_engine::WorkflowStore::new(Arc::clone(&artifacts)));
    let ui_actions = Arc::new(gui::ActionDispatcher::new(
        storage::audit::AuditRecorder::new(db.clone()),
    ));
    let skills = Arc::new(gui::SkillStore::new(Arc::clone(&artifacts)));
    let mini_apps = Arc::new(gui::MiniAppStore::new(Arc::clone(&artifacts), db.clone()));
    let data_store = Arc::new(data::DataStore::new(
        db.clone(),
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
        app_platform::Registry::new(db.clone()),
    ));
    let installs = Arc::new(app_platform::InstallService::new(
        db.clone(),
        app_platform::Registry::new(db.clone()),
        Arc::clone(&mini_app_code),
        Arc::clone(&data_store),
        Arc::new(AllowAll),
        None,
        vec![],
    ));
    let skill_installs = Arc::new(app_platform::SkillInstallService::new(
        db.clone(),
        app_platform::Registry::new(db.clone()),
        app_platform::TrustedKeyStore::new(db.clone()),
        Arc::clone(&artifacts),
        Arc::new(AllowAll),
    ));
    let bundles = Arc::new(app_platform::BundleStore::new(
        Arc::new(FakeStore),
        Arc::new(AllowAll),
        storage::audit::AuditRecorder::new(db.clone()),
    ));
    let app_usage = Arc::new(app_platform::AppUsageStore::new(
        db.clone(),
        Arc::new(AllowAll),
    ));
    let workflow_registration = Arc::new(workflow_engine::RegistrationService::new(
        db.clone(),
        workflow_engine::DelegationStore::new(db.clone(), Arc::new(AllowAll)),
    ));
    let audit = Arc::new(storage::audit::AuditRecorder::new(db.clone()));
    let workflow_summaries = Arc::new(workflow_engine::WorkflowSummaryStore::new(db.clone()));
    let workflow_layout = Arc::new(workflow_engine::EditorLayoutStore::new(db.clone()));
    let workflow_approvals = Arc::new(workflow_engine::ApprovalStore::new(db.clone()));
    let collab_hub = Arc::new(collab::CollabHub::new(
        db.clone(),
        Arc::new(AllowAll),
        Arc::clone(&storage),
    ));
    let tabular_svc = std::sync::Arc::new(tabular::TabularService::new(
        std::sync::Arc::clone(&storage),
        tabular::RunnerConfig::new("shiki-tabular-runner", std::time::Duration::from_secs(5)),
        tabular::Quotas::default(),
    ));
    AppState {
        config: Arc::new(config),
        insights: std::sync::Arc::new(api::insights::InsightsService::new(db.clone())),
        db: api::state::ReadinessProbe::new(db),
        authz: Arc::new(AllowAll),
        jwks,
        sessions,
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
        workflow_runs: None,
        workflow_registration,
        workflow_summaries,
        workflow_layout,
        workflow_approvals,
        audit,
        directory,
        tenants,
        search: None,
        chat: None,
        llm: None,
        rag_admin,
        office: None,
        docx_composer: std::sync::Arc::new(office::DocxComposer::new(
            reqwest::Client::new(),
            "http://127.0.0.1:1",
        )),
    }
}

fn session_record(csrf: &str) -> SessionRecord {
    SessionRecord {
        principal: Principal {
            kind: authz::PrincipalKind::User,
            id: "00000000-0000-0000-0000-000000000001".into(),
            email: Some("alice@acme.example".into()),
            groups: vec!["/acme".into()],
            roles: vec!["engineering".into()],
            tenant_id: None,
        },
        tenant_id: "default".into(),
        access_token: "access".into(),
        refresh_token: None,
        id_token: None,
        access_expires_at: chrono::Utc::now().timestamp() + 3600,
        csrf_token: csrf.into(),
        keycloak_sid: None,
    }
}

fn manifest_json(name: &str, version: &str) -> serde_json::Value {
    serde_json::json!({
        "name": name,
        "version": version,
        "description": "経費申請",
        "requested_scopes": ["data.read", "data.write"],
        "tools": ["doc_search"],
        "tables": [],
        "workflows": [],
        "budget": {},
        "frontend": null,
        "server": null,
        "trust_tier": "in_house",
    })
}

/// create → get → update → publish の一気通貫と語彙照合 400 を HTTP 経由で検証する。
#[tokio::test]
async fn manifest_crud_and_publish_over_http() {
    let Some(pool) = setup().await else { return };
    let sessions = Arc::new(MemorySessionStore::new());
    sessions
        .put(
            "default",
            "sid-app",
            &session_record("csrf"),
            Duration::from_secs(3600),
        )
        .await
        .unwrap();
    let app = build_router(state_with(pool, sessions));
    let cookie = "shiki_session=sid-app.default; shiki_csrf=csrf";

    // create（POST は CSRF 二重送信が要る）。
    let resp = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/apps/manifests")
                .header(COOKIE, cookie)
                .header("x-csrf-token", "csrf")
                .header("content-type", "application/json")
                .body(Body::from(
                    serde_json::to_vec(&serde_json::json!({
                        "manifest": manifest_json("http-expense", "1.0.0"),
                    }))
                    .unwrap(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();
    let status = resp.status();
    let bytes = resp.into_body().collect().await.unwrap().to_bytes();
    assert_eq!(
        status,
        StatusCode::CREATED,
        "{}",
        String::from_utf8_lossy(&bytes)
    );
    let body: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
    let id = body["id"].as_str().unwrap().to_string();
    assert_eq!(body["version"], 1);

    // get（バージョン省略＝最新）。
    let resp = app
        .clone()
        .oneshot(
            Request::builder()
                .uri(format!("/apps/manifests/{id}"))
                .header(COOKIE, cookie)
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);

    // update（新バージョン追記）。
    let resp = app
        .clone()
        .oneshot(
            Request::builder()
                .method("PUT")
                .uri(format!("/apps/manifests/{id}"))
                .header(COOKIE, cookie)
                .header("x-csrf-token", "csrf")
                .header("content-type", "application/json")
                .body(Body::from(
                    serde_json::to_vec(&serde_json::json!({
                        "manifest": manifest_json("http-expense", "1.1.0"),
                        "expected_version": 1,
                    }))
                    .unwrap(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let body: serde_json::Value =
        serde_json::from_slice(&resp.into_body().collect().await.unwrap().to_bytes()).unwrap();
    assert_eq!(body["version"], 2);

    // publish（不変レジストリ登録・201）。
    let resp = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri(format!("/apps/manifests/{id}/publish"))
                .header(COOKIE, cookie)
                .header("x-csrf-token", "csrf")
                .header("content-type", "application/json")
                .body(Body::from(
                    serde_json::to_vec(&serde_json::json!({ "artifact_version": 1 })).unwrap(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::CREATED);

    // 語彙照合: 未知スコープは 400。
    let resp = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/apps/manifests")
                .header(COOKIE, cookie)
                .header("x-csrf-token", "csrf")
                .header("content-type", "application/json")
                .body(Body::from(
                    serde_json::to_vec(&serde_json::json!({
                        "manifest": {
                            "name": "bad", "version": "1.0.0", "description": "",
                            "requested_scopes": ["storage.delete"], "tools": [],
                            "tables": [], "workflows": [], "budget": {},
                            "frontend": null, "server": null, "trust_tier": "in_house",
                        },
                    }))
                    .unwrap(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
}

/// publish → 同意インストール → 一覧 → import → アンインストールの HTTP 一気通貫（Task 9.13b）。
#[tokio::test]
async fn install_import_uninstall_over_http() {
    let Some(pool) = setup().await else { return };
    let sessions = Arc::new(MemorySessionStore::new());
    sessions
        .put(
            "default",
            "sid-inst",
            &session_record("csrf"),
            Duration::from_secs(3600),
        )
        .await
        .unwrap();
    let state = state_with(pool.clone(), sessions);
    let app = build_router(state.clone());
    // 自動実行の失敗の持ち主の確かめ（OpenFGA）だけが失敗する同じサーバー。
    let fga_down = {
        let mut s = state.clone();
        s.installs = Arc::new(app_platform::InstallService::new(
            pool.clone(),
            app_platform::Registry::new(pool.clone()),
            Arc::clone(&s.mini_app_code),
            Arc::clone(&s.data),
            Arc::new(CheckFails),
            None,
            vec![],
        ));
        build_router(s)
    };
    let cookie = "shiki_session=sid-inst.default; shiki_csrf=csrf";
    let post = |uri: String, body: serde_json::Value| {
        Request::builder()
            .method("POST")
            .uri(uri)
            .header(COOKIE, cookie)
            .header("x-csrf-token", "csrf")
            .header("content-type", "application/json")
            .body(Body::from(serde_json::to_vec(&body).unwrap()))
            .unwrap()
    };

    // publish まで（テーブル 1 本つきマニフェスト）。
    let mut m = manifest_json("http-install", "1.0.0");
    m["tables"] = serde_json::json!([{
        "name": "http_install_t",
        "schema": { "fields": [ { "name": "title", "type": "text", "required": true } ] },
    }]);
    let resp = app
        .clone()
        .oneshot(post(
            "/apps/manifests".into(),
            serde_json::json!({ "manifest": m }),
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::CREATED);
    let body: serde_json::Value =
        serde_json::from_slice(&resp.into_body().collect().await.unwrap().to_bytes()).unwrap();
    let id = body["id"].as_str().unwrap().to_string();
    let resp = app
        .clone()
        .oneshot(post(
            format!("/apps/manifests/{id}/publish"),
            serde_json::json!({}),
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::CREATED);

    // granted ⊄ requested → 400。
    let resp = app
        .clone()
        .oneshot(post(
            "/apps/installations".into(),
            serde_json::json!({
                "name": "http-install", "version": "1.0.0",
                "granted_scopes": ["rag.query"],
            }),
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::BAD_REQUEST);

    // 同意インストール → 200＋テーブルプロビジョン。
    let resp = app
        .clone()
        .oneshot(post(
            "/apps/installations".into(),
            serde_json::json!({
                "name": "http-install", "version": "1.0.0",
                "granted_scopes": ["data.read", "data.write"],
            }),
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let body: serde_json::Value =
        serde_json::from_slice(&resp.into_body().collect().await.unwrap().to_bytes()).unwrap();
    assert_eq!(body["app_id"].as_str().unwrap(), id);
    assert_eq!(body["table_ids"].as_array().unwrap().len(), 1);

    // 一覧に載る。
    let resp = app
        .clone()
        .oneshot(
            Request::builder()
                .uri("/apps/installations")
                .header(COOKIE, cookie)
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let body: serde_json::Value =
        serde_json::from_slice(&resp.into_body().collect().await.unwrap().to_bytes()).unwrap();
    let listed = body["items"]
        .as_array()
        .unwrap()
        .iter()
        .find(|i| i["app_id"] == id.as_str())
        .expect("listed");
    // テーブルの数を添える（画面がテーブルを使える部署・グループを変える口を出すか決める・GAP-68）。
    assert_eq!(listed["table_count"], 1, "{listed}");
    // 自動実行の失敗が無いアプリには function_failures を付けない（GAP-60）。
    assert!(listed.get("function_failures").is_none(), "{listed}");
    assert_function_failures_are_listed(&app, &fga_down, cookie, &pool, &id).await;

    // アプリのテーブルを使える部署・グループ（GAP-68）: 読める・足せる。ディレクトリに居ない部署は 400。
    let roles = |method: &str, body: Option<serde_json::Value>| {
        Request::builder()
            .method(method)
            .uri(format!("/apps/installations/{id}/roles"))
            .header(COOKIE, cookie)
            .header("x-csrf-token", "csrf")
            .header("content-type", "application/json")
            .body(body.map_or_else(Body::empty, |b| Body::from(b.to_string())))
            .unwrap()
    };
    let resp = app.clone().oneshot(roles("GET", None)).await.unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let body: serde_json::Value =
        serde_json::from_slice(&resp.into_body().collect().await.unwrap().to_bytes()).unwrap();
    assert_eq!(body["tables"][0]["name"], "http_install_t", "{body}");
    assert!(body["roles"]["submitter_roles"].is_array(), "{body}");
    let resp = app
        .clone()
        .oneshot(roles(
            "PATCH",
            Some(serde_json::json!({ "add": { "submitter_roles": ["ghost-http"] } })),
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
    storage::DirectoryStore::new(pool.clone())
        .upsert_role("staff-http", "default", "acme", "社員")
        .await
        .unwrap();
    let resp = app
        .clone()
        .oneshot(roles(
            "PATCH",
            Some(serde_json::json!({ "add": { "submitter_roles": ["staff-http"] } })),
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let audited: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM audit_log WHERE tenant_id = 'default' AND object_id = $1 \
           AND action = 'app.table_roles.update' AND decision = 'allow'",
    )
    .bind(&id)
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!(audited, 1);
    sqlx::query(
        "DELETE FROM directory_role WHERE tenant_id = 'default' AND role_id = 'staff-http'",
    )
    .execute(&pool)
    .await
    .unwrap();

    // オフライン import: 信頼鍵を台帳へ直接登録 → 署名付きマニフェストを import。
    let secret = [11u8; 32];
    let public = {
        use ed25519_dalek::SigningKey;
        SigningKey::from_bytes(&secret).verifying_key().to_bytes()
    };
    let keys = app_platform::TrustedKeyStore::new(pool.clone());
    let kctx = authz::AuthContext::new(
        Principal {
            kind: authz::PrincipalKind::User,
            id: "provisioner".into(),
            email: None,
            groups: vec![],
            roles: vec![],
            tenant_id: Some("default".into()),
        },
        "acme".into(),
        authz::TenantId::parse("default").unwrap(),
    );
    keys.add(&kctx, "http-key", &public, None).await.unwrap();
    let import_manifest: app_platform::MiniAppManifest =
        serde_json::from_value(manifest_json("http-imported", "2.0.0")).unwrap();
    let sig = app_platform::sign_manifest(&import_manifest, &secret).unwrap();
    let resp = app
        .clone()
        .oneshot(post(
            "/apps/registry/import".into(),
            serde_json::json!({
                "manifest": manifest_json("http-imported", "2.0.0"),
                "signature_hex": hex::encode(&sig),
                "key_id": "http-key",
            }),
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    // 改竄署名は 403。
    let mut bad = sig.clone();
    bad[0] ^= 0xff;
    let resp = app
        .clone()
        .oneshot(post(
            "/apps/registry/import".into(),
            serde_json::json!({
                "manifest": manifest_json("http-imported-2", "2.0.0"),
                "signature_hex": hex::encode(&bad),
                "key_id": "http-key",
            }),
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::FORBIDDEN);

    // 同じテナントのほかの組織のインストールは、入れ直し・アンインストールとも、理由と直し方つきの 409（BUG-212）。
    let delete = || {
        Request::builder()
            .method("DELETE")
            .uri(format!("/apps/installations/{id}"))
            .header(COOKIE, cookie)
            .header("x-csrf-token", "csrf")
            .body(Body::empty())
            .unwrap()
    };
    let set_org = |org: &'static str| {
        let pool = pool.clone();
        let id = id.clone();
        async move {
            sqlx::query(
                "UPDATE app_installation SET org = $1 WHERE tenant_id = 'default' AND app_id = $2::uuid",
            )
            .bind(org)
            .bind(&id)
            .execute(&pool)
            .await
            .unwrap();
        }
    };
    set_org("beta").await;
    let reinstall = post(
        "/apps/installations".into(),
        serde_json::json!({
            "name": "http-install", "version": "1.0.0",
            "granted_scopes": ["data.read"],
        }),
    );
    for req in [reinstall, delete()] {
        let resp = app.clone().oneshot(req).await.unwrap();
        assert_eq!(resp.status(), StatusCode::CONFLICT);
        let body: serde_json::Value =
            serde_json::from_slice(&resp.into_body().collect().await.unwrap().to_bytes()).unwrap();
        assert_eq!(body["code"], "installed_in_other_org", "{body}");
        assert_eq!(
            body["message"],
            app_gateway::INSTALLED_IN_OTHER_ORG,
            "{body}"
        );
    }
    set_org("acme").await;

    // アンインストール → 204。
    let resp = app.clone().oneshot(delete()).await.unwrap();
    assert_eq!(resp.status(), StatusCode::NO_CONTENT);
}

/// 利用量 API（/apps/{id}/usage）の owner ゲートと応答形状（Task 9.15）。
#[tokio::test]
async fn app_usage_endpoint_requires_owner() {
    let Some(pool) = setup().await else { return };
    let sessions = Arc::new(MemorySessionStore::new());
    sessions
        .put(
            "default",
            "sid-usage",
            &session_record("csrf"),
            Duration::from_secs(3600),
        )
        .await
        .unwrap();
    let app = build_router(state_with(pool.clone(), sessions));
    let cookie = "shiki_session=sid-usage.default; shiki_csrf=csrf";

    // AllowAll authz なので owner チェックは通り、集計は空でも 200＋形状。
    let resp = app
        .clone()
        .oneshot(
            Request::builder()
                .uri(format!("/apps/{}/usage", uuid::Uuid::new_v4()))
                .header(COOKIE, cookie)
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let body: serde_json::Value =
        serde_json::from_slice(&resp.into_body().collect().await.unwrap().to_bytes()).unwrap();
    assert!(body["capabilities"].is_array());
    assert!(body["llm"].is_array());
}

/// skill の publish → 同意インストール → カタログ掲載 → V4 skill 照合（10.11/10.1b・#344）
/// を HTTP 経由で一気通貫に検証する。
#[tokio::test]
async fn skill_publish_install_and_v4_over_http() {
    let Some(pool) = setup().await else { return };
    let sessions = Arc::new(MemorySessionStore::new());
    sessions
        .put(
            "default",
            "sid-skill",
            &session_record("csrf"),
            Duration::from_secs(3600),
        )
        .await
        .unwrap();
    let app = build_router(state_with(pool, sessions));
    let cookie = "shiki_session=sid-skill.default; shiki_csrf=csrf";
    let post = |uri: String, body: serde_json::Value| {
        Request::builder()
            .method("POST")
            .uri(uri)
            .header(COOKIE, cookie)
            .header("x-csrf-token", "csrf")
            .header("content-type", "application/json")
            .body(Body::from(serde_json::to_vec(&body).unwrap()))
            .unwrap()
    };
    let suffix = uuid::Uuid::new_v4().simple().to_string();
    let name = format!("expense-{}", &suffix[..8]);

    // skill を作成（保存時検証つき）。
    let resp = app
        .clone()
        .oneshot(post(
            "/skills".into(),
            serde_json::json!({
                "name": name,
                "body": {
                    "description": "経費精算の規程に基づいて確認するスキル",
                    "instructions": "あなたは経費規程の専門家です。",
                }
            }),
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::CREATED);
    let body: serde_json::Value =
        serde_json::from_slice(&resp.into_body().collect().await.unwrap().to_bytes()).unwrap();
    let skill_id = body["id"].as_str().unwrap().to_string();

    // インストール前は V4 skill 照合で保存不可（10.1b 受け入れ条件③）。
    let ir = serde_json::json!({
        "ir_version": 1,
        "name": format!("wf-{}", &suffix[..8]),
        "nodes": [{ "id": "s", "type": "skill.invoke",
            "params": { "skill": format!("skill:{name}@1.0.0") } }],
        "edges": []
    });
    let resp = app
        .clone()
        .oneshot(post(
            "/workflows/validate".into(),
            serde_json::json!({ "ir": ir }),
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let body: serde_json::Value =
        serde_json::from_slice(&resp.into_body().collect().await.unwrap().to_bytes()).unwrap();
    assert!(
        body["errors"]
            .as_array()
            .unwrap()
            .iter()
            .any(|e| e["code"] == "ir.unknown_skill"),
        "未インストール skill 参照は拒否: {body}"
    );

    // publish（in-house）→ 本人インストール。
    let resp = app
        .clone()
        .oneshot(post(
            format!("/skills/{skill_id}/publish"),
            serde_json::json!({ "version": "1.0.0", "trust_tier": "in_house" }),
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    // 同一バージョンの再 publish は 409（不変）。
    let resp = app
        .clone()
        .oneshot(post(
            format!("/skills/{skill_id}/publish"),
            serde_json::json!({ "version": "1.0.0", "trust_tier": "in_house" }),
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::CONFLICT);

    let resp = app
        .clone()
        .oneshot(post(
            "/skills/installations".into(),
            serde_json::json!({ "name": name }),
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);

    // インストール済み一覧に載る。
    let resp = app
        .clone()
        .oneshot(
            Request::builder()
                .uri("/skills/installations")
                .header(COOKIE, cookie)
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let body: serde_json::Value =
        serde_json::from_slice(&resp.into_body().collect().await.unwrap().to_bytes()).unwrap();
    assert!(body["installations"]
        .as_array()
        .unwrap()
        .iter()
        .any(|i| i["name"] == name.as_str()));

    // インストール後は V4 skill 照合を通る。
    let resp = app
        .clone()
        .oneshot(post(
            "/workflows/validate".into(),
            serde_json::json!({ "ir": ir }),
        ))
        .await
        .unwrap();
    let body: serde_json::Value =
        serde_json::from_slice(&resp.into_body().collect().await.unwrap().to_bytes()).unwrap();
    assert!(
        body["errors"].as_array().unwrap().is_empty(),
        "インストール済みなら保存可能: {body}"
    );

    // アンインストール → 再び保存不可（fail-closed の一段目）。
    let resp = app
        .clone()
        .oneshot(
            Request::builder()
                .method("DELETE")
                .uri(format!("/skills/installations/{name}"))
                .header(COOKIE, cookie)
                .header("x-csrf-token", "csrf")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::NO_CONTENT);
    let resp = app
        .clone()
        .oneshot(post(
            "/workflows/validate".into(),
            serde_json::json!({ "ir": ir }),
        ))
        .await
        .unwrap();
    let body: serde_json::Value =
        serde_json::from_slice(&resp.into_body().collect().await.unwrap().to_bytes()).unwrap();
    assert!(body["errors"]
        .as_array()
        .unwrap()
        .iter()
        .any(|e| e["code"] == "ir.unknown_skill"));
}

/// first-party skill の import → 個別同意なしインストール → カタログ掲載（10.15・#344）。
#[tokio::test]
async fn first_party_skill_import_and_install_over_http() {
    let Some(pool) = setup().await else { return };
    let sessions = Arc::new(MemorySessionStore::new());
    sessions
        .put(
            "default",
            "sid-fp",
            &session_record("csrf"),
            Duration::from_secs(3600),
        )
        .await
        .unwrap();
    let app = build_router(state_with(pool.clone(), sessions));
    let cookie = "shiki_session=sid-fp.default; shiki_csrf=csrf";
    let post = |uri: String, body: serde_json::Value| {
        Request::builder()
            .method("POST")
            .uri(uri)
            .header(COOKIE, cookie)
            .header("x-csrf-token", "csrf")
            .header("content-type", "application/json")
            .body(Body::from(serde_json::to_vec(&body).unwrap()))
            .unwrap()
    };
    let suffix = uuid::Uuid::new_v4().simple().to_string();
    let name = format!("slack-notify-{}", &suffix[..8]);

    // 信頼鍵を台帳へ登録し、リポジトリ同梱バンドル（sdk/first-party-skills）へ署名する。
    let secret = [21u8; 32];
    let public = {
        use ed25519_dalek::SigningKey;
        SigningKey::from_bytes(&secret).verifying_key().to_bytes()
    };
    let keys = app_platform::TrustedKeyStore::new(pool.clone());
    let kctx = authz::AuthContext::new(
        Principal {
            kind: authz::PrincipalKind::User,
            id: "provisioner".into(),
            email: None,
            groups: vec![],
            roles: vec![],
            tenant_id: Some("default".into()),
        },
        "acme".into(),
        authz::TenantId::parse("default").unwrap(),
    );
    keys.add(&kctx, "skill-key", &public, None).await.unwrap();
    let body: serde_json::Value = serde_json::from_str(include_str!(
        "../../../sdk/first-party-skills/slack-notify/skill.json"
    ))
    .unwrap();
    let digest = app_platform::value_digest(&body);
    // 署名対象は name/version に束縛された signing digest（別名 replay 防止・#344）。
    let signing = app_platform::registry_signing_digest(&name, "1.0.0", &digest);
    let sig = app_platform::sign_digest(&signing, &secret).unwrap();

    // 改竄署名は 403（fail-closed）。
    let mut bad = sig.clone();
    bad[0] ^= 0xff;
    let resp = app
        .clone()
        .oneshot(post(
            "/skills/registry/import".into(),
            serde_json::json!({
                "name": name, "version": "1.0.0", "body": body,
                "signature_base64": base64_encode(&bad),
            }),
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::FORBIDDEN);

    // 正しい署名で import → first-party として publish される。
    let resp = app
        .clone()
        .oneshot(post(
            "/skills/registry/import".into(),
            serde_json::json!({
                "name": name, "version": "1.0.0", "body": body,
                "signature_base64": base64_encode(&sig),
            }),
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let entry: serde_json::Value =
        serde_json::from_slice(&resp.into_body().collect().await.unwrap().to_bytes()).unwrap();
    assert_eq!(entry["trust_tier"], "first_party");

    // 個別共有・管理者の個別同意なしで本人がインストールできる（署名検証済みのため）。
    let resp = app
        .clone()
        .oneshot(post(
            "/skills/installations".into(),
            serde_json::json!({ "name": name }),
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let inst: serde_json::Value =
        serde_json::from_slice(&resp.into_body().collect().await.unwrap().to_bytes()).unwrap();
    assert_eq!(inst["trust_tier"], "first_party");

    // V4 skill 照合も通る（skill:name@1.0.0 参照のワークフローが保存可能）。
    let ir = serde_json::json!({
        "ir_version": 1,
        "name": format!("notify-{}", &suffix[..8]),
        "declared_scopes": ["http.egress"],
        "nodes": [{ "id": "s", "type": "skill.invoke",
            "params": { "skill": format!("skill:{name}@1.0.0") } }],
        "edges": []
    });
    let resp = app
        .clone()
        .oneshot(post(
            "/workflows/validate".into(),
            serde_json::json!({ "ir": ir }),
        ))
        .await
        .unwrap();
    let body: serde_json::Value =
        serde_json::from_slice(&resp.into_body().collect().await.unwrap().to_bytes()).unwrap();
    assert!(
        body["errors"].as_array().unwrap().is_empty(),
        "first-party skill 参照のワークフローが保存可能: {body}"
    );
}

fn base64_encode(bytes: &[u8]) -> String {
    use base64::Engine as _;
    base64::engine::general_purpose::STANDARD.encode(bytes)
}

/// B1 のアプリのインストールは、設定から求めたシェルのポップアップの戻り先でクライアントを登録し、そのクライアントで
/// 取ったトークンで multi テナントのゲートウェイを呼べる。アンインストールでトークンが出なくなり、再インストールで
/// 戻る（BUG-172）。`KEYCLOAK_TEST_URL`（realm shiki を import 済み）と DB が要る。
#[tokio::test]
async fn b1_install_tokens_reach_the_gateway_in_multi_tenancy() {
    use app_gateway::test_support::TestRealm;
    let Some(pool) = setup().await else { return };
    let Some(realm) = TestRealm::from_env() else {
        eprintln!("KEYCLOAK_TEST_URL 未設定のためスキップ");
        return;
    };
    // alice（realm の a-corp の人）のセッション。
    const ALICE: &str = "00000000-0000-0000-0000-000000000001";
    let mut record = session_record("csrf");
    record.principal.id = ALICE.into();
    record.principal.groups = vec!["/tenants/a-corp/a-corp".into()];
    record.principal.tenant_id = Some("a-corp".into());
    record.tenant_id = "a-corp".into();
    let sid = format!("sid-{}", uuid::Uuid::new_v4().simple());
    let sessions = Arc::new(MemorySessionStore::new());
    sessions
        .put("a-corp", &sid, &record, Duration::from_secs(3600))
        .await
        .unwrap();
    let mut state = state_with(pool.clone(), sessions);
    let mut config = (*state.config).clone();
    config.auth.issuer = realm.issuer();
    config.auth.tenancy = Tenancy::Multi;
    let (id, secret) = realm.provisioner();
    config.auth.provisioner_client_id = Some(id.into());
    config.auth.provisioner_client_secret = Some(secret.into());
    // web のオリジンは BFF のログインの戻り先（既定 http://localhost:3000/auth/callback）から求める。
    config.gateway.web_origin = None;
    // ゲートウェイの audience を既定から変えても、インストールが登録するクライアントのトークンは同じ値を aud に持ち、
    // ゲートウェイを呼べる（以前は登録が定数の shiki-gateway を書き、どのトークンも 401 だった・BUG-206）。
    config.gateway.audience = "shiki-gateway-r7f".into();
    let shell_callback = "http://localhost:3000/apps/oauth/callback";
    assert_eq!(config.miniapp_b1_redirect_uris(), [shell_callback]);
    state.installs = Arc::new(api::miniapp_install::build_install_service(
        &config,
        &reqwest::Client::new(),
        &pool,
        &state.authz,
        &state.mini_app_code,
        &state.data,
        None,
    ));
    state.config = Arc::new(config.clone());
    let gateway = app_gateway::build_gateway_router(app_gateway::GatewayState {
        installations: app_gateway::AppInstallationStore::new(pool.clone()),
        keys: Arc::new(api::middleware::JwksCache::new(
            reqwest::Client::new(),
            config.auth.effective_jwks_uri(),
            Duration::from_secs(300),
        )),
        token_cfg: app_gateway::GatewayTokenConfig {
            audience: config.gateway.audience.clone(),
            issuer: config.auth.issuer.clone(),
        },
        authz: state.authz.clone(),
        audit: storage::audit::AuditRecorder::new(pool.clone()),
        tenant_source: config.auth.tenant_source(),
        caps: app_gateway::CapabilityDeps {
            db: pool.clone(),
            storage: Arc::clone(&state.storage),
            data: Arc::clone(&state.data),
            fsms: Arc::clone(&state.fsms),
            rag: Arc::new(app_gateway::NoRag),
            notifications: app_gateway::NotificationStore::new(pool.clone()),
            llm: None,
            agent: Arc::new(app_gateway::NoAgent),
            ai_daily_cap_usd_micros: 0,
            functions: Arc::new(app_gateway::NoFunctions),
        },
    });
    let app = build_router(state);
    let cookie = format!("shiki_session={sid}.a-corp; shiki_csrf=csrf");
    let call = |method: &str, uri: String, body: Option<serde_json::Value>| {
        let app = app.clone();
        let req = Request::builder()
            .method(method)
            .uri(uri)
            .header(COOKIE, cookie.clone())
            .header("x-csrf-token", "csrf")
            .header("content-type", "application/json")
            .body(body.map_or_else(Body::empty, |b| Body::from(b.to_string())))
            .unwrap();
        async move {
            let resp = app.oneshot(req).await.unwrap();
            let status = resp.status();
            let bytes = resp.into_body().collect().await.unwrap().to_bytes();
            (
                status,
                serde_json::from_slice::<serde_json::Value>(&bytes).unwrap_or_default(),
            )
        }
    };
    let whoami = |token: String| {
        let gateway = gateway.clone();
        async move {
            let resp = gateway
                .oneshot(
                    Request::builder()
                        .uri("/gw/whoami")
                        .header("authorization", format!("Bearer {token}"))
                        .body(Body::empty())
                        .unwrap(),
                )
                .await
                .unwrap();
            let status = resp.status();
            let bytes = resp.into_body().collect().await.unwrap().to_bytes();
            (
                status,
                serde_json::from_slice::<serde_json::Value>(&bytes).unwrap_or_default(),
            )
        }
    };

    let name = format!("b1-http-{}", uuid::Uuid::new_v4().simple());
    let (s, body) = call(
        "POST",
        "/apps/manifests".into(),
        Some(serde_json::json!({ "manifest": manifest_json(&name, "1.0.0") })),
    )
    .await;
    assert_eq!(s, StatusCode::CREATED, "{body}");
    let app_id = body["id"].as_str().unwrap().to_string();
    let client_id = format!("app-{app_id}-b1");
    let install =
        serde_json::json!({ "name": name, "version": "1.0.0", "granted_scopes": ["data.read"] });
    let body = async {
        let (s, body) = call(
            "POST",
            format!("/apps/manifests/{app_id}/publish"),
            Some(serde_json::json!({})),
        )
        .await;
        assert_eq!(s, StatusCode::CREATED, "{body}");
        let (s, body) = call("POST", "/apps/installations".into(), Some(install.clone())).await;
        assert_eq!(s, StatusCode::OK, "{body}");
        assert_eq!(body["client_id_b1"], client_id);
        let client = realm.client(&client_id).await.expect("client");
        assert_eq!(client["redirectUris"], serde_json::json!([shell_callback]));

        // シェルのポップアップと同じ要求で取ったトークンで、multi テナントのゲートウェイを呼べる。
        let token = realm
            .user_token(&client_id, shell_callback, "alice", "openid data.read")
            .await
            .expect("token");
        let (s, body) = whoami(token.clone()).await;
        assert_eq!(s, StatusCode::OK, "{body}");
        assert_eq!(body["user_sub"], ALICE);
        assert_eq!(body["app_id"], app_id);

        // アンインストール: 発行済みのトークンは断られ、新しいトークンは出ない。
        let (s, body) = call("DELETE", format!("/apps/installations/{app_id}"), None).await;
        assert!(s.is_success(), "{s} {body}");
        assert_eq!(whoami(token).await.0, StatusCode::FORBIDDEN);
        assert!(realm
            .user_token(&client_id, shell_callback, "alice", "openid data.read")
            .await
            .is_err());

        // 再インストールでクライアントが有効に戻り、またゲートウェイを呼べる。
        let (s, body) = call("POST", "/apps/installations".into(), Some(install.clone())).await;
        assert_eq!(s, StatusCode::OK, "{body}");
        let token = realm
            .user_token(&client_id, shell_callback, "alice", "openid data.read")
            .await
            .expect("token after reinstall");
        assert_eq!(whoami(token).await.0, StatusCode::OK);
    };
    let outcome = futures_catch(body).await;
    let _ = call("DELETE", format!("/apps/installations/{app_id}"), None).await;
    realm.delete_client(&client_id).await;
    sqlx::query("DELETE FROM app_installation WHERE tenant_id = 'a-corp' AND app_id = $1::uuid")
        .bind(&app_id)
        .execute(&pool)
        .await
        .expect("cleanup installation");
    if let Err(panic) = outcome {
        std::panic::resume_unwind(panic);
    }
}

/// 自動実行の直近の失敗は、インストールの一覧の `function_failures` に添えて返す（持ち主にだけ・GAP-60。持ち主で
/// ない人に返さないことは、実 OpenFGA の miniapp_b2_tests が確かめる）。持ち主かを確かめられない（OpenFGA の一時的な
/// 障害・`fga_down`）ときも一覧は返し、失敗を添えない（持ち主でないときと同じ見え方）。
async fn assert_function_failures_are_listed(
    app: &axum::Router,
    fga_down: &axum::Router,
    cookie: &str,
    pool: &PgPool,
    app_id: &str,
) {
    let app_uuid = uuid::Uuid::parse_str(app_id).unwrap();
    let (tenant, org): (String, String) =
        sqlx::query_as("SELECT tenant_id, org FROM app_installation WHERE app_id = $1")
            .bind(app_uuid)
            .fetch_one(pool)
            .await
            .unwrap();
    let failure = app_platform::FunctionFailure {
        code: "script_error".into(),
        message: "金額が空です".into(),
        retryable: false,
    };
    app_platform::FunctionFailureStore::new(pool.clone())
        .record_failure(
            &tenant,
            &org,
            app_uuid,
            "remind",
            app_platform::FunctionTrigger::Cron,
            &failure,
        )
        .await
        .unwrap();
    let list = |router: &axum::Router| {
        router.clone().oneshot(
            Request::builder()
                .uri("/apps/installations")
                .header(COOKIE, cookie)
                .body(Body::empty())
                .unwrap(),
        )
    };
    let resp = list(app).await.unwrap();
    let body: serde_json::Value =
        serde_json::from_slice(&resp.into_body().collect().await.unwrap().to_bytes()).unwrap();
    let down = list(fga_down).await.unwrap();
    let down_status = down.status();
    let down_body: serde_json::Value =
        serde_json::from_slice(&down.into_body().collect().await.unwrap().to_bytes()).unwrap();
    sqlx::query("DELETE FROM app_function_failure WHERE tenant_id = $1 AND app_id = $2")
        .bind(&tenant)
        .bind(app_uuid)
        .execute(pool)
        .await
        .unwrap();
    let listed = body["items"]
        .as_array()
        .unwrap()
        .iter()
        .find(|i| i["app_id"] == app_id)
        .expect("listed");
    let failures = listed["function_failures"].as_array().expect("failures");
    assert_eq!(failures.len(), 1, "{listed}");
    assert_eq!(failures[0]["function"], "remind");
    assert_eq!(failures[0]["trigger"], "cron");
    assert_eq!(failures[0]["message"], "金額が空です");
    assert_eq!(failures[0]["failures"], 1);

    assert_eq!(down_status, StatusCode::OK, "{down_body}");
    let listed = down_body["items"]
        .as_array()
        .unwrap()
        .iter()
        .find(|i| i["app_id"] == app_id)
        .expect("OpenFGA が一時的に落ちていても一覧は返す");
    assert!(listed.get("function_failures").is_none(), "{listed}");
}

/// 本体の panic を捕まえる（片付けをしてから落とすため）。
async fn futures_catch<F: std::future::Future<Output = ()>>(
    body: F,
) -> Result<(), Box<dyn std::any::Any + Send>> {
    use futures::FutureExt as _;
    std::panic::AssertUnwindSafe(body).catch_unwind().await
}
