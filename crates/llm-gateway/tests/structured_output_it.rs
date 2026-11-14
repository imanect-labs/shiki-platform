//! 構造化出力（GAP-13）の結合テスト。
//!
//! - アダプタ（127.0.0.1 の axum モック・DB 非依存）: Anthropic の `output_config` で effort と format が
//!   併合されて届くこと・拒否の停止理由・スキーマを拒んだ 400 を本文を出さない定型文にすること。
//!   OpenAI 互換の `response_format`・拒否（`delta.refusal`・`content_filter`）。
//! - gateway（`STORAGE_TEST_DATABASE_URL` が無ければスキップ）: カタログのモードに合わせて、プロバイダへ
//!   届く形（`response_format` か system の指示か）が変わること。行は作らない（組織の設定を読むだけ）。
//! - 実エンドポイント（`ANTHROPIC_TEST_URL` が無ければスキップ）: Anthropic の構造化出力が実際に通る
//!   こと・任意項目の上限（24）を超えたスキーマが定型文の誤りになること。
//! - 項目の並び（BUG-123・D-103）: 届いた本文の JSON の文字列で、出力の形の項目が作者の並び（`x-order`）で
//!   書かれていること（Anthropic・OpenAI 互換・OpenAI 互換の口で繋ぐ Gemini API。制約付きデコードと指示のモード）。

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::print_stderr
)]

use std::sync::{Arc, Mutex};

use axum::http::StatusCode;
use axum::response::IntoResponse;
use axum::Router;
use futures::stream::StreamExt;
use serde_json::{json, Value};

use llm_gateway::model::{
    Effort, GenerateRequest, Message, ResponseFormat, Role, StopReason, StreamDelta,
};
use llm_gateway::provider::{LlmError, LlmProvider};
use llm_gateway::providers::{anthropic::AnthropicProvider, openai::OpenAiProvider};
use llm_gateway::structured::{apply_mode, is_schema_rejection};
use llm_gateway::{GatewayConfig, LlmGateway, ModelCatalog, ModelEntry, StructuredOutputMode};

type SeenBody = Arc<Mutex<Option<Value>>>;

/// 固定ステータス＋固定ボディを返し、受けたリクエストボディ（JSON）を記録するモック。
async fn spawn_mock(status: StatusCode, body: &'static str) -> (String, SeenBody) {
    let seen: SeenBody = Arc::new(Mutex::new(None));
    let rec = seen.clone();
    let app = Router::new().fallback(move |req: String| {
        let rec = rec.clone();
        async move {
            *rec.lock().unwrap() = serde_json::from_str(&req).ok();
            (status, [("content-type", "text/event-stream")], body).into_response()
        }
    });
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    });
    (format!("http://{addr}"), seen)
}

type SeenRaw = Arc<Mutex<Option<String>>>;

/// [`spawn_mock`] と同じで、受けたリクエストボディを文字列のまま記録する（キーの並びを見るため）。
async fn spawn_raw_mock(body: &'static str) -> (String, SeenRaw) {
    let seen: SeenRaw = Arc::new(Mutex::new(None));
    let rec = seen.clone();
    let app = Router::new().fallback(move |req: String| {
        let rec = rec.clone();
        async move {
            *rec.lock().unwrap() = Some(req);
            (
                StatusCode::OK,
                [("content-type", "text/event-stream")],
                body,
            )
                .into_response()
        }
    });
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    });
    (format!("http://{addr}"), seen)
}

fn triage_schema() -> Value {
    json!({
        "type": "object",
        "required": ["category", "confidence"],
        "properties": {
            "category": { "type": "string", "enum": ["billing", "bug", "howto", "other"] },
            "confidence": { "type": "integer", "minimum": 0, "maximum": 100 },
            "summary": { "type": "string", "maxLength": 80 },
        },
    })
}

fn structured(user: &str) -> GenerateRequest {
    let mut req = GenerateRequest::new(vec![Message::text(Role::User, user)]);
    req.response_format = Some(ResponseFormat::JsonSchema {
        name: "triage".into(),
        schema: triage_schema(),
    });
    req
}

