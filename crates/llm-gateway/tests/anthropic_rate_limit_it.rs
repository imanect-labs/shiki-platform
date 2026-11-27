//! Anthropic の 429 の送り直し（`retry-after` の指示どおりに待つ）の結線テスト（ローカル axum モック）。
//!
//! - アダプタ直叩き（DB 非依存）: `retry-after` が 20 秒以下なら待って 2 回まで送り直し、長い待ち・指示の無いもの・
//!   3 度目の 429・締め切りを越える待ちは送り直さずに [`LlmError::RateLimited`] にする。
//! - ゲートウェイ経由（実 Postgres・`STORAGE_TEST_DATABASE_URL` が無いとスキップ）: ゲートウェイ自身の利用上限で
//!   断った生成はプロバイダへ 1 度も送らず、プロバイダの 429 にもゲートウェイの送り直しを重ねない。

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::print_stderr
)]

use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use authz::{AuthContext, Principal};
use axum::http::StatusCode;
use axum::response::IntoResponse;
use axum::Router;
use futures::stream::StreamExt;

use llm_gateway::providers::anthropic::AnthropicProvider;
use llm_gateway::{
    GatewayConfig, GenerateRequest, GenerationRecord, LlmError, LlmGateway, LlmProvider,
    LlmTimeouts, Message, ModelCatalog, ModelEntry, ProviderConfig, ProviderKind, Role,
    StreamDelta, Usage, UsageLimitsConfig, UsageSource,
};

const SSE_OK: &str = concat!(
    "data: {\"type\":\"message_start\",\"message\":{\"usage\":{\"input_tokens\":3}}}\n\n",
    "data: {\"type\":\"content_block_delta\",\"index\":0,\"delta\":{\"type\":\"text_delta\",\"text\":\"ok\"}}\n\n",
    "data: {\"type\":\"message_delta\",\"delta\":{\"stop_reason\":\"end_turn\"},\"usage\":{\"output_tokens\":1}}\n\n",
    "data: {\"type\":\"message_stop\"}\n\n",
);

const LIMITED: &str = r#"{"type":"error","error":{"type":"rate_limit_error","message":"Number of request tokens has exceeded your per-minute rate limit"}}"#;

/// 最初の `limited.len()` 回は 429（要素が `Some(v)` なら `retry-after: v` を付ける）、その後は 200 の SSE を返す
/// モックを立てる。受けた要求の数を返す。
async fn serve(limited: Vec<Option<&'static str>>) -> (String, Arc<Mutex<usize>>) {
    let calls = Arc::new(Mutex::new(0usize));
    let seen = calls.clone();
    let app = Router::new().fallback(move || {
        let seen = seen.clone();
        let limited = limited.clone();
        async move {
            let n = {
                let mut c = seen.lock().unwrap();
                *c += 1;
                *c - 1
            };
            match limited.get(n) {
                Some(retry_after) => {
                    let mut resp = (StatusCode::TOO_MANY_REQUESTS, LIMITED).into_response();
                    if let Some(v) = retry_after {
                        resp.headers_mut().insert("retry-after", v.parse().unwrap());
                    }
                    resp
                }
                None => (
                    StatusCode::OK,
                    [("content-type", "text/event-stream")],
                    SSE_OK,
                )
                    .into_response(),
            }
        }
    });
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    (format!("http://{addr}"), calls)
}

fn req() -> GenerateRequest {
    GenerateRequest::new(vec![Message::text(Role::User, "hi")])
}

/// ストリームを最後まで読み、`Done` で終わったか。
async fn ends_done(mut s: llm_gateway::DeltaStream) -> bool {
    let mut last = None;
    while let Some(item) = s.next().await {
        last = Some(item.expect("delta should be Ok"));
    }
    matches!(last, Some(StreamDelta::Done { .. }))
}

/// `retry-after` が短ければ、その秒数だけ待って 2 回まで送り直し、通れば普通に流す。
#[tokio::test]
async fn short_retry_after_is_waited_out_up_to_twice() {
    let (base, calls) = serve(vec![Some("0"), Some("1")]).await;
    let provider = AnthropicProvider::new(reqwest::Client::new(), base, "k", "claude-x");
    let started = Instant::now();
    let s = provider.stream(&req()).await.expect("2 回の送り直しで通る");
    assert!(ends_done(s).await);
    assert_eq!(*calls.lock().unwrap(), 3);
    assert!(
        started.elapsed() >= Duration::from_secs(1),
        "retry-after の秒数だけ待つ: {:?}",
        started.elapsed()
    );
}

/// 3 度目の 429・長い待ち（20 秒超）・待ちの指示の無い 429 は、送り直さずに上限の誤りにする。
#[tokio::test]
async fn rate_limit_stays_when_retries_run_out_or_the_wait_is_long_or_unknown() {
    for (limited, want_calls) in [
        (vec![Some("0"); 3], 3),
        (vec![Some("21")], 1),
        (vec![None], 1),
        (vec![Some("Wed, 21 Oct 2026 07:28:00 GMT")], 1),
    ] {
        let label = format!("{limited:?}");
        let (base, calls) = serve(limited).await;
        let provider = AnthropicProvider::new(reqwest::Client::new(), base, "k", "claude-x");
        let started = Instant::now();
        match provider.stream(&req()).await {
            Err(LlmError::RateLimited(msg)) => {
                assert!(msg.starts_with("利用上限に達しました"), "{label}: {msg}");
                assert!(!msg.contains("per-minute"), "本文を出さない: {msg}");
            }
            Err(e) => panic!("{label}: RateLimited を期待: {e:?}"),
            Ok(_) => panic!("{label}: RateLimited を期待したが流れた"),
        }
        assert_eq!(*calls.lock().unwrap(), want_calls, "{label}");
        assert!(
            started.elapsed() < Duration::from_secs(1),
            "{label}: 待たない"
        );
    }
}

