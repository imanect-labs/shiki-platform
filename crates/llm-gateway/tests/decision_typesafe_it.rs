//! TypeSafe System One 互換アダプタ（FEAT-1 の T9・実験的）の HTTP の結線テスト（ローカル axum のモック）。
//!
//! API キーが無いので実 API は呼ばない。公開文書の形（docs.typesafe.ai の HTTP API の頁の例）を返すモックで、
//! 送る形・受ける形・誤りの写し方・版ガードを確かめる。最後のテストだけ `STORAGE_TEST_DATABASE_URL` があれば
//! gateway のチョークポイント（組織の opt-in・会計）まで通す（無ければスキップ）。

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::print_stderr
)]

use std::sync::{Arc, Mutex};

use axum::extract::State;
use axum::http::{HeaderMap, StatusCode};
use axum::response::IntoResponse;
use axum::routing::post;
use axum::{Json, Router};
use serde_json::{json, Value};

use llm_gateway::providers::typesafe::TypesafeDecider;
use llm_gateway::{
    Answer, ChoiceOption, DecideCall, DecideRequest, DecisionMethod, DecisionProvider,
    GatewayConfig, LlmError, LlmGateway, Modality, ModelCatalog, ModelEntry, ProviderConfig,
    ProviderKind, Question, QuestionKind, SettingsPatch, UsageSource,
};

type Seen = Arc<Mutex<Vec<(HeaderMap, Value)>>>;

#[derive(Clone)]
struct Mock {
    seen: Seen,
    status: StatusCode,
    body: Value,
}

async fn handle(
    State(m): State<Mock>,
    headers: HeaderMap,
    Json(body): Json<Value>,
) -> impl IntoResponse {
    m.seen.lock().unwrap().push((headers, body));
    (m.status, Json(m.body.clone()))
}

/// 決まったステータスと本文を返すモックを立て、(base_url, 受けた要求) を返す。
async fn spawn(status: StatusCode, body: Value) -> (String, Seen) {
    let seen: Seen = Arc::default();
    let app = Router::new()
        .route("/v1/systemone", post(handle))
        .with_state(Mock {
            seen: Arc::clone(&seen),
            status,
            body,
        });
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    });
    (format!("http://{addr}"), seen)
}

fn request(model: &str) -> DecideRequest {
    DecideRequest {
        model: model.into(),
        state: json!("I was charged twice for my subscription this month."),
        questions: vec![
            Question {
                key: "tone".into(),
                instructions: "What is the tone of this message?".into(),
                kind: QuestionKind::Choice {
                    options: vec![
                        ChoiceOption {
                            key: "angry".into(),
                            description: "An upset or hostile message".into(),
                        },
                        ChoiceOption {
                            key: "calm".into(),
                            description: "A neutral or polite message".into(),
                        },
                        ChoiceOption {
                            key: "excited".into(),
                            description: "An enthusiastic or eager message".into(),
                        },
                    ],
                },
            },
            Question {
                key: "urgency".into(),
                instructions: "How urgent is this message?".into(),
                kind: QuestionKind::Score {
                    levels: vec![
                        "Can wait".into(),
                        "Needs attention this week".into(),
                        "Needs attention today".into(),
                    ],
                },
            },
            Question {
                key: "billing".into(),
                instructions: "Is this message about billing?".into(),
                kind: QuestionKind::Boolean {
                    yes: None,
                    no: None,
                },
            },
        ],
    }
}

/// 公開文書の例（choice・score・noul）をまとめた応答。
fn published_reply(model: &str) -> Value {
    json!({
        "model": model,
        "answers": {
            "tone": { "type": "choice", "choice": "calm", "confidence": 0.7,
                      "probabilities": { "angry": 0.15, "calm": 0.8, "excited": 0.05 } },
            "urgency": { "type": "score", "score": 1.7, "confidence": 0.55,
                         "legend": { "0": "Can wait", "1": "Needs attention this week", "2": "Needs attention today" },
                         "probabilities": { "0": 0.1, "1": 0.1, "2": 0.8 } },
            "billing": { "type": "noul", "noul": 0.98 }
        },
        "usage": { "input_tokens": 120, "output_tokens": 12 }
    })
}