async fn drain(provider: &dyn LlmProvider, req: &GenerateRequest) -> Vec<StreamDelta> {
    let mut stream = provider.stream(req).await.expect("stream should start");
    let mut out = Vec::new();
    while let Some(item) = stream.next().await {
        out.push(item.expect("delta should be Ok"));
    }
    out
}

fn text_of(deltas: &[StreamDelta]) -> String {
    deltas
        .iter()
        .filter_map(|d| match d {
            StreamDelta::TextDelta { text } => Some(text.as_str()),
            _ => None,
        })
        .collect()
}

fn stop_of(deltas: &[StreamDelta]) -> Option<StopReason> {
    deltas.iter().find_map(|d| match d {
        StreamDelta::Done { stop_reason, .. } => Some(*stop_reason),
        _ => None,
    })
}

const ANTHROPIC_REFUSAL: &str = concat!(
    "data: {\"type\":\"message_start\",\"message\":{\"usage\":{\"input_tokens\":9}}}\n\n",
    "data: {\"type\":\"content_block_delta\",\"index\":0,\"delta\":{\"type\":\"text_delta\",\"text\":\"{\\\"cat\"}}\n\n",
    "data: {\"type\":\"message_delta\",\"delta\":{\"stop_reason\":\"refusal\"},\"usage\":{\"output_tokens\":3}}\n\n",
    "data: {\"type\":\"message_stop\"}\n\n",
);

/// Anthropic が任意項目の上限で返す実際の 400（2026-10-27 に Haiku 5.5 で確かめた文言）。
const ANTHROPIC_TOO_MANY_OPTIONAL: &str = r#"{"type":"error","error":{"type":"invalid_request_error","message":"Schemas contains too many optional parameters (26), which would make grammar compilation inefficient. Reduce the number of optional parameters in your tool schemas (limit: 24)."},"request_id":"req_011secret"}"#;

#[tokio::test]
async fn anthropic_merges_effort_and_format_and_maps_refusal() {
    let (base, seen) = spawn_mock(StatusCode::OK, ANTHROPIC_REFUSAL).await;
    let provider = AnthropicProvider::new(reqwest::Client::new(), base, "k", "claude-haiku-5-5");
    let mut req = structured("請求の件です");
    req.effort = Some(Effort::Low);
    req.temperature = Some(0.0);
    let deltas = drain(&provider, &req).await;

    let body = seen.lock().unwrap().clone().unwrap();
    assert_eq!(body["output_config"]["effort"], "low");
    let format = &body["output_config"]["format"];
    assert_eq!(format["type"], "json_schema");
    assert_eq!(format["schema"]["additionalProperties"], false);
    assert!(format["schema"]["properties"]["confidence"]
        .get("minimum")
        .is_none());
    assert_eq!(
        format["schema"]["properties"]["confidence"]["description"],
        "制約: 0 以上、100 以下"
    );
    assert!(body.get("temperature").is_none());
    assert_eq!(stop_of(&deltas), Some(StopReason::Refusal));
}

#[tokio::test]
async fn anthropic_schema_rejection_hides_the_provider_body() {
    let (base, _) = spawn_mock(StatusCode::BAD_REQUEST, ANTHROPIC_TOO_MANY_OPTIONAL).await;
    let provider = AnthropicProvider::new(reqwest::Client::new(), base, "k", "claude-haiku-5-5");
    let Err(e) = provider.stream(&structured("x")).await else {
        panic!("expected an error")
    };
    assert!(is_schema_rejection(&e), "{e:?}");
    let msg = e.to_string();
    assert!(
        !msg.contains("req_011secret") && !msg.contains("optional parameters"),
        "{msg}"
    );

    // 形を求めていない要求の 400 は従来どおり（構造化出力の誤りにしない）。
    let plain = GenerateRequest::new(vec![Message::text(Role::User, "x")]);
    let Err(e) = provider.stream(&plain).await else {
        panic!("expected an error")
    };
    assert!(matches!(e, LlmError::BadRequest(_)) && !is_schema_rejection(&e));
}

