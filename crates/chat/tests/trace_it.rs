//! チャット生成ジョブのトレース連携の結合テスト（`STORAGE_TEST_DATABASE_URL` が必要）。
//!
//! 発話を受けたリクエストの span の中で `post_message` → 生成ワーカーが claim した生成ジョブの
//! span が、そのリクエスト span の**子**として同じトレースに出ることを確かめる。
//!
//! ワーカーは別タスクで走るので subscriber はプロセス全体に入れる。このバイナリにはテストを
//! 1 つだけ置く（他のテストの span を混ぜない）。

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
use futures::FutureExt;
use llm_gateway::{
    GatewayConfig, LlmGateway, ModelCatalog, ModelEntry, ProviderConfig, ProviderKind,
};
use opentelemetry::trace::{TraceContextExt, TracerProvider as _};
use opentelemetry_sdk::trace::{InMemorySpanExporter, SdkTracerProvider};
use sqlx::{postgres::PgPoolOptions, PgPool};
use tracing::Instrument;
use tracing_opentelemetry::OpenTelemetrySpanExt;
use tracing_subscriber::layer::SubscriberExt;

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

/// このテスト専用のデータベースを作って migration を当てる。
///
/// 生成ジョブの待ち行列は全テストで共有なので、同じ DB では他のテスト（並列に走る別プロセス）の
/// ワーカーがジョブを拾い、自分の gateway（stub）で生成を成功させてしまう。
/// 返り値: (テスト用 DB のプール, 消すための管理接続, DB 名)。
async fn isolated_setup(prefix: &str) -> Option<(PgPool, PgPool, String)> {
    let Ok(db_url) = shiki_testenv::var("STORAGE_TEST_DATABASE_URL") else {
        eprintln!("STORAGE_TEST_DATABASE_URL 未設定のためスキップ");
        return None;
    };
    let admin = PgPoolOptions::new()
        .max_connections(1)
        .connect(&db_url)
        .await
        .expect("Postgres へ接続できること");
    let name = format!("{prefix}_{}", uuid::Uuid::new_v4().simple());
    sqlx::query(&format!("CREATE DATABASE {name}"))
        .execute(&admin)
        .await
        .expect("テスト用 DB を作れること");
    let mut url = reqwest::Url::parse(&db_url).expect("DB URL");
    url.set_path(&name);
    let pool = PgPoolOptions::new()
        .max_connections(5)
        .connect(url.as_str())
        .await
        .expect("テスト用 DB へ接続できること");
    if let Err(e) = sqlx::migrate!("../../migrations").run(&pool).await {
        drop_isolated(pool, admin, &name).await;
        panic!("マイグレーション適用: {e}");
    }
    Some((pool, admin, name))
}

/// テスト用 DB を消す（共有 Postgres に溜めない）。ワーカーの接続が残っていても FORCE で切る。
async fn drop_isolated(pool: PgPool, admin: PgPool, name: &str) {
    // ワーカーのタスクが接続を返さないことがあるので、close は長く待たない（残りは FORCE で切る）。
    let _ = tokio::time::timeout(Duration::from_secs(5), pool.close()).await;
    sqlx::query(&format!("DROP DATABASE IF EXISTS {name} WITH (FORCE)"))
        .execute(&admin)
        .await
        .expect("テスト用 DB を消せること");
    admin.close().await;
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
        authz::TenantId::parse(tenant).unwrap(),
    )
}

fn stub_gateway(pool: PgPool) -> LlmGateway {
    let config = GatewayConfig {
        provider: ProviderConfig {
            kind: ProviderKind::Stub,
            base_url: None,
            api_key: None,
            user_agent: None,
            session_header: None,
            ..Default::default()
        },
        catalog: ModelCatalog {
            default_model: "m".into(),
            models: vec![ModelEntry {
                id: "m".into(),
                real_id: None,
                prompt_price_micros_per_mtok: 0,
                completion_price_micros_per_mtok: 0,
                context_window_tokens: None,
                ..Default::default()
            }],
        },
        langfuse: None,
        ..Default::default()
    };
    LlmGateway::build(pool, reqwest::Client::new(), config).expect("gateway")
}

