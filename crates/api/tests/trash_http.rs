//! ゴミ箱と保持期間の口の結合テスト（実 Postgres ＋実 OpenFGA ＋ RustFS・GAP-108b / GAP-108c・7p レビュー）。
//!
//! ルータ（セッション・CSRF）を通して、次を固定する。
//!
//! - `GET /trash`: 項目の直下にノードの項目（flatten）と、完全に削除できるか・配下の数・削除した時刻・保持期間の
//!   期限が出る。ページに保持日数が出る。
//! - `DELETE /trash/{id}`: 持ち主でない editor は 403（消えない）。ゴミ箱に無い・削除したフォルダの中は、
//!   `code` を持つ 409。持ち主は 204 で、配下のファイルも見つからなくなる。
//! - `POST /trash/empty`: 空の `cursor` は先頭から・読めない `cursor` は 400・CSRF の無い呼び出しは 403。
//! - `PUT /admin/tenants/{tenant_id}/trash-retention` のハンドラ: 範囲・省略・桁溢れは 400、無いテナントは 404、
//!   設定と監査が残る（ルートの登録と provisioner の認可は `server_tests`・`auth_flow` が確かめる）。
//!
//! テストごとに別のテナント・組織・OpenFGA のストアを使う（ほかのテストのゴミ箱・保持期間と混ざらない）。
//! `STORAGE_TEST_DATABASE_URL` と `OPENFGA_TEST_URL` が無ければスキップ（`SHIKI_REQUIRE_IT=1` なら失敗）。

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
    error::ApiError,
    routes::admin::{set_tenant_trash_retention, ProvisionerIdentity, TenantTrashRetentionRequest},
    session::{MemorySessionStore, SessionStore},
    state::AppState,
};
use authz::client::{OpenFgaClient, OpenFgaConfig};
use authz::{AuthContext, AuthzClient, Consistency, FgaObject, Principal, Relation};
use axum::body::Body;
use axum::extract::{Path, State};
use axum::http::{header::COOKIE, Request, StatusCode};
use axum::{Extension, Json};
use chrono::{DateTime, Utc};
use http_body_util::BodyExt;
use serde_json::{json, Value};
use sqlx::postgres::PgPoolOptions;
use storage::{ShareRole, ShareTarget, StorageService};
use tower::ServiceExt;
use uuid::Uuid;

fn config(db_url: &str, tenant: &str) -> AppConfig {
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
            tenant_id: Some(tenant.into()),
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
        membership_reconcile: api::membership_reconcile::MembershipReconcileConfig::default(),
        egress: Default::default(),
    }
}

struct Env {
    app: axum::Router,
    state: AppState,
    pool: sqlx::PgPool,
    fga: Arc<dyn AuthzClient>,
    storage: Arc<StorageService>,
    tenant: String,
    org: String,
}

impl Env {
    fn ctx(&self, user: &str) -> AuthContext {
        AuthContext::new(
            principal(user, &self.org),
            self.org.clone(),
            authz::TenantId::parse(self.tenant.clone()).unwrap(),
        )
    }

    async fn wait_check(&self, user: &str, relation: Relation, obj: &FgaObject) {
        let ctx = self.ctx(user);
        for _ in 0..200 {
            if self
                .fga
                .check(
                    &ctx.subject(),
                    relation,
                    obj,
                    Consistency::HigherConsistency,
                )
                .await
                .expect("check")
            {
                return;
            }
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
        panic!("seed したタプルが見えませんでした");
    }

    /// `user` のファイルを `parent` に置く（本体は RustFS に入る）。
    async fn write(&self, user: &str, parent: Option<Uuid>, name: &str) -> Uuid {
        let ctx = self.ctx(user);
        let node = self
            .storage
            .write_file_internal(
                &ctx,
                parent,
                name,
                format!("trash-http {name} {}", Uuid::new_v4()).as_bytes(),
                "text/plain",
                None,
            )
            .await
            .expect("書き込み");
        self.wait_check(user, Relation::Editor, &ctx.ns().file(&node.id.to_string()))
            .await;
        node.id
    }

    async fn exists(&self, id: Uuid) -> bool {
        sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM node WHERE tenant_id = $1 AND id = $2)")
            .bind(&self.tenant)
            .bind(id)
            .fetch_one(&self.pool)
            .await
            .unwrap()
    }

