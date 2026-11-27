//! LLM 呼び出しの計装（GenAI semconv の span）の結合テスト（実 Postgres・stub プロバイダ）。
//!
//! `STORAGE_TEST_DATABASE_URL` が設定されている時のみ実行（未設定はスキップ）。
//! - `LlmGateway::stream` の span が呼び出し元の span の子として出る（＝同じ trace_id に乗る）。
//! - 振り先（実モデル ID・プロバイダ）と使用トークンが刻まれ、ストリームの終わりまで閉じない。
//! - 拒否（許可外のモデル等）は span に失敗として残る。本文は載らない。

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::print_stderr
)]

#[path = "../src/telemetry/capture.rs"]
mod capture;

use authz::{AuthContext, Principal};
use futures::StreamExt;
use llm_gateway::{
    GatewayConfig, GenerateRequest, LlmError, LlmGateway, Message, ModelCatalog, ModelEntry, Role,
};
use sqlx::postgres::PgPoolOptions;

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

async fn gateway() -> Option<LlmGateway> {
    let Ok(db_url) = shiki_testenv::var("STORAGE_TEST_DATABASE_URL") else {
        eprintln!("STORAGE_TEST_DATABASE_URL 未設定のためスキップ");
        return None;
    };
    let pool = PgPoolOptions::new()
        .max_connections(2)
        .connect(&db_url)
        .await
        .expect("DB 接続");
    sqlx::migrate!("../../migrations").run(&pool).await.unwrap();
    let config = GatewayConfig {
        catalog: ModelCatalog {
            default_model: "fast".into(),
            models: vec![ModelEntry {
                id: "fast".into(),
                real_id: Some("stub-real-1".into()),
                ..Default::default()
            }],
        },
        ..Default::default()
    };
    Some(LlmGateway::build(pool, reqwest::Client::new(), config).expect("gateway"))
}

#[tokio::test]
async fn stream_span_is_child_of_caller_and_closes_after_stream() {
    let Some(gw) = gateway().await else { return };
    let tenant = format!("t-{}", uuid::Uuid::new_v4());
    let cap = capture::Captured::default();
    let _g = cap.guard();

    let parent = tracing::info_span!("chat.turn");
    let mut req = GenerateRequest::new(vec![Message::text(Role::User, "社外秘の質問")]);
    req.model = Some("fast".into());
    let mut stream = {
        use tracing::Instrument;
        gw.stream(&ctx(&tenant), req)
            .instrument(parent.clone())
            .await
            .unwrap()
    };
    drop(parent);
    let mut items = 0;
    while let Some(d) = stream.next().await {
        d.unwrap();
        items += 1;
        assert!(
            !cap.span("gen_ai.chat").closed,
            "ストリームの途中で閉じない"
        );
    }
    assert!(items > 0);
    drop(stream);

    let s = cap.span("gen_ai.chat");
    assert!(s.closed);
    assert_eq!(s.parent.as_deref(), Some("chat.turn"));
    assert_eq!(s.field("shiki.llm.model"), "fast");
    assert_eq!(s.field("gen_ai.request.model"), "stub-real-1");
    assert_eq!(s.field("gen_ai.provider.name"), "stub");
    assert!(s.fields.contains_key("gen_ai.usage.input_tokens"));
    assert!(s.fields.contains_key("gen_ai.usage.output_tokens"));
    assert!(s.fields.contains_key("gen_ai.server.time_to_first_token"));
    assert!(!s.fields.contains_key("otel.status_code"));
    assert!(
        !cap.all_values().iter().any(|v| v.contains("社外秘")),
        "プロンプト本文は span に載せない"
    );
}

#[tokio::test]
async fn rejected_call_is_recorded_as_error() {
    let Some(gw) = gateway().await else { return };
    let tenant = format!("t-{}", uuid::Uuid::new_v4());
    let cap = capture::Captured::default();
    let _g = cap.guard();
    let mut req = GenerateRequest::new(vec![Message::text(Role::User, "hi")]);
    req.model = Some("missing".into());
    assert!(matches!(
        gw.stream(&ctx(&tenant), req).await,
        Err(LlmError::Config(_))
    ));
    let s = cap.span("gen_ai.chat");
    assert!(s.closed);
    assert_eq!(s.field("otel.status_code"), "ERROR");
    assert_eq!(s.field("error.type"), "config");
}
