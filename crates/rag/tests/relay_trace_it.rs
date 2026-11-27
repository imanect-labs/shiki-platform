//! ファイル更新 → outbox → relay → rag_ingest の経路で、書込んだリクエストの trace context が
//! インジェストのジョブまで届くことの結合テスト（`STORAGE_TEST_DATABASE_URL` が必要）。
//!
//! relay は未処理の outbox を全部さらうので、他のテストと混ざらないよう専用の DB を作って消す。

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::print_stderr
)]

use std::time::Duration;

use authz::{AuthContext, Principal};
use opentelemetry::trace::{TraceContextExt, TracerProvider as _};
use opentelemetry_sdk::trace::{InMemorySpanExporter, SdkTracerProvider};
use sqlx::{postgres::PgPoolOptions, PgPool};
use storage::event::{WriteEvent, WriteOp};
use tracing_opentelemetry::OpenTelemetrySpanExt;
use tracing_subscriber::layer::SubscriberExt;
use uuid::Uuid;

/// 専用の DB を作って migration を当てる。返り値: (プール, 管理接続, DB 名)。
async fn isolated_db() -> Option<(PgPool, PgPool, String)> {
    let Ok(db_url) = shiki_testenv::var("STORAGE_TEST_DATABASE_URL") else {
        eprintln!("STORAGE_TEST_DATABASE_URL 未設定のためスキップ");
        return None;
    };
    let admin = PgPoolOptions::new()
        .max_connections(1)
        .connect(&db_url)
        .await
        .expect("Postgres へ接続できること");
    let name = format!("rag_relay_trace_{}", Uuid::new_v4().simple());
    sqlx::query(&format!("CREATE DATABASE {name}"))
        .execute(&admin)
        .await
        .expect("テスト用 DB を作れること");
    let mut url = reqwest::Url::parse(&db_url).expect("DB URL");
    url.set_path(&name);
    let pool = PgPoolOptions::new()
        .max_connections(2)
        .connect(url.as_str())
        .await
        .expect("テスト用 DB へ接続できること");
    if let Err(e) = sqlx::migrate!("../../migrations").run(&pool).await {
        drop_db(pool, admin, &name).await;
        panic!("マイグレーション適用: {e}");
    }
    Some((pool, admin, name))
}

async fn drop_db(pool: PgPool, admin: PgPool, name: &str) {
    let _ = tokio::time::timeout(Duration::from_secs(5), pool.close()).await;
    let _ = sqlx::query(&format!("DROP DATABASE IF EXISTS {name} WITH (FORCE)"))
        .execute(&admin)
        .await;
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

#[tokio::test]
async fn ingest_job_inherits_the_writing_request_context() {
    let Some((pool, admin, name)) = isolated_db().await else {
        return;
    };
    let exporter = InMemorySpanExporter::default();
    let provider = SdkTracerProvider::builder()
        .with_simple_exporter(exporter)
        .build();
    let _guard = tracing::subscriber::set_default(
        tracing_subscriber::registry()
            .with(tracing_opentelemetry::layer().with_tracer(provider.tracer("it"))),
    );

    let tenant = format!("t-{}", Uuid::new_v4().simple());
    let c = ctx(&tenant);

    // ファイルを書込んだリクエスト（API の http_request 相当）の中で outbox へ積む。
    let request = tracing::info_span!("http_request");
    let request_cx = request.context().span().span_context().clone();
    let trace_hex = request_cx.trace_id().to_string();
    {
        let _g = request.enter();
        let mut conn = pool.acquire().await.unwrap();
        storage::event::emit_on(
            &mut conn,
            &c,
            WriteEvent {
                node_id: Uuid::new_v4(),
                version: 1,
                op: WriteOp::Create,
                payload: serde_json::json!({}),
            },
            Some(&trace_hex),
        )
        .await
        .unwrap();
    }

    // relay は別のループ（別の span）で動く。
    let relayed = {
        let loop_span = tracing::info_span!("relay_loop");
        let _g = loop_span.enter();
        rag::pipeline::relay::relay_once(&pool, &rag::RagConfig::default())
            .await
            .unwrap()
    };
    assert_eq!(relayed, 1);

    let mut conn = pool.acquire().await.unwrap();
    let jobs = jobq::claim(&mut conn, rag::RAG_INGEST_QUEUE, Duration::from_mins(1), 10)
        .await
        .unwrap();
    let job_ok = jobs.len() == 1;
    let tp = jobs.first().and_then(|j| j.traceparent.clone());
    drop(conn);
    drop_db(pool, admin, &name).await;

    assert!(job_ok, "取込ジョブが 1 件積まれること");
    let expected = format!("00-{}-{}-01", request_cx.trace_id(), request_cx.span_id());
    assert_eq!(tp.as_deref(), Some(expected.as_str()));
}
