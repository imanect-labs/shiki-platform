//! 生成ワーカーのエンドツーエンド結合テスト（Task 3.5 / 3.11）。
//!
//! `STORAGE_TEST_DATABASE_URL` が設定されている時のみ実行。実 LLM の代わりに決定的 stub
//! プロバイダを使い、**ChatWorker → llm-gateway(stub) → sink(append+projection) → SSE event_stream**
//! の実コード経路を走らせて、送信→ストリーミング→確定→復元購読が通ることを検証する。

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
use chat::{ChatStore, ChatWorker, ContentBlock, StreamEventKind, WorkerConfig};
use futures::stream::StreamExt;
use llm_gateway::{
    GatewayConfig, LlmGateway, ModelCatalog, ModelEntry, ProviderConfig, ProviderKind,
};
use sqlx::{postgres::PgPoolOptions, PgPool};

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

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn worker_generates_streams_and_persists_projection() {
    let Some(pool) = setup().await else { return };
    let tenant = format!("t-{}", uuid::Uuid::new_v4());
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
            sandbox: None,
            artifacts: None,
            web_search: None,
            storage: None,
        },
        WorkerConfig {
            system_prompt: "あなたはアシスタントです。".into(),
            model: Some("m".into()),
            lease_secs: 30,
            max_steps: 4,
            ..Default::default()
        },
    );
    // 生成ワーカーを起動（jobq を消費）。
    worker.spawn(1);

    let c = ctx(&tenant);
    let thread = store.create_thread(&c, "t", false, None).await.unwrap();
    let res = store
        .post_message(&c, thread.id, "hello world", &[], Some(false), false, None)
        .await
        .unwrap();

    // SSE 相当の event_stream を drain し、トークン→done を受け取る。
    let mut rx = store.event_stream(res.run_id, 0);
    let mut text = String::new();
    let mut done = false;
    for _ in 0..500 {
        let next = tokio::time::timeout(Duration::from_secs(15), rx.next())
            .await
            .expect("イベント待ちがタイムアウト（ワーカーが生成しない）");
        let Some(ev) = next else { break };
        match ev.event {
            StreamEventKind::Token { text: t } => text.push_str(&t),
            StreamEventKind::Done { .. } => {
                done = true;
                break;
            }
            StreamEventKind::Error { message } => panic!("生成失敗: {message}"),
            _ => {}
        }
    }
    assert!(done, "done イベントを受け取ること");
    assert!(
        text.contains("hello world"),
        "stub 応答が本文を含むこと: {text:?}"
    );

    // message.content に projection が書き戻されている（接続非依存生成の確定）。
    let msgs = store.get_messages(&c, thread.id, None).await.unwrap();
    let asst = msgs
        .iter()
        .find(|m| m.id == res.assistant_message_id)
        .expect("assistant メッセージ");
    let has_text = asst.content.iter().any(|b| match b {
        ContentBlock::Text { text } => text.contains("hello world"),
        _ => false,
    });
    assert!(has_text, "確定メッセージに本文 projection が残ること");

    // 会計が刻まれている（tenant スコープ・冪等キー）。
    let usage_count: i64 =
        sqlx::query_scalar("SELECT count(*) FROM llm_usage WHERE tenant_id = $1")
            .bind(&tenant)
            .fetch_one(&pool)
            .await
            .unwrap();
    assert!(usage_count >= 1, "llm_usage に会計行が刻まれること");
}

