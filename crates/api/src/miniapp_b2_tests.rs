//! B2（サーバーの関数）を本物の経路で通す結合テスト（BUG-182・183）。
//!
//! 実 Keycloak（`KEYCLOAK_TEST_URL`）・実 Postgres（`STORAGE_TEST_DATABASE_URL`）・実 OpenFGA（`OPENFGA_TEST_URL`）
//! と、実 QuickJS の関数の実行で確かめる。どれかが無ければスキップする。
//!
//! 1. 同意インストールで B1・B2 のクライアントを登録し、B2 の secret を保管する（OpenFGA の can_use@miniapp）。
//! 2. 自動実行（cron と同じ [`super::invoke_service`]）: B2 の client_credentials のトークンで関数を動かすと、
//!    ゲートウェイ（multi テナント）がアプリの主体として扱い、関数がアプリのテーブルにレコードを足せる。
//! 3. 利用者が起点（`POST /gw/apps/functions/{function}/invoke`・[`GatewayFunctionPort`]）: alice の B1 のトークンを
//!    B2 が交換し、関数が alice の権限でアプリのテーブルを読める（関数の中の主体は利用者のまま）。
//!
//! 作ったクライアント・行・OpenFGA のストアは、テストの最後に消す。

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::large_futures,
    clippy::print_stderr
)]

use std::collections::HashMap;
use std::panic::AssertUnwindSafe;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use app_gateway::test_support::TestRealm;
use app_platform::{
    Budget, FunctionRunner, InstallRequest, InstallService, ManifestTable, MiniAppCodeStore,
    MiniAppManifest, Registry, ServerSpec, TrustTier,
};
use authz::client::{OpenFgaClient, OpenFgaConfig};
use authz::{AuthContext, AuthzClient, Principal, PrincipalKind, Relation};
use data::{FieldDef, FieldType, RefResolver, TableSchema};
use futures::FutureExt as _;
use script_runtime::ScriptEngine;
use sqlx::PgPool;
use storage::content_address::{miniapp_bundle_key, sha256_hex};
use storage::{ObjectStore, ObjectStoreError};
use uuid::Uuid;

use super::{invoke_service, Subscription, TriggerDeps};
use crate::gateway_functions::GatewayFunctionPort;

/// web シェルのホスト支援 PKCE の戻り先（web/src/lib/miniapp-oauth.ts と同じ）。
const SHELL_CALLBACK: &str = "http://localhost:3000/apps/oauth/callback";
/// realm の試しの利用者 alice の sub（deploy/keycloak/shiki-realm.json）。
const ALICE: &str = "00000000-0000-0000-0000-000000000001";

/// 関数: count はアプリのテーブルの行を数え、remind は行を足す。どちらも呼び出す主体（actor）を返す。
const SCRIPT: &str = r#"
function main(input) {
  var tables = Shiki.data.listTables();
  var t = tables[0].id;
  if (input.function === "count") {
    var q = Shiki.data.query(t, {});
    return { actor: input.actor, tables: tables.length, rows: q.items.length };
  }
  var r = Shiki.data.create(t, { title: "自動で足した行" });
  return { actor: input.actor, owner: r.owner };
}
"#;

/// メモリの ObjectStore（サーバーのコードの置き場）。
#[derive(Default)]
struct MemStore(Mutex<HashMap<String, Vec<u8>>>);

#[async_trait::async_trait]
impl ObjectStore for MemStore {
    async fn ensure_bucket(&self) -> Result<(), ObjectStoreError> {
        Ok(())
    }
    async fn presign_put(&self, _: &str, _: Duration, _: i64) -> Result<String, ObjectStoreError> {
        Ok(String::new())
    }
    async fn presign_get(
        &self,
        _: &str,
        _: Duration,
        _: Option<&str>,
        _: Option<&str>,
    ) -> Result<String, ObjectStoreError> {
        Ok(String::new())
    }
    async fn presign_get_internal(&self, _: &str, _: Duration) -> Result<String, ObjectStoreError> {
        Ok(String::new())
    }
    async fn read_and_hash(&self, k: &str) -> Result<(String, u64), ObjectStoreError> {
        Err(ObjectStoreError::NotFound(k.into()))
    }
    async fn put_object(&self, k: &str, b: Vec<u8>, _: &str) -> Result<(), ObjectStoreError> {
        self.0.lock().unwrap().insert(k.into(), b);
        Ok(())
    }
    async fn get_object(&self, k: &str) -> Result<Vec<u8>, ObjectStoreError> {
        self.0
            .lock()
            .unwrap()
            .get(k)
            .cloned()
            .ok_or_else(|| ObjectStoreError::NotFound(k.into()))
    }
    async fn exists(&self, k: &str) -> Result<bool, ObjectStoreError> {
        Ok(self.0.lock().unwrap().contains_key(k))
    }
    async fn copy(&self, _: &str, _: &str) -> Result<(), ObjectStoreError> {
        Ok(())
    }
    async fn delete(&self, _: &str) -> Result<(), ObjectStoreError> {
        Ok(())
    }
    async fn list_prefix(
        &self,
        _: &str,
        _: Option<&str>,
    ) -> Result<(Vec<String>, Option<String>), ObjectStoreError> {
        Ok((vec![], None))
    }
    async fn delete_batch(&self, _: &[String]) -> Result<(), ObjectStoreError> {
        Ok(())
    }
}

