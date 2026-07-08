//! Phase 5 DoD e2e（Task 5.1/5.4/5.8/5.11）: autonomous run が worker で Autonomous プロファイルとして
//! 動き、`fs_write` が Durable Workspace（StorageService）へ書き込み、書込イベントを発行することを検証する。
//!
//! **専用テストバイナリに分離**する: worker を spawn する他テストと同一バイナリだと、共有 jobq を
//! storage 未配線の worker が claim して自律 run が chat にフォールバックし得るため（バイナリ間は逐次実行）。
//! 実 Postgres＋MinIO が必要（`STORAGE_TEST_DATABASE_URL`＋`STORAGE_TEST_S3_ENDPOINT`）。

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::print_stderr
)]

use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use authz::{
    AuthContext, AuthzClient, AuthzError, Consistency, FgaObject, ObjectType, Principal,
    ReadTupleKey, Relation, Subject,
};
use chat::{ChatStore, ChatWorker, StreamEventKind, WorkerConfig};
use futures::stream::StreamExt;
use llm_gateway::{
    GatewayConfig, LlmGateway, ModelCatalog, ModelEntry, ProviderConfig, ProviderKind,
};
use sqlx::{postgres::PgPoolOptions, PgPool};
use storage::{object_store::S3Config, ObjectStore, S3ObjectStore, StorageService};

/// 全許可のモック authz（DB/ストレージロジックの検証に集中する）。
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

async fn setup() -> Option<PgPool> {
    let Ok(db_url) = std::env::var("STORAGE_TEST_DATABASE_URL") else {
        eprintln!("STORAGE_TEST_DATABASE_URL 未設定のためスキップ");
        return None;
    };
    let pool = PgPoolOptions::new()
        .max_connections(5)
        .connect(&db_url)
        .await
        .expect("Postgres へ接続できること");
    sqlx::migrate!("../../migrations")
        .run(&pool)
        .await
        .expect("マイグレーション適用");
    Some(pool)
}

fn ctx(tenant: &str) -> AuthContext {
    AuthContext::new(
        Principal {
            kind: authz::PrincipalKind::User,
            id: "alice".into(),
            email: None,
            groups: vec![],
            roles: vec![],
            tenant_id: Some(tenant.into()),
        },
        "acme".into(),
        tenant.into(),
    )
}

fn stub_gateway(pool: PgPool) -> LlmGateway {
    let config = GatewayConfig {
        provider: ProviderConfig {
            kind: ProviderKind::Stub,
            base_url: None,
            api_key: None,
            timeout_secs: 120,
        },
        catalog: ModelCatalog {
            default_model: "m".into(),
            models: vec![ModelEntry {
                id: "m".into(),
                real_id: None,
                prompt_price_micros_per_mtok: 0,
                completion_price_micros_per_mtok: 0,
            }],
        },
        langfuse: None,
    };
    LlmGateway::build(pool, reqwest::Client::new(), config).expect("gateway")
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn autonomous_run_writes_workspace_file_e2e() {
    let Some(pool) = setup().await else { return };
    let tenant = format!("t-{}", uuid::Uuid::new_v4());

    // StorageService（AllowAll authz＋MinIO）。DoD の「書込→再索引経路」の書込側チョークポイント。
    let s3_endpoint = std::env::var("STORAGE_TEST_S3_ENDPOINT")
        .unwrap_or_else(|_| "http://localhost:9000".into());
    let s3 = S3Config {
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
    let object_store: Arc<dyn ObjectStore> = Arc::new(S3ObjectStore::new(&s3));
    object_store.ensure_bucket().await.expect("バケット準備");
    let storage = Arc::new(StorageService::new(
        pool.clone(),
        object_store,
        Arc::new(AllowAll),
        Duration::from_mins(5),
        Duration::from_mins(15),
        5 * 1024 * 1024 * 1024,
    ));

    let store = ChatStore::connect(pool.clone(), Arc::new(AllowAll), None)
        .await
        .unwrap();
    let gateway = stub_gateway(pool.clone());
    let worker = ChatWorker::new(
        pool.clone(),
        store.clone(),
        chat::WorkerDeps {
            gateway,
            search: None,
            sandbox: None, // fs_write は純ストレージ（shell 不要）。
            artifacts: None,
            web_search: None,
            storage: Some(storage.clone()),
        },
        WorkerConfig {
            system_prompt: "あなたは自律アシスタントです。".into(),
            model: Some("m".into()),
            lease_secs: 30,
            max_steps: 4,
            ..Default::default()
        },
    );
    worker.spawn(1);

    let c = ctx(&tenant);
    let thread = store.create_thread(&c, "自律", false, None).await.unwrap();
    // autonomous=true で投入 → stub が fs_write を呼ぶ（fswrite: プレフィックス・ApprovalPolicy=auto で承認不要）。
    let res = store
        .post_message(&c, thread.id, "fswrite: hello-e2e", &[], None, true, None)
        .await
        .unwrap();

    // 完了まで drain（fs_write の tool_call が流れる）。
    let mut rx = store.event_stream(res.run_id, 0);
    let mut done = false;
    let mut saw_fs_write = false;
    for _ in 0..500 {
        let next = tokio::time::timeout(Duration::from_secs(20), rx.next())
            .await
            .expect("イベント待ちタイムアウト");
        let Some(ev) = next else { break };
        match ev.event {
            StreamEventKind::ToolCall { name, .. } if name == "fs_write" => saw_fs_write = true,
            StreamEventKind::Done { .. } => {
                done = true;
                break;
            }
            StreamEventKind::Error { message } => panic!("生成失敗: {message}"),
            _ => {}
        }
    }
    assert!(done, "autonomous run が完了する");
    assert!(
        saw_fs_write,
        "fs_write ツール呼び出しが流れる（Autonomous プロファイル＋フルツール）"
    );

    // ワークスペースフォルダが lazy 作成され、fs_write がそこへ書き込んでいる（Durable Workspace）。
    let folder = store
        .workspace_folder_id(thread.id, &c.tenant_id)
        .await
        .unwrap()
        .expect("ワークスペースフォルダが作成される");
    let node = storage
        .resolve_child_file(&c, folder, "agent-note.txt", None)
        .await
        .unwrap()
        .expect("agent-note.txt が書き込まれる");
    let (_, bytes) = storage.read_file_internal(&c, node, None).await.unwrap();
    assert_eq!(bytes, b"hello-e2e", "書込内容がワークスペースに永続する");
    // 書込イベント（→自動再索引）が発行されている（Task 5.8）。
    let outbox: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM storage_event_outbox WHERE node_id = $1 AND op = 'create'",
    )
    .bind(node)
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!(outbox, 1, "書込イベントが再索引経路へ発行される");
}