fn test_deps(gateway: LlmGateway) -> chat::WorkerDeps {
    chat::WorkerDeps {
        gateway,
        search: None,
        sandbox: None,
        artifacts: None,
        web_search: None,
        parser: None,
        storage: None,
        workflow_resolver: None,
        skill_artifacts: None,
        skill_catalog: None,
        workflow_store: None,
        workflow_catalog: None,
        collab: None,
        tabular: None,
        office: None,
        office_live: None,
        office_creator: None,
        office_engine: None,
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn generation_job_span_is_a_child_of_the_request_span() {
    let Some((pool, admin, db_name)) = isolated_setup("chat_trace").await else {
        return;
    };
    // アサーションが落ちてもテスト用 DB は消す（共有 Postgres に溜めない）。
    let outcome = std::panic::AssertUnwindSafe(scenario(pool.clone()))
        .catch_unwind()
        .await;
    drop_isolated(pool, admin, &db_name).await;
    if let Err(panic) = outcome {
        std::panic::resume_unwind(panic);
    }
}

async fn scenario(pool: PgPool) {
    let exporter = InMemorySpanExporter::default();
    let provider = SdkTracerProvider::builder()
        .with_simple_exporter(exporter.clone())
        .build();
    tracing::subscriber::set_global_default(
        tracing_subscriber::registry()
            .with(tracing_opentelemetry::layer().with_tracer(provider.tracer("it"))),
    )
    .expect("このバイナリで最初の subscriber");

    let tenant = format!("t-{}", uuid::Uuid::new_v4());
    let store = ChatStore::connect(pool.clone(), Arc::new(AllowAll), None)
        .await
        .unwrap();
    let worker = ChatWorker::new(
        pool.clone(),
        store.clone(),
        test_deps(stub_gateway(pool.clone())),
        WorkerConfig {
            system_prompt: "あなたはアシスタントです。".into(),
            model: Some("m".into()),
            lease_secs: 120,
            max_steps: 4,
            ..Default::default()
        },
    );
    worker.spawn(1);

    let c = ctx(&tenant);
    let thread = store
        .create_thread(&c, "t", false, None, None)
        .await
        .unwrap();

    // API の `http_request` span に相当する span の中で発話する（trace_id も API と同じく渡す）。
    let request = tracing::info_span!("http_request");
    let request_cx = request.context().span().span_context().clone();
    let trace_hex = request_cx.trace_id().to_string();
    let res = store
        .post_message(
            &c,
            thread.id,
            "hello trace",
            &[],
            None,
            Some(false),
            false,
            &[],
            Some(&trace_hex),
        )
        .instrument(request.clone())
        .await
        .unwrap();
    drop(request);

    let mut rx = store.event_stream(c.tenant_id(), res.run_id, 0);
    let mut done = false;
    for _ in 0..500 {
        let next = tokio::time::timeout(Duration::from_secs(15), rx.next())
            .await
            .expect("イベント待ちがタイムアウト（ワーカーが生成しない）");
        let Some(ev) = next else { break };
        match ev.event {
            StreamEventKind::Done { .. } => {
                done = true;
                break;
            }
            StreamEventKind::Error { message } => panic!("生成失敗: {message}"),
            _ => {}
        }
    }
    assert!(done, "done イベントを受け取ること");

    // ジョブの span は ack の後に閉じる。閉じて export されるまで待つ。
    let mut job_span = None;
    for _ in 0..100 {
        provider.force_flush().unwrap();
        job_span = exporter
            .get_finished_spans()
            .unwrap()
            .into_iter()
            .find(|s| s.name == "job chat_generation");
        if job_span.is_some() {
            break;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    let job_span = job_span.expect("生成ジョブの span が出ること");
    assert_eq!(job_span.span_context.trace_id(), request_cx.trace_id());
    assert_eq!(job_span.parent_span_id, request_cx.span_id());
    let attr = |k: &str| {
        job_span
            .attributes
            .iter()
            .find(|kv| kv.key.as_str() == k)
            .map(|kv| kv.value.to_string())
    };
    assert_eq!(attr("job.queue").as_deref(), Some("chat_generation"));
    assert_eq!(attr("tenant_id").as_deref(), Some(tenant.as_str()));
    assert_eq!(attr("job.attempts").as_deref(), Some("1"));
    assert!(attr("job.wait_ms").is_some());
}