const OPENAI_REFUSAL: &str = concat!(
    "data: {\"choices\":[{\"index\":0,\"delta\":{\"role\":\"assistant\",\"refusal\":\"\"}}]}\n\n",
    "data: {\"choices\":[{\"index\":0,\"delta\":{\"refusal\":\"I can't help with that.\"}}]}\n\n",
    "data: {\"choices\":[{\"index\":0,\"delta\":{},\"finish_reason\":\"stop\"}]}\n\n",
    "data: {\"choices\":[],\"usage\":{\"prompt_tokens\":5,\"completion_tokens\":4}}\n\n",
    "data: [DONE]\n\n",
);

const OPENAI_CONTENT_FILTER: &str = concat!(
    "data: {\"choices\":[{\"index\":0,\"delta\":{\"content\":\"{\"}}]}\n\n",
    "data: {\"choices\":[{\"index\":0,\"delta\":{},\"finish_reason\":\"content_filter\"}]}\n\n",
    "data: [DONE]\n\n",
);

#[tokio::test]
async fn openai_sends_response_format_and_maps_refusal() {
    let (base, seen) = spawn_mock(StatusCode::OK, OPENAI_REFUSAL).await;
    let provider = OpenAiProvider::new(reqwest::Client::new(), base, None, "gpt-x");
    let deltas = drain(&provider, &structured("x")).await;

    let body = seen.lock().unwrap().clone().unwrap();
    let rf = &body["response_format"];
    assert_eq!(rf["type"], "json_schema");
    assert_eq!(rf["json_schema"]["name"], "triage");
    // summary が任意なので strict は名乗らない（事後検証で守る）。
    assert_eq!(rf["json_schema"]["strict"], false);
    assert_eq!(rf["json_schema"]["schema"]["additionalProperties"], false);
    assert_eq!(text_of(&deltas), "I can't help with that.");
    assert_eq!(stop_of(&deltas), Some(StopReason::Refusal));

    let (base, _) = spawn_mock(StatusCode::OK, OPENAI_CONTENT_FILTER).await;
    let provider = OpenAiProvider::new(reqwest::Client::new(), base, None, "gpt-x");
    let deltas = drain(&provider, &structured("x")).await;
    assert_eq!(stop_of(&deltas), Some(StopReason::Refusal));
}

#[tokio::test]
async fn openai_schema_rejection_hides_the_provider_body() {
    const BODY: &str =
        r#"{"error":{"message":"Invalid schema for response_format 'triage': org-123"}}"#;
    let (base, _) = spawn_mock(StatusCode::BAD_REQUEST, BODY).await;
    let provider = OpenAiProvider::new(reqwest::Client::new(), base, None, "gpt-x");
    let Err(e) = provider.stream(&structured("x")).await else {
        panic!("expected an error")
    };
    assert!(
        is_schema_rejection(&e) && !e.to_string().contains("org-123"),
        "{e}"
    );
}

// ---- 項目の並び（BUG-123・D-103） ----

/// 作者が理由を先・結論を後に並べた形（辞書順なら answer・evidence・reason の順になる）。
fn reasoned_request() -> GenerateRequest {
    let mut req = GenerateRequest::new(vec![Message::text(Role::User, "判定してください")]);
    req.system = Some("あなたは審査係です。".into());
    req.response_format = Some(ResponseFormat::JsonSchema {
        name: "judge".into(),
        schema: json!({
            "type": "object",
            "x-order": ["reason", "evidence", "answer"],
            "required": ["reason", "evidence", "answer"],
            "additionalProperties": false,
            "properties": {
                "answer": { "type": "string", "enum": ["approve", "reject"] },
                "evidence": {
                    "type": "array",
                    "items": {
                        "type": "object",
                        "x-order": ["quote", "page"],
                        "required": ["quote", "page"],
                        "additionalProperties": false,
                        "properties": { "page": { "type": "integer" }, "quote": { "type": "string" } },
                    },
                },
                "reason": { "type": "string" },
            },
        }),
    });
    req
}