#[tokio::test]
async fn a_decision_is_sent_in_the_published_shape_and_read_back() {
    let (base, seen) = spawn(StatusCode::OK, published_reply("jev-1.13.0")).await;
    let d = TypesafeDecider::new(reqwest::Client::new(), base, "sk-test", "jev-1.13.0");
    let got = d.decide(&request("decide-jev")).await.unwrap();

    let seen = seen.lock().unwrap();
    assert_eq!(seen.len(), 1);
    let (headers, body) = &seen[0];
    assert_eq!(headers["authorization"], "Bearer sk-test");
    assert_eq!(
        body["model"], "jev-1.13.0",
        "設定の版を送る（判定用のモデルの論理名ではない）"
    );
    assert_eq!(
        body["state"],
        json!("I was charged twice for my subscription this month.")
    );
    assert_eq!(
        body["questions"]["tone"],
        json!({ "type": "choice", "instructions": "What is the tone of this message?",
                "criteria": { "angry": "An upset or hostile message", "calm": "A neutral or polite message",
                              "excited": "An enthusiastic or eager message" } })
    );
    assert_eq!(
        body["questions"]["urgency"]["criteria"],
        json!([
            "Can wait",
            "Needs attention this week",
            "Needs attention today"
        ])
    );
    assert_eq!(
        body["questions"]["billing"],
        json!({ "type": "noul", "instructions": "Is this message about billing?" })
    );

    assert_eq!(got.resolved_model, "jev-1.13.0");
    assert_eq!(got.method, DecisionMethod::Native);
    assert!(!got.calibrated, "公開文書は較正を主張していない");
    assert_eq!(got.samples, 1);
    assert_eq!(
        (got.usage.prompt_tokens, got.usage.completion_tokens),
        (120, 12)
    );
    assert!(
        matches!(&got.answers["tone"], Answer::Choice { value, confidence, .. } if value == "calm" && (*confidence - 0.8).abs() < 1e-12)
    );
    assert!(matches!(
        &got.answers["urgency"],
        Answer::Score { level_index: 2, value, .. } if (*value - 1.7).abs() < 1e-12
    ));
    assert!(
        matches!(got.answers["billing"], Answer::Boolean { value: true, probability, confidence } if (probability - 0.98).abs() < 1e-12 && (confidence - 0.98).abs() < 1e-12)
    );
}

#[tokio::test]
async fn http_errors_map_to_gateway_errors_without_the_vendor_body() {
    let secret_body = json!({ "detail": "org_9f2 over quota; see https://billing.example" });
    for (status, want) in [
        (StatusCode::TOO_MANY_REQUESTS, "rate_limited"),
        (StatusCode::UNAUTHORIZED, "config"),
        (StatusCode::UNPROCESSABLE_ENTITY, "bad_request"),
        (StatusCode::SERVICE_UNAVAILABLE, "unavailable"),
    ] {
        let (base, _) = spawn(status, secret_body.clone()).await;
        let d = TypesafeDecider::new(reqwest::Client::new(), base, "k", "jev-1.13.0");
        let f = d.decide(&request("decide-jev")).await.unwrap_err();
        let kind = match &f.error {
            LlmError::RateLimited(m) => {
                assert!(!m.contains("org_9f2") && !m.contains("https://"), "{m}");
                "rate_limited"
            }
            LlmError::Config(m) => {
                assert!(!m.contains("org_9f2"), "{m}");
                "config"
            }
            LlmError::BadRequest(m) => {
                // 4xx の本文はログ用にだけ持ち、定型文の頭（判定の失敗の種類）と取り違えない。
                assert!(llm_gateway::decide_error_kind(&f.error).is_none(), "{m}");
                "bad_request"
            }
            LlmError::Unavailable(_) => "unavailable",
            other @ LlmError::Internal(_) => panic!("{status}: {other:?}"),
        };
        assert_eq!(kind, want, "{status}");
        assert_eq!(
            f.usage,
            llm_gateway::Usage::default(),
            "HTTP の誤りは課金されない前提"
        );
    }
}

/// 設定の版と違う版が答えたもの・形の崩れた答えは使わない。課金された分（usage）は返す（会計に載せる）。
#[tokio::test]
async fn other_versions_and_malformed_answers_fail_with_their_usage() {
    let (base, _) = spawn(StatusCode::OK, published_reply("jev-1.14.0")).await;
    let d = TypesafeDecider::new(reqwest::Client::new(), base, "k", "jev-1.13.0");
    let f = d.decide(&request("decide-jev")).await.unwrap_err();
    assert!(
        matches!(&f.error, LlmError::Internal(m) if m.contains("jev-1.14.0")),
        "{:?}",
        f.error
    );
    // 応答の不整合（再試行しない）と分かる。
    assert_eq!(
        llm_gateway::decide_error_kind(&f.error),
        Some(llm_gateway::DecideErrorKind::InvalidResponse)
    );
    assert_eq!(f.usage.prompt_tokens, 120);

    let mut missing = published_reply("jev-1.13.0");
    missing["answers"]
        .as_object_mut()
        .unwrap()
        .remove("billing");
    let (base, _) = spawn(StatusCode::OK, missing).await;
    let d = TypesafeDecider::new(reqwest::Client::new(), base, "k", "jev-1.13.0");
    let f = d.decide(&request("decide-jev")).await.unwrap_err();
    assert!(
        matches!(&f.error, LlmError::Internal(m) if m.contains("billing")),
        "{:?}",
        f.error
    );
    assert_eq!(
        llm_gateway::decide_error_kind(&f.error),
        Some(llm_gateway::DecideErrorKind::InvalidResponse)
    );
    assert_eq!(f.usage.prompt_tokens, 120);
}