/// 参照の確かめ（このテストの表は参照の列を持たない）。
struct NoRefs;

#[async_trait::async_trait]
impl RefResolver for NoRefs {
    async fn user_exists(&self, _: &AuthContext, _: &str) -> Result<bool, String> {
        Ok(false)
    }
    async fn role_exists(&self, _: &AuthContext, _: &str) -> Result<bool, String> {
        Ok(true)
    }
    async fn file_readable(&self, _: &AuthContext, _: Uuid) -> Result<bool, String> {
        Ok(false)
    }
}

fn user_ctx(tenant: &str, user: &str) -> AuthContext {
    AuthContext::new(
        Principal {
            kind: PrincipalKind::User,
            id: user.into(),
            email: None,
            groups: vec![],
            roles: vec![],
            tenant_id: Some(tenant.into()),
        },
        "acme".into(),
        tenant.into(),
    )
}

struct Env {
    pool: PgPool,
    realm: TestRealm,
    authz: Arc<dyn AuthzClient>,
    fga_url: String,
    store_id: String,
    objects: Arc<MemStore>,
    data: Arc<data::DataStore>,
    secrets: Arc<secrets::SecretStore>,
    engine: Arc<ScriptEngine>,
    tenant: String,
}

async fn env() -> Option<Env> {
    let realm = TestRealm::from_env()?;
    let db_url = std::env::var("STORAGE_TEST_DATABASE_URL").ok()?;
    let fga_url = std::env::var("OPENFGA_TEST_URL").ok()?;
    let pool = sqlx::postgres::PgPoolOptions::new()
        .max_connections(8)
        .connect(&db_url)
        .await
        .expect("pg");
    sqlx::migrate!("../../migrations")
        .run(&pool)
        .await
        .expect("migrate");
    let fga = OpenFgaClient::connect(
        reqwest::Client::new(),
        &OpenFgaConfig {
            base_url: fga_url.clone(),
            store_name: format!("miniapp-b2-it-{}", Uuid::new_v4()),
        },
        &authz::model::default_model(),
    )
    .await
    .expect("OpenFGA");
    let store_id = fga.store_id().to_string();
    let authz: Arc<dyn AuthzClient> = Arc::new(fga);
    let data = Arc::new(data::DataStore::new(
        pool.clone(),
        authz.clone(),
        Arc::new(NoRefs),
    ));
    let secrets = Arc::new(secrets::SecretStore::new(
        pool.clone(),
        authz.clone(),
        Arc::new(secrets::LocalKeyFileProvider::from_bytes([7u8; 32])),
    ));
    Some(Env {
        pool,
        realm,
        authz,
        fga_url,
        store_id,
        objects: Arc::new(MemStore::default()),
        data,
        secrets,
        engine: Arc::new(ScriptEngine::new().expect("engine")),
        tenant: format!("t-{}", Uuid::new_v4()),
    })
}