/// 本文の文字列で、`keys` がこの順に書かれているか。`escaped` なら文字列の中（system の指示）を探す。
fn written_in_order(raw: &str, keys: &[&str], escaped: bool) -> bool {
    let at: Vec<Option<usize>> = keys
        .iter()
        .map(|k| {
            let needle = if escaped {
                format!("\\\"{k}\\\":")
            } else {
                format!("\"{k}\":")
            };
            raw.find(&needle)
        })
        .collect();
    at.iter().all(Option::is_some) && at.windows(2).all(|w| w[0] < w[1])
}

const ANTHROPIC_DONE: &str = concat!(
    "data: {\"type\":\"message_start\",\"message\":{\"usage\":{\"input_tokens\":9}}}\n\n",
    "data: {\"type\":\"message_delta\",\"delta\":{\"stop_reason\":\"end_turn\"},\"usage\":{\"output_tokens\":1}}\n\n",
    "data: {\"type\":\"message_stop\"}\n\n",
);

const OPENAI_DONE: &str = concat!(
    "data: {\"choices\":[{\"index\":0,\"delta\":{\"content\":\"{}\"}}]}\n\n",
    "data: {\"choices\":[{\"index\":0,\"delta\":{},\"finish_reason\":\"stop\"}]}\n\n",
    "data: [DONE]\n\n",
);

#[tokio::test]
async fn anthropic_sends_the_schema_in_the_authors_order() {
    let (base, seen) = spawn_raw_mock(ANTHROPIC_DONE).await;
    let provider = AnthropicProvider::new(reqwest::Client::new(), base, "k", "claude-haiku-5-5");
    let mut req = reasoned_request();
    req.effort = Some(Effort::Low);
    drain(&provider, &req).await;
    let raw = seen.lock().unwrap().clone().unwrap();
    assert!(
        written_in_order(&raw, &["reason", "evidence", "answer"], false),
        "{raw}"
    );
    assert!(written_in_order(&raw, &["quote", "page"], false), "{raw}");
    assert!(!raw.contains("x-order"), "{raw}");
    // 並べ替えても中身は同じ（effort との併合・閉じたオブジェクト）。
    let body: Value = serde_json::from_str(&raw).unwrap();
    assert_eq!(body["output_config"]["effort"], "low");
    assert_eq!(
        body["output_config"]["format"]["schema"]["properties"]["evidence"]["items"]
            ["additionalProperties"],
        false
    );
}

#[tokio::test]
async fn openai_compatible_sends_the_schema_in_the_authors_order() {
    // OpenAI 互換の制約付きデコード（json_schema を有効にしたモデル）と、同じ口で繋ぐ Gemini API
    // （`/v1beta/openai`）。どちらも同じアダプタ・同じ本文の組み方を通る。
    for path in ["/v1", "/v1beta/openai"] {
        let (base, seen) = spawn_raw_mock(OPENAI_DONE).await;
        let provider = OpenAiProvider::new(
            reqwest::Client::new(),
            format!("{base}{path}"),
            Some("k".into()),
            "gemini-2.5-flash",
        );
        drain(&provider, &reasoned_request()).await;
        let raw = seen.lock().unwrap().clone().unwrap();
        assert!(
            written_in_order(&raw, &["reason", "evidence", "answer"], false),
            "{path}: {raw}"
        );
        assert!(written_in_order(&raw, &["quote", "page"], false), "{raw}");
        assert!(!raw.contains("x-order"), "{raw}");
        let body: Value = serde_json::from_str(&raw).unwrap();
        assert_eq!(body["response_format"]["json_schema"]["strict"], true);
    }
}