    async fn send(&self, r: Request<Body>) -> (StatusCode, Value) {
        let resp = self.app.clone().oneshot(r).await.unwrap();
        let status = resp.status();
        let bytes = resp.into_body().collect().await.unwrap().to_bytes();
        (
            status,
            serde_json::from_slice(&bytes).unwrap_or(Value::Null),
        )
    }

    fn req(
        &self,
        method: &str,
        user: &str,
        path: &str,
        body: Option<&Value>,
        csrf: bool,
    ) -> Request<Body> {
        let mut b = Request::builder()
            .method(method)
            .uri(path)
            .header(
                COOKIE,
                format!(
                    "shiki_session=sid-{user}.{}; shiki_csrf=csrf-{user}",
                    self.tenant
                ),
            )
            .header("content-type", "application/json");
        if csrf {
            b = b.header("x-csrf-token", format!("csrf-{user}"));
        }
        b.body(body.map_or_else(Body::empty, |v| Body::from(v.to_string())))
            .unwrap()
    }

    /// `user` のゴミ箱の 1 ページ目。
    async fn trash(&self, user: &str) -> Value {
        let (status, body) = self
            .send(self.req("GET", user, "/trash", None, false))
            .await;
        assert_eq!(status, StatusCode::OK, "{body}");
        body
    }
}

fn principal(id: &str, org: &str) -> Principal {
    Principal {
        kind: authz::PrincipalKind::User,
        id: id.into(),
        email: None,
        groups: vec![format!("/{org}")],
        roles: vec![],
        tenant_id: None,
    }
}

/// 利用者（セッション ID = 利用者 ID）。alice: フォルダの持ち主／bob: フォルダの editor（持ち主でも管理者でもない）。
const USERS: [&str; 2] = ["alice", "bob"];