/// ゲートウェイを 127.0.0.1 の空いたポートで立て、そこへ能力を送る関数の実行と、利用者が起点の関数の port を返す。
async fn spawn_gateway(
    env: &Env,
    tenant_source: authz::TenantSource,
) -> (String, Arc<GatewayFunctionPort>, Arc<FunctionRunner>) {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let origin = format!("http://{}", listener.local_addr().unwrap());
    let runner = Arc::new(
        FunctionRunner::new(Arc::clone(&env.engine), env.objects.clone(), origin.clone())
            .expect("runner"),
    );
    let port = Arc::new(GatewayFunctionPort {
        runner: Arc::clone(&runner),
        http: reqwest::Client::new(),
        token_endpoint: env.realm.token_endpoint(),
        secrets: Some(Arc::clone(&env.secrets)),
        installations: app_gateway::AppInstallationStore::new(env.pool.clone()),
    });
    let artifacts = Arc::new(artifact::ArtifactStore::new(
        env.pool.clone(),
        env.authz.clone(),
    ));
    let state = app_gateway::GatewayState {
        installations: app_gateway::AppInstallationStore::new(env.pool.clone()),
        keys: Arc::new(env.realm.key_resolver()),
        token_cfg: app_gateway::GatewayTokenConfig {
            audience: "shiki-gateway".into(),
            issuer: env.realm.issuer(),
        },
        authz: env.authz.clone(),
        audit: storage::audit::AuditRecorder::new(env.pool.clone()),
        tenant_source,
        default_org: "acme".into(),
        caps: app_gateway::CapabilityDeps {
            db: env.pool.clone(),
            storage: Arc::new(storage::StorageService::new(
                env.pool.clone(),
                env.objects.clone(),
                env.authz.clone(),
                Duration::from_mins(1),
                Duration::from_mins(1),
                1024 * 1024,
            )),
            data: Arc::clone(&env.data),
            fsms: Arc::new(data::FsmStore::new(artifacts, (*env.data).clone())),
            rag: Arc::new(app_gateway::NoRag),
            notifications: app_gateway::NotificationStore::new(env.pool.clone()),
            llm: None,
            agent: Arc::new(app_gateway::NoAgent),
            ai_daily_cap_usd_micros: 0,
            functions: port.clone(),
        },
    };
    let router = app_gateway::build_gateway_router(state);
    tokio::spawn(async move { axum::serve(listener, router).await.unwrap() });
    (origin, port, runner)
}

/// 同意インストール（テーブル 1 つ・B2・閲覧は役割 sales）を通し、`(app_id, b1, b2)` を返す。登録するクライアントは
/// 先に `clients` に残す（途中で落ちても片付ける）。
async fn install(
    env: &Env,
    installs: &InstallService,
    code: &MiniAppCodeStore,
    clients: &Mutex<Vec<String>>,
) -> (Uuid, String, String) {
    let installer = user_ctx(&env.tenant, "installer");
    let sha = sha256_hex(SCRIPT.as_bytes());
    env.objects.0.lock().unwrap().insert(
        miniapp_bundle_key(&env.tenant, &sha),
        SCRIPT.as_bytes().to_vec(),
    );
    let name = format!("b2-it-{}", Uuid::new_v4().simple());
    let manifest = MiniAppManifest {
        name: name.clone(),
        version: "1.0.0".into(),
        description: "B2 の試し".into(),
        requested_scopes: vec!["data.read".into(), "data.write".into()],
        tools: vec![],
        tables: vec![ManifestTable {
            name: "経費".into(),
            schema: TableSchema {
                fields: vec![FieldDef::new("title", FieldType::Text)],
                status_field: None,
                row_policy: None,
                field_policy: vec![],
                aggregate_min_rows: None,
                fsm_ref: None,
                rag: None,
            },
        }],
        workflows: vec![],
        budget: Budget::default(),
        frontend: None,
        server: Some(ServerSpec {
            code_sha256: Some(sha),
            functions: vec!["count".into(), "remind".into()],
            ..ServerSpec::default()
        }),
        trust_tier: TrustTier::InHouse,
    };
    let app_id = code
        .create(&installer, &manifest, None)
        .await
        .expect("create");
    let (b1, b2) = (format!("app-{app_id}-b1"), format!("app-{app_id}-b2"));
    clients.lock().unwrap().extend([b1.clone(), b2.clone()]);
    code.publish(&installer, app_id, None, None, None)
        .await
        .expect("publish");
    installs
        .install(
            &installer,
            InstallRequest {
                name,
                version: "1.0.0".into(),
                granted_scopes: vec!["data.read".into(), "data.write".into()],
                viewer_roles: vec!["sales".into()],
                editor_roles: vec![],
                submitter_roles: vec![],
            },
            None,
        )
        .await
        .expect("install");
    (app_id, b1, b2)
}