#[tokio::test]
async fn prompt_mode_writes_the_schema_in_the_authors_order_into_system() {
    // OpenAI 互換の既定（指示だけ）。Gemini API などは形を system の指示で伝える（gateway が送る前に整える）。
    for mode in [
        StructuredOutputMode::Prompt,
        StructuredOutputMode::JsonObject,
    ] {
        let (base, seen) = spawn_raw_mock(OPENAI_DONE).await;
        let provider = OpenAiProvider::new(
            reqwest::Client::new(),
            format!("{base}/v1beta/openai"),
            Some("k".into()),
            "gemini-2.5-flash",
        );
        let mut req = reasoned_request();
        apply_mode(&mut req, mode);
        drain(&provider, &req).await;
        let raw = seen.lock().unwrap().clone().unwrap();
        assert!(
            written_in_order(&raw, &["reason", "evidence", "answer"], true),
            "{mode:?}: {raw}"
        );
        assert!(written_in_order(&raw, &["quote", "page"], true), "{raw}");
        assert!(!raw.contains("x-order"), "{raw}");
    }
}

// ---- gateway（カタログのモード → プロバイダへ届く形） ----

async fn pool() -> Option<sqlx::PgPool> {
    let Ok(db_url) = shiki_testenv::var("STORAGE_TEST_DATABASE_URL") else {
        eprintln!("STORAGE_TEST_DATABASE_URL 未設定のためスキップ");
        return None;
    };
    let pool = sqlx::postgres::PgPoolOptions::new()
        .max_connections(2)
        .connect(&db_url)
        .await
        .expect("DB 接続");
    sqlx::migrate!("../../migrations")
        .run(&pool)
        .await
        .expect("マイグレーション適用");
    Some(pool)
}

fn ctx() -> authz::AuthContext {
    // 設定の行を持たない新しいテナント（既定＝全モデル有効）。読むだけで行は作らない。
    let tenant = format!("t-{}", uuid::Uuid::new_v4());
    authz::AuthContext::new(
        authz::Principal {
            kind: authz::PrincipalKind::User,
            id: "alice".into(),
            email: None,
            groups: vec![],
            roles: vec![],
            tenant_id: Some(tenant.clone()),
        },
        "acme".into(),
        authz::TenantId::parse(tenant).unwrap(),
    )
}

/// stub に 2 モデル: `native`（既定＝json_schema）と `prompted`（prompt を明示）。
fn gateway(pool: sqlx::PgPool) -> LlmGateway {
    let config = GatewayConfig {
        catalog: ModelCatalog {
            default_model: "native".into(),
            models: vec![
                ModelEntry {
                    id: "native".into(),
                    ..Default::default()
                },
                ModelEntry {
                    id: "prompted".into(),
                    structured_output: Some(StructuredOutputMode::Prompt),
                    ..Default::default()
                },
            ],
        },
        ..Default::default()
    };
    LlmGateway::build(pool, reqwest::Client::new(), config).expect("gateway")
}

async fn generate(gw: &LlmGateway, model: &str, user: &str) -> String {
    let mut req = structured(user);
    req.system = Some("あなたは仕分け係です。".into());
    req.model = Some(model.into());
    let mut stream = gw.stream(&ctx(), req).await.expect("stream");
    let mut text = String::new();
    while let Some(d) = stream.next().await {
        if let StreamDelta::TextDelta { text: t } = d.unwrap() {
            text.push_str(&t);
        }
    }
    text
}

#[tokio::test]
async fn gateway_applies_the_catalog_mode_before_the_provider() {
    let Some(pool) = pool().await else { return };
    let gw = gateway(pool);

    // json_schema: 形の指定はそのまま届き、system には何も足さない。
    let system = generate(&gw, "native", "echosystem:").await;
    assert_eq!(system, "system: あなたは仕分け係です。");
    // prompt: system の末尾に指示とスキーマ（制約込み）が足される。
    let system = generate(&gw, "prompted", "echosystem:").await;
    assert!(
        system.starts_with("system: あなたは仕分け係です。\n\n出力の形式: "),
        "{system}"
    );
    assert!(system.contains("<json_schema>") && system.contains("\"maximum\":100"));

    // どちらのモードでも、stub はスキーマに合う値を返す（後続の担当の E2E が使う）。
    for model in ["native", "prompted"] {
        let v: Value = serde_json::from_str(&generate(&gw, model, "不具合（bug）です").await)
            .unwrap_or_else(|e| panic!("{model}: {e}"));
        assert_eq!(v["category"], "bug", "{model}");
        assert_eq!(v["confidence"], 0, "{model}");
    }
}