async fn setup() -> Option<Env> {
    let (Ok(db_url), Ok(fga_url)) = (
        shiki_testenv::var("STORAGE_TEST_DATABASE_URL"),
        shiki_testenv::var("OPENFGA_TEST_URL"),
    ) else {
        eprintln!("STORAGE_TEST_DATABASE_URL / OPENFGA_TEST_URL 未設定のためスキップ");
        return None;
    };
    let s3_endpoint = std::env::var("STORAGE_TEST_S3_ENDPOINT")
        .unwrap_or_else(|_| "http://localhost:9000".into());
    let pool = PgPoolOptions::new()
        .max_connections(8)
        .connect(&db_url)
        .await
        .expect("pg");
    sqlx::migrate!("../../migrations")
        .run(&pool)
        .await
        .expect("migrate");
    let suffix = Uuid::new_v4().simple().to_string();
    let tenant = format!("trhttp{}", &suffix[..12]);
    let org = format!("trorg{}", &suffix[..12]);
    sqlx::query(
        "INSERT INTO tenant (tenant_id, org, display_name, status) VALUES ($1, $2, 'trash http', 'active')",
    )
    .bind(&tenant)
    .bind(&org)
    .execute(&pool)
    .await
    .expect("tenant");
    let fga: Arc<dyn AuthzClient> = Arc::new(
        OpenFgaClient::connect(
            reqwest::Client::new(),
            &OpenFgaConfig {
                base_url: fga_url,
                store_name: format!("shiki-trash-http-{}", Uuid::new_v4()),
            },
            &authz::model::default_model(),
        )
        .await
        .expect("fga"),
    );
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
    object_store.ensure_bucket().await.expect("バケット準備");
    let storage = Arc::new(StorageService::new(
        pool.clone(),
        Arc::clone(&object_store),
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
                &tenant,
                &format!("sid-{u}"),
                &api::session::SessionRecord {
                    principal: principal(u, &org),
                    tenant_id: tenant.clone(),
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
    let state = build_state(
        &pool,
        config(&db_url, &tenant),
        Arc::clone(&storage),
        Arc::clone(&object_store),
        tabular,
        sessions,
        &fga,
        directory,
        data_store,
    );
    let env = Env {
        app: build_router(state.clone()),
        state,
        pool,
        fga,
        storage,
        tenant,
        org,
    };
    // 2 人とも組織のメンバー（「空にする」はメンバーでないと 403）。
    for u in USERS {
        let ctx = env.ctx(u);
        let org_obj = ctx.ns().organization(&env.org);
        env.fga
            .write_tuple(&ctx.subject(), Relation::Member, &org_obj)
            .await
            .expect("member");
        env.wait_check(u, Relation::Member, &org_obj).await;
    }
    Some(env)
}

fn item(page: &Value, id: Uuid) -> &Value {
    page["items"]
        .as_array()
        .unwrap()
        .iter()
        .find(|i| i["id"] == id.to_string())
        .unwrap_or_else(|| panic!("ゴミ箱に {id} がありません: {page}"))
}

fn ts(v: &Value) -> DateTime<Utc> {
    DateTime::parse_from_rfc3339(
        v.as_str()
            .unwrap_or_else(|| panic!("時刻ではありません: {v}")),
    )
    .unwrap()
    .with_timezone(&Utc)
}

/// 一覧・1 件の完全削除・空にするを、ルータを通して確かめる。
#[tokio::test]
async fn trash_routes_list_purge_and_empty() {
    let Some(env) = setup().await else { return };
    let alice = env.ctx("alice");
    let bob = env.ctx("bob");

    // alice のフォルダに alice と bob（editor）のファイルを置き、alice がフォルダをゴミ箱へ移す。
    let dir = env
        .storage
        .create_folder(&alice, None, "共有のフォルダ", None)
        .await
        .unwrap()
        .id;
    env.wait_check(
        "alice",
        Relation::Owner,
        &alice.ns().folder(&dir.to_string()),
    )
    .await;
    env.storage
        .share_node(
            &alice,
            dir,
            &ShareTarget::User { id: "bob".into() },
            ShareRole::Editor,
            None,
        )
        .await
        .unwrap();
    env.wait_check("bob", Relation::Editor, &bob.ns().folder(&dir.to_string()))
        .await;
    let alices = env.write("alice", Some(dir), "alice.txt").await;
    env.write("bob", Some(dir), "bob.txt").await;
    env.storage
        .soft_delete_folder(&alice, dir, None)
        .await
        .unwrap();

    // (1) 一覧: ノードの項目は直下にあり（flatten）、持ち主は消せる。配下の数とほかの人の数・削除した時刻が出る。
    // 保持期間が無いので期限と日数は null。
    let page = env.trash("alice").await;
    assert_eq!(page["retention_days"], Value::Null, "{page}");
    let it = item(&page, dir);
    assert_eq!(it["name"], "共有のフォルダ");
    assert_eq!(it["kind"], "folder");
    assert_eq!(it["can_purge"], true);
    assert_eq!(it["file_count"], 2);
    assert_eq!(it["others_file_count"], 1);
    assert_eq!(it["purge_after"], Value::Null);
    let deleted_at: DateTime<Utc> =
        sqlx::query_scalar("SELECT deleted_at FROM node WHERE tenant_id = $1 AND id = $2")
            .bind(&env.tenant)
            .bind(dir)
            .fetch_one(&env.pool)
            .await
            .unwrap();
    assert_eq!(ts(&it["deleted_at"]), deleted_at);
    let page = env.trash("bob").await;
    assert_eq!(item(&page, dir)["can_purge"], false, "editor は消せない");

    // (2) 持ち主でも管理者でもない人の完全削除は 403 で、何も消えない。
    let (status, body) = env
        .send(env.req("DELETE", "bob", &format!("/trash/{dir}"), None, true))
        .await;
    assert_eq!(status, StatusCode::FORBIDDEN, "{body}");
    assert!(env.exists(dir).await && env.exists(alices).await);

    // (3) ゴミ箱に無いものと、削除したフォルダの中のものは、code を持つ 409。
    let live = env.write("alice", None, "生きているファイル.txt").await;
    for (id, code) in [(live, "not_in_trash"), (alices, "inside_deleted_folder")] {
        let (status, body) = env
            .send(env.req("DELETE", "alice", &format!("/trash/{id}"), None, true))
            .await;
        assert_eq!(status, StatusCode::CONFLICT, "{code}: {body}");
        assert_eq!(body["code"], code, "{body}");
        assert_eq!(body["status"], 409);
        assert!(
            body["title"].as_str().is_some_and(|t| !t.is_empty()),
            "{body}"
        );
        assert!(env.exists(id).await, "{code}: 消えない");
    }

    // (4) 持ち主は 204 で消せる。中の自分のファイルも見つからなくなる。
    let (status, body) = env
        .send(env.req("DELETE", "alice", &format!("/trash/{dir}"), None, true))
        .await;
    assert_eq!(status, StatusCode::NO_CONTENT, "{body}");
    let (status, _) = env
        .send(env.req("GET", "alice", &format!("/files/{alices}"), None, false))
        .await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    assert!(!env.exists(dir).await && !env.exists(alices).await);

    // (5) 空にする: 空の cursor は先頭から。読めない cursor は 400、CSRF の無い呼び出しは 403。
    let gone = env.write("alice", None, "空にするファイル.txt").await;
    env.storage
        .soft_delete_file(&alice, gone, None)
        .await
        .unwrap();
    let (status, body) = env
        .send(env.req(
            "POST",
            "alice",
            "/trash/empty",
            Some(&json!({ "cursor": "" })),
            false,
        ))
        .await;
    assert_eq!(status, StatusCode::FORBIDDEN, "CSRF のヘッダが無い: {body}");
    assert!(env.exists(gone).await, "CSRF で断ったら消さない");
    let (status, body) = env
        .send(env.req(
            "POST",
            "alice",
            "/trash/empty",
            Some(&json!({ "cursor": "zz" })),
            true,
        ))
        .await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{body}");
    let (status, body) = env
        .send(env.req(
            "POST",
            "alice",
            "/trash/empty",
            Some(&json!({ "cursor": "" })),
            true,
        ))
        .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert!(body["purged"].as_u64().unwrap() >= 1, "{body}");
    assert!(!env.exists(gone).await);
    assert!(env.exists(live).await, "ゴミ箱に無いものは消さない");
}

/// テナントに保持期間があれば、一覧のページに日数が、項目に自動で消える期限が出る（掃除と同じ式）。
#[tokio::test]
async fn trash_list_shows_retention_deadline() {
    let Some(env) = setup().await else { return };
    let alice = env.ctx("alice");
    let change = env
        .storage
        .trash_retention()
        .set_retention_days(&env.tenant, Some(30), "test", json!({}))
        .await
        .unwrap()
        .expect("active なテナント");
    let since = change.since.expect("日数を入れたら猶予の起点が入る");
    let file = env.write("alice", None, "期限のあるファイル.txt").await;
    env.storage
        .soft_delete_file(&alice, file, None)
        .await
        .unwrap();
    // 起点（日数を入れた時刻）より後に削除したものは、削除した時刻から数える。
    let page = env.trash("alice").await;
    assert_eq!(page["retention_days"], 30, "{page}");
    let it = item(&page, file);
    let deleted_at = ts(&it["deleted_at"]);
    assert_eq!(
        ts(&it["purge_after"]),
        deleted_at.max(since) + chrono::Duration::days(30)
    );
    // 猶予の起点が削除より後（日数を入れる前に削除したもの）なら、起点から数える。
    let later = deleted_at + chrono::Duration::days(2);
    sqlx::query("UPDATE tenant SET trash_retention_since = $2 WHERE tenant_id = $1")
        .bind(&env.tenant)
        .bind(later)
        .execute(&env.pool)
        .await
        .unwrap();
    let page = env.trash("alice").await;
    assert_eq!(
        ts(&item(&page, file)["purge_after"]),
        later + chrono::Duration::days(30)
    );
}

/// 運用者の口のハンドラ: 範囲・省略・桁溢れは 400 で値を変えない、`null` で止める、無いテナントは 404。
/// 変更は provisioner を記録者として監査に残る。
#[tokio::test]
async fn admin_trash_retention_handler_validates_and_records() {
    let Some(env) = setup().await else { return };
    let put = |tenant: String, body: Value| {
        let state = env.state.clone();
        async move {
            set_tenant_trash_retention(
                State(state),
                Extension(ProvisionerIdentity("provisioner:shiki-provisioner".into())),
                Path(tenant),
                Json(serde_json::from_value::<TenantTrashRetentionRequest>(body).unwrap()),
            )
            .await
        }
    };
    let setting = || async {
        sqlx::query_as::<_, (Option<i32>, Option<DateTime<Utc>>)>(
            "SELECT trash_retention_days, trash_retention_since FROM tenant WHERE tenant_id = $1",
        )
        .bind(&env.tenant)
        .fetch_one(&env.pool)
        .await
        .unwrap()
    };

    assert_eq!(
        put(env.tenant.clone(), json!({ "days": 30 }))
            .await
            .unwrap(),
        StatusCode::NO_CONTENT
    );
    let (days, since) = setting().await;
    assert_eq!(days, Some(30));
    let since = since.expect("日数を入れたら猶予の起点が入る");
    let (actor, grace_reset): (String, Value) = sqlx::query_as(
        "SELECT actor, metadata -> 'grace_reset' FROM audit_log \
         WHERE tenant_id = $1 AND action = 'tenant.trash_retention.set'",
    )
    .bind(&env.tenant)
    .fetch_one(&env.pool)
    .await
    .unwrap();
    assert_eq!(actor, "provisioner:shiki-provisioner");
    assert_eq!(grace_reset, json!(true));

    for bad in [
        json!({}),
        json!({ "days": 0 }),
        json!({ "days": 3651 }),
        json!({ "days": 4_000_000_000_i64 }),
    ] {
        match put(env.tenant.clone(), bad.clone()).await {
            Err(ApiError::BadRequest(_)) => {}
            other => panic!("{bad}: {other:?}"),
        }
    }
    assert_eq!(
        setting().await,
        (Some(30), Some(since)),
        "断った値で変わらない"
    );

    // null で止める。猶予の起点は変えない。
    assert_eq!(
        put(env.tenant.clone(), json!({ "days": null }))
            .await
            .unwrap(),
        StatusCode::NO_CONTENT
    );
    assert_eq!(setting().await, (None, Some(since)));

    // 無いテナントは 404、使えない文字の tenant_id は 400。
    match put(format!("{}x", env.tenant), json!({ "days": 30 })).await {
        Err(ApiError::NotFound) => {}
        other => panic!("無いテナント: {other:?}"),
    }
    match put("bad tenant".into(), json!({ "days": 30 })).await {
        Err(ApiError::BadRequest(_)) => {}
        other => panic!("使えない文字: {other:?}"),
    }
}

#[allow(clippy::too_many_arguments)]
fn build_state(
    pool: &sqlx::PgPool,
    config: AppConfig,
    storage: Arc<storage::StorageService>,
    object_store: Arc<dyn storage::ObjectStore>,
    tabular: Arc<tabular::TabularService>,
    sessions: Arc<MemorySessionStore>,
    authz: &Arc<dyn AuthzClient>,
    directory: Arc<storage::DirectoryStore>,
    data_store: Arc<data::DataStore>,
) -> AppState {
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
        object_store,
        Arc::clone(authz),
        storage::audit::AuditRecorder::new(pool.clone()),
    ));
    let app_usage = Arc::new(app_platform::AppUsageStore::new(
        pool.clone(),
        Arc::clone(authz),
    ));
    AppState {
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
        egress_proxy: None,
        docx_composer: std::sync::Arc::new(office::DocxComposer::new(
            reqwest::Client::new(),
            "http://127.0.0.1:1",
        )),
    }
}