async fn cleanup(env: &Env, clients: &[String]) {
    for c in clients {
        env.realm.delete_client(c).await;
    }
    for sql in [
        "DELETE FROM app_installation WHERE tenant_id = $1",
        "DELETE FROM app_capability_usage WHERE tenant_id = $1",
        "DELETE FROM app_event_subscription WHERE tenant_id = $1",
        "DELETE FROM app_function_schedule WHERE tenant_id = $1",
        "DELETE FROM secret WHERE tenant_id = $1",
        "DELETE FROM registry_entry WHERE tenant_id = $1",
        "DELETE FROM artifact_version WHERE tenant_id = $1",
        "DELETE FROM artifact WHERE tenant_id = $1",
        "DELETE FROM storage_event_outbox WHERE tenant_id = $1",
        "DELETE FROM audit_log WHERE tenant_id = $1",
    ] {
        sqlx::query(sql)
            .bind(&env.tenant)
            .execute(&env.pool)
            .await
            .expect(sql);
    }
    env.data.purge_tenant(&env.tenant).await.expect("purge");
    let deleted = reqwest::Client::new()
        .delete(format!("{}/stores/{}", env.fga_url, env.store_id))
        .send()
        .await
        .expect("delete store");
    assert!(deleted.status().is_success(), "{}", deleted.status());
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn b2_functions_run_as_the_app_on_schedule_and_as_the_user_on_request() {
    let Some(env) = env().await else {
        eprintln!("KEYCLOAK_TEST_URL・STORAGE_TEST_DATABASE_URL・OPENFGA_TEST_URL のどれかが無いためスキップ");
        return;
    };
    let artifacts = Arc::new(artifact::ArtifactStore::new(
        env.pool.clone(),
        env.authz.clone(),
    ));
    let code = MiniAppCodeStore::new(artifacts, Registry::new(env.pool.clone()));
    let code = Arc::new(code);
    let installs = InstallService::new(
        env.pool.clone(),
        Registry::new(env.pool.clone()),
        Arc::clone(&code),
        Arc::clone(&env.data),
        env.authz.clone(),
        Some(env.realm.oauth_client()),
        vec![SHELL_CALLBACK.into()],
    )
    .with_secrets(Some(Arc::clone(&env.secrets)), Some("localhost".into()));
    let clients = Mutex::new(Vec::new());
    let body = async {
        let (app_id, b1, _b2) = install(&env, &installs, &code, &clients).await;
        let ns = authz::Namespace::for_tenant(&env.tenant);
        // alice を役割 sales に入れる（インストールがアプリのテーブルの閲覧を sales に渡した）。
        env.authz
            .write_tuple(&ns.user(ALICE), Relation::Member, &ns.role("sales"))
            .await
            .expect("alice in sales");

        // 自動実行（multi テナント）: B2 の client_credentials のトークンで、アプリの主体として行を足す。
        let (_, multi_port, multi_runner) = spawn_gateway(&env, authz::TenantSource::Claim).await;
        let deps = TriggerDeps {
            db: env.pool.clone(),
            runner: multi_runner,
            port: multi_port,
            claim_hints: storage::event::ClaimHints::default(),
        };
        invoke_service(
            &deps,
            &Subscription {
                tenant_id: env.tenant.clone(),
                org: "acme".into(),
                app_id,
                function: "remind".into(),
            },
            serde_json::json!({ "trigger": "cron" }),
        )
        .await
        .expect("service run");
        let owners: Vec<String> =
            sqlx::query_scalar("SELECT owner FROM data_record WHERE tenant_id = $1")
                .bind(&env.tenant)
                .fetch_all(&env.pool)
                .await
                .unwrap();
        assert_eq!(owners, [app_id.to_string()], "アプリの主体として 1 行足す");

        // 利用者が起点（single テナント）: alice の B1 のトークンを B2 が交換し、関数は alice としてテーブルを読む。
        let (single, _, _) =
            spawn_gateway(&env, authz::TenantSource::Fixed(env.tenant.clone())).await;
        let token = env
            .realm
            .user_token(&b1, SHELL_CALLBACK, "alice", "openid data.read")
            .await
            .expect("alice token");
        let resp = reqwest::Client::new()
            .post(format!("{single}/gw/apps/functions/count/invoke"))
            .bearer_auth(&token)
            .json(&serde_json::json!({}))
            .send()
            .await
            .expect("invoke");
        let status = resp.status();
        let outcome: serde_json::Value = resp.json().await.unwrap_or_default();
        assert!(status.is_success(), "{status} {outcome}");
        assert_eq!(outcome["ok"], true, "{outcome}");
        assert_eq!(outcome["value"]["actor"], "user", "{outcome}");
        assert_eq!(outcome["value"]["tables"], 1, "{outcome}");
        assert_eq!(outcome["value"]["rows"], 1, "{outcome}");

        // alice を役割から外すと、交換しても alice の権限なので読めない（アプリの権限に昇格しない）。
        env.authz
            .delete_tuple(&ns.user(ALICE), Relation::Member, &ns.role("sales"))
            .await
            .expect("alice out of sales");
        let resp = reqwest::Client::new()
            .post(format!("{single}/gw/apps/functions/count/invoke"))
            .bearer_auth(&token)
            .json(&serde_json::json!({}))
            .send()
            .await
            .expect("invoke");
        let outcome: serde_json::Value = resp.json().await.unwrap_or_default();
        assert_eq!(outcome["ok"], false, "{outcome}");
        let _ = installs
            .uninstall(&user_ctx(&env.tenant, "installer"), app_id, None)
            .await;
    };
    let outcome = AssertUnwindSafe(body).catch_unwind().await;
    let clients = clients.into_inner().unwrap();
    cleanup(&env, &clients).await;
    if let Err(panic) = outcome {
        std::panic::resume_unwind(panic);
    }
}