// ---- 実エンドポイント（中継経由の Haiku 5.5 など） ----

/// `ANTHROPIC_TEST_URL` があれば実エンドポイントのアダプタを組む（無ければスキップ）。
///
/// 例（キャンペーンの中継）: `ANTHROPIC_TEST_URL=http://127.0.0.1:8787`
///     `ANTHROPIC_TEST_MODEL=claude-haiku-5-5` `ANTHROPIC_TEST_KEY=dummy-via-relay`
fn live_provider() -> Option<AnthropicProvider> {
    let base = shiki_testenv::var("ANTHROPIC_TEST_URL").ok()?;
    let model = std::env::var("ANTHROPIC_TEST_MODEL").unwrap_or_else(|_| "m".into());
    Some(AnthropicProvider::new(
        reqwest::Client::new(),
        base,
        std::env::var("ANTHROPIC_TEST_KEY").unwrap_or_default(),
        model,
    ))
}

/// 実エンドポイントで、制約付きデコードの出力がスキーマ（enum・必須）に合い、制約を落とした
/// 項目も説明どおりの値域に入ること。effort なし・effort 付き（併合）の 2 回を送る。
#[tokio::test]
async fn anthropic_live_structured_output() {
    let Some(provider) = live_provider() else {
        return;
    };
    for effort in [None, Some(Effort::Low)] {
        let mut req = structured(
            "次の問い合わせを分類してください: 「先月の請求額が二重に引き落とされています。返金してください。」",
        );
        req.effort = effort;
        // 思考も max_tokens に数える（effort 省略時の既定は medium）。
        req.max_tokens = Some(2048);
        let deltas = drain(&provider, &req).await;
        let text = text_of(&deltas);
        assert_eq!(
            stop_of(&deltas),
            Some(StopReason::EndTurn),
            "{effort:?}: {text}"
        );
        let v: Value = serde_json::from_str(&text).unwrap_or_else(|e| panic!("{e}: {text}"));
        assert_eq!(v["category"], "billing", "{effort:?}: {text}");
        let c = v["confidence"].as_i64().expect("integer confidence");
        assert!((0..=100).contains(&c), "{text}");
        let keys: Vec<&String> = v.as_object().unwrap().keys().collect();
        assert!(
            keys.iter()
                .all(|k| ["category", "confidence", "summary"].contains(&k.as_str())),
            "{text}"
        );
        eprintln!("live structured ({effort:?}): {text}");
    }
}

/// 実エンドポイントで、任意項目の上限（24）を超えたスキーマが定型文のスキーマ拒否になること
/// （生成前に 400 で返るので課金されない）。
#[tokio::test]
async fn anthropic_live_too_many_optional_parameters_is_a_schema_rejection() {
    let Some(provider) = live_provider() else {
        return;
    };
    let props: serde_json::Map<String, Value> = (0..25)
        .map(|i| (format!("f{i}"), json!({ "type": "string" })))
        .collect();
    let mut req = GenerateRequest::new(vec![Message::text(Role::User, "hi")]);
    req.max_tokens = Some(16);
    req.response_format = Some(ResponseFormat::JsonSchema {
        name: "wide".into(),
        schema: json!({ "type": "object", "properties": props }),
    });
    let Err(e) = provider.stream(&req).await else {
        panic!("expected a 400")
    };
    assert!(is_schema_rejection(&e), "{e:?}");
    assert!(e.to_string().contains("複雑すぎて"), "{e}");
}
