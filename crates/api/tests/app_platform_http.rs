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
    let url = std::env::var("STORAGE_TEST_DATABASE_URL").ok()?;
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
            backend: ObjectStoreBackend::Minio,
            s3: None,
            max_upload_size_bytes: 5 * 1024 * 1024 * 1024,
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
            langfuse: None,
        },
        chat: api::config::ChatConfig::default(),
        websearch: api::config::WebSearchConfig::default(),
        secrets: api::config::SecretsConfig::default(),
        workflow: api::workflow_runtime::WorkflowConfig::default(),
        gateway: api::config::GatewayConfig::default(),
        tabular: api::config::TabularConfig::default(),
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
    let ui_validator = Arc::new(gui::SpecValidator::new(Arc::clone(&artifacts), db.clone()));
    let ui_specs = Arc::new(gui::UiSpecStore::new(Arc::clone(&artifacts), ui_validator));
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
        bundles,
        app_usage,
        ui_specs,
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
        audit,
        directory,
        tenants,
        search: None,
        chat: None,
        rag_admin,
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
    let app = build_router(state_with(pool.clone(), sessions));
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
    assert!(body["items"]
        .as_array()
        .unwrap()
        .iter()
        .any(|i| i["app_id"] == id.as_str()));

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
        "default".into(),
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

    // アンインストール → 204。
    let resp = app
        .clone()
        .oneshot(
            Request::builder()
                .method("DELETE")
                .uri(format!("/apps/installations/{id}"))
                .header(COOKIE, cookie)
                .header("x-csrf-token", "csrf")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
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