/// gateway のチョークポイントを通す: 判定用のモデルは opt-in なので、組織の管理者が有効にするまでモックへ
/// 届かない（既定は無効）。有効にすると判定し、会計は判定用のモデルの単価・TypeSafe のプロバイダで付く。
#[tokio::test]
async fn through_the_gateway_the_model_is_opt_in_and_billed_as_itself() {
    let Ok(db_url) = std::env::var("STORAGE_TEST_DATABASE_URL") else {
        eprintln!("STORAGE_TEST_DATABASE_URL 未設定のためスキップ");
        return;
    };
    let pool = sqlx::postgres::PgPoolOptions::new()
        .max_connections(2)
        .connect(&db_url)
        .await
        .unwrap();
    sqlx::migrate!("../../migrations").run(&pool).await.unwrap();
    let (base, seen) = spawn(StatusCode::OK, published_reply("jev-1.13.0")).await;
    let gw = LlmGateway::build(
        pool.clone(),
        reqwest::Client::new(),
        GatewayConfig {
            extra_providers: vec![ProviderConfig {
                id: "typesafe".into(),
                kind: ProviderKind::TypesafeSystemone,
                base_url: Some(base),
                api_key: Some("k".into()),
                ..Default::default()
            }],
            catalog: ModelCatalog {
                default_model: "fast".into(),
                models: vec![
                    ModelEntry {
                        id: "fast".into(),
                        ..Default::default()
                    },
                    ModelEntry {
                        id: "decide-jev".into(),
                        real_id: Some("jev-1.13.0".into()),
                        provider: Some("typesafe".into()),
                        modality: Modality::Decision,
                        processed_abroad: true,
                        requires_opt_in: true,
                        // $0.042 / Mtok（入力だけ課金）。
                        prompt_price_micros_per_mtok: 42_000,
                        ..Default::default()
                    },
                ],
            },
            ..Default::default()
        },
    )
    .unwrap();
    let tenant = format!("ts-{}", uuid::Uuid::new_v4().simple());
    let flow = authz::AuthContext::for_workflow(
        authz::TenantId::parse(tenant.clone()).unwrap(),
        "org".into(),
        "wf-1",
    );
    let call = DecideCall {
        idempotency_key: "wf:ts:r1:ai".into(),
        trace_id: None,
        source: UsageSource::Workflow,
        app_id: None,
    };
    let err = gw
        .decide(&flow, &request("decide-jev"), &call)
        .await
        .unwrap_err();
    assert!(
        matches!(&err, LlmError::Config(m) if m.contains("not enabled")),
        "{err:?}"
    );
    assert!(
        seen.lock().unwrap().is_empty(),
        "有効にするまで外へ送らない"
    );

    let admin = authz::AuthContext::new(
        authz::Principal {
            kind: authz::PrincipalKind::User,
            id: "admin".into(),
            email: None,
            groups: vec![],
            roles: vec![],
            tenant_id: Some(tenant.clone()),
        },
        "org".into(),
        authz::TenantId::parse(tenant.clone()).unwrap(),
    );
    gw.update_tenant_settings(
        &admin,
        &SettingsPatch {
            models: vec![("decide-jev".into(), true)],
            ..Default::default()
        },
    )
    .await
    .unwrap()
    .unwrap();
    let got = gw
        .decide(&flow, &request("decide-jev"), &call)
        .await
        .unwrap();
    assert_eq!(got.resolved_model, "jev-1.13.0");
    assert_eq!(seen.lock().unwrap().len(), 1);
    let rows: Vec<(String, String, i64)> = sqlx::query_as(
        "SELECT model, provider_id, cost_usd_micros FROM llm_usage WHERE tenant_id = $1",
    )
    .bind(&tenant)
    .fetch_all(&pool)
    .await
    .unwrap();
    // 120 トークン × $0.042/Mtok = 5.04 マイクロ USD（整数に切り捨て）。
    assert_eq!(
        rows,
        [("decide-jev".to_string(), "typesafe".to_string(), 5)]
    );
    for table in ["llm_usage", "tenant_llm_model", "tenant_llm_settings"] {
        sqlx::query(&format!("DELETE FROM {table} WHERE tenant_id = $1"))
            .bind(&tenant)
            .execute(&pool)
            .await
            .unwrap();
    }
}