/// 生成全体の締め切りより長い待ちはしない（すぐ上限の誤りとして返す）。
#[tokio::test]
async fn retry_after_beyond_the_total_deadline_is_not_waited() {
    let (base, calls) = serve(vec![Some("2")]).await;
    let provider = AnthropicProvider::new(reqwest::Client::new(), base, "k", "claude-x")
        .with_timeouts(LlmTimeouts {
            first_response: Duration::from_secs(5),
            idle: Duration::from_secs(5),
            total: Duration::from_millis(500),
        });
    let started = Instant::now();
    assert!(matches!(
        provider.stream(&req()).await,
        Err(LlmError::RateLimited(_))
    ));
    assert_eq!(*calls.lock().unwrap(), 1);
    assert!(started.elapsed() < Duration::from_secs(1));
}

// --- ゲートウェイ経由（実 Postgres） ---

async fn pool() -> Option<sqlx::PgPool> {
    let Ok(db_url) = shiki_testenv::var("STORAGE_TEST_DATABASE_URL") else {
        eprintln!("STORAGE_TEST_DATABASE_URL 未設定のためスキップ");
        return None;
    };
    let pool = sqlx::postgres::PgPoolOptions::new()
        .max_connections(5)
        .connect(&db_url)
        .await
        .expect("DB 接続");
    sqlx::migrate!("../../migrations")
        .run(&pool)
        .await
        .expect("マイグレーション適用");
    Some(pool)
}

fn ctx(tenant: &str, user: &str) -> AuthContext {
    AuthContext::new(
        Principal {
            kind: authz::PrincipalKind::User,
            id: user.into(),
            email: None,
            groups: vec![],
            roles: vec![],
            tenant_id: Some(tenant.into()),
        },
        "acme".into(),
        authz::TenantId::parse(tenant).unwrap(),
    )
}

/// 既定プロバイダを Anthropic（モック）にしたゲートウェイ。モデル `claude-x` は 1 Mtok あたり $1、
/// ユーザーごとの上限は $1。
fn gateway(pool: sqlx::PgPool, base: String) -> LlmGateway {
    let config = GatewayConfig {
        provider: ProviderConfig {
            kind: ProviderKind::Anthropic,
            base_url: Some(base),
            api_key: Some("k".into()),
            ..Default::default()
        },
        catalog: ModelCatalog {
            default_model: "claude-x".into(),
            models: vec![ModelEntry {
                id: "claude-x".into(),
                prompt_price_micros_per_mtok: 1_000_000,
                ..Default::default()
            }],
        },
        limits: UsageLimitsConfig {
            enabled: true,
            per_user_usd_micros: Some(1_000_000),
            providers: vec![],
        },
        ..Default::default()
    };
    LlmGateway::build(pool, reqwest::Client::new(), config).expect("gateway")
}

/// ゲートウェイの利用上限で断った生成は、プロバイダへ 1 度も送らない（429 の送り直しの対象にならない）。
#[tokio::test]
async fn gateway_usage_limit_is_never_sent_to_the_provider() {
    let Some(pool) = pool().await else { return };
    let (base, calls) = serve(vec![]).await;
    let gw = gateway(pool, base);
    let tenant = format!("t-{}", uuid::Uuid::new_v4());
    let alice = ctx(&tenant, "alice");
    gw.record_generation(
        &alice,
        &GenerationRecord {
            idempotency_key: uuid::Uuid::new_v4().to_string(),
            model: "claude-x".into(),
            usage: Usage {
                prompt_tokens: 1_000_000, // $1（上限ちょうど）
                ..Default::default()
            },
            trace_id: None,
            input_preview: String::new(),
            output_preview: String::new(),
            app_id: None,
            source: UsageSource::Chat,
            first_in_trace: true,
            started_at: None,
        },
    )
    .await;
    match gw.stream(&alice, req()).await {
        Err(LlmError::RateLimited(msg)) => assert!(msg.contains("$1.00"), "{msg}"),
        Err(e) => panic!("RateLimited を期待: {e:?}"),
        Ok(_) => panic!("上限に達したら流さない"),
    }
    assert_eq!(*calls.lock().unwrap(), 0, "プロバイダへ送らない");
}

/// プロバイダの 429 の送り直しはアダプタの 2 回だけで、ゲートウェイの送り直し（一時障害向け）を重ねない。
/// 上限の内なら、送り直しで通った生成はゲートウェイからも普通に流れる。
#[tokio::test]
async fn gateway_adds_no_retries_on_top_of_the_adapter() {
    let Some(pool) = pool().await else { return };
    let tenant = format!("t-{}", uuid::Uuid::new_v4());
    let alice = ctx(&tenant, "alice");

    let (base, calls) = serve(vec![Some("0"); 3]).await;
    let gw = gateway(pool.clone(), base);
    assert!(matches!(
        gw.stream(&alice, req()).await,
        Err(LlmError::RateLimited(_))
    ));
    assert_eq!(*calls.lock().unwrap(), 3, "最初の 1 回＋送り直し 2 回だけ");

    let (base, calls) = serve(vec![Some("0"); 2]).await;
    let gw = gateway(pool, base);
    let s = gw.stream(&alice, req()).await.expect("送り直しで通る");
    assert!(ends_done(s).await);
    assert_eq!(*calls.lock().unwrap(), 3);
}
