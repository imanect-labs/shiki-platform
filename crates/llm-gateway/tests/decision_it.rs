//! 判定のチョークポイント（`LlmGateway::decide`・FEAT-1）の結合テスト（実 Postgres・LLM は stub）。
//!
//! `STORAGE_TEST_DATABASE_URL` が設定されている時のみ実行（未設定はスキップ）。
//! - 使えた判定は呼び出し側のキーで、使えなかった判定は `{key}#f{uuid}` で会計に載る。失敗の後に同じ
//!   キーで成功しても、成功の行が重複除去で消えない（R2）。
//! - llm_decider は元の chat モデルの単価・プロバイダで請求する（会計の model は元のモデル）。
//! - 判定用のモデルと元の chat モデルの**両方**が組織で有効でないと判定しない（fail-closed）。
//! - チャットのモデルを切り替えて保存しても、判定用のモデルは許可リストに残る。
//! - 利用上限は請求先のプロバイダの枠で確かめる。
//!
//! テストで作った行（会計・組織の AI 設定）はテナントごとに最後に消す。

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::print_stderr
)]

use authz::AuthContext;
use llm_gateway::{
    decide_error_kind, Answer, ChoiceOption, DecideCall, DecideRequest, GatewayConfig, LlmError,
    LlmGateway, Modality, ModelCatalog, ModelEntry, ProviderConfig, ProviderKind, ProviderLimit,
    Question, QuestionKind, SettingsPatch, UsageLimitsConfig, UsageSource,
};
use sqlx::postgres::PgPoolOptions;
use sqlx::PgPool;

async fn pool() -> Option<PgPool> {
    let Ok(db_url) = std::env::var("STORAGE_TEST_DATABASE_URL") else {
        eprintln!("STORAGE_TEST_DATABASE_URL 未設定のためスキップ");
        return None;
    };
    let pool = PgPoolOptions::new()
        .max_connections(4)
        .connect(&db_url)
        .await
        .expect("DB 接続");
    sqlx::migrate!("../../migrations")
        .run(&pool)
        .await
        .expect("マイグレーション適用");
    Some(pool)
}

/// テストごとの組織（AI 設定・会計を他のテストと共有しない）。
fn workflow() -> AuthContext {
    let tenant = format!("dec-{}", uuid::Uuid::new_v4().simple());
    AuthContext::for_workflow(tenant, "org".into(), "wf-1")
}

/// 管理者（組織の AI 設定を保存する）。
fn admin(of: &AuthContext) -> AuthContext {
    AuthContext::new(
        authz::Principal {
            kind: authz::PrincipalKind::User,
            id: "admin".into(),
            email: None,
            groups: vec![],
            roles: vec![],
            tenant_id: Some(of.tenant_id.clone()),
        },
        of.org.clone(),
        of.tenant_id.clone(),
    )
}

async fn cleanup(pool: &PgPool, ctx: &AuthContext) {
    for table in ["llm_usage", "tenant_llm_model", "tenant_llm_settings"] {
        sqlx::query(&format!("DELETE FROM {table} WHERE tenant_id = $1"))
            .bind(&ctx.tenant_id)
            .execute(pool)
            .await
            .unwrap();
    }
}

fn chat(id: &str, provider: &str) -> ModelEntry {
    ModelEntry {
        id: id.into(),
        provider: Some(provider.into()),
        // 1 Mtok あたり $1（請求の単位が元のモデルになることを金額で見る）。
        prompt_price_micros_per_mtok: 1_000_000,
        completion_price_micros_per_mtok: 1_000_000,
        ..Default::default()
    }
}

fn decision(id: &str, via: Option<&str>, provider: &str) -> ModelEntry {
    ModelEntry {
        id: id.into(),
        provider: Some(provider.into()),
        modality: Modality::Decision,
        decision_via: via.map(str::to_string),
        ..Default::default()
    }
}

/// 既定プロバイダ `default`（stub）に chat の fast・smart、判定用の decide-stub（stub の決定器）と
/// decide-fast（fast を元にした llm_decider）。`paid` は利用上限を試す追加の stub プロバイダ。
fn gateway(pool: PgPool, paid: &str, limits: UsageLimitsConfig) -> LlmGateway {
    LlmGateway::build(
        pool,
        reqwest::Client::new(),
        GatewayConfig {
            extra_providers: vec![ProviderConfig {
                id: paid.into(),
                kind: ProviderKind::Stub,
                ..Default::default()
            }],
            catalog: ModelCatalog {
                default_model: "fast".into(),
                models: vec![
                    chat("fast", "default"),
                    chat("smart", "default"),
                    chat("paid-chat", paid),
                    decision("decide-stub", None, "default"),
                    decision("decide-fast", Some("fast"), "default"),
                    decision("decide-paid", Some("paid-chat"), paid),
                ],
            },
            limits,
            ..Default::default()
        },
    )
    .expect("gateway")
}

fn request(model: &str, state: &str) -> DecideRequest {
    DecideRequest {
        model: model.into(),
        state: serde_json::json!({ "body": state }),
        questions: vec![
            Question {
                key: "category".into(),
                instructions: "問い合わせの種類はどれか".into(),
                kind: QuestionKind::Choice {
                    options: vec![
                        ChoiceOption {
                            key: "billing".into(),
                            description: "請求".into(),
                        },
                        ChoiceOption {
                            key: "bug".into(),
                            description: "不具合".into(),
                        },
                    ],
                },
            },
            Question {
                key: "angry".into(),
                instructions: "強い不満を示しているか".into(),
                kind: QuestionKind::Boolean {
                    yes: Some("解約".into()),
                    no: None,
                },
            },
        ],
    }
}

fn call(key: &str) -> DecideCall {
    DecideCall {
        idempotency_key: key.into(),
        trace_id: None,
        source: UsageSource::Workflow,
        app_id: None,
    }
}

/// 組織の会計の行（冪等キー・model・provider・provider_id・source・cost）。
async fn usage_rows(
    pool: &PgPool,
    ctx: &AuthContext,
) -> Vec<(String, String, String, String, String, i64)> {
    sqlx::query_as(
        "SELECT idempotency_key, model, provider, provider_id, source, cost_usd_micros \
         FROM llm_usage WHERE tenant_id = $1 ORDER BY created_at, idempotency_key",
    )
    .bind(&ctx.tenant_id)
    .fetch_all(pool)
    .await
    .unwrap()
}

#[tokio::test]
async fn a_stub_decision_is_accounted_under_the_callers_key() {
    let Some(pool) = pool().await else { return };
    let gw = gateway(pool.clone(), "paid-unused", UsageLimitsConfig::default());
    let flow = workflow();
    let got = gw
        .decide(
            &flow,
            &request("decide-stub", "不具合で動きません"),
            &call("wf:r1:decide"),
        )
        .await
        .unwrap();
    assert!(matches!(&got.answers["category"], Answer::Choice { value, .. } if value == "bug"));
    assert!(!got.calibrated);
    let rows = usage_rows(&pool, &flow).await;
    assert_eq!(rows.len(), 1, "{rows:?}");
    let (key, model, provider, provider_id, source, _) = &rows[0];
    assert_eq!(
        (
            key.as_str(),
            model.as_str(),
            provider.as_str(),
            provider_id.as_str(),
            source.as_str()
        ),
        ("wf:r1:decide", "decide-stub", "stub", "default", "workflow")
    );
    cleanup(&pool, &flow).await;
}

#[tokio::test]
async fn an_llm_decision_is_billed_as_its_base_chat_model() {
    let Some(pool) = pool().await else { return };
    let gw = gateway(pool.clone(), "paid-unused", UsageLimitsConfig::default());
    let flow = workflow();
    let got = gw
        .decide(
            &flow,
            &request("decide-fast", "請求について"),
            &call("wf:r2:decide"),
        )
        .await
        .unwrap();
    assert!(
        got.resolved_model.starts_with("fast+llm_decider/v3/k1/"),
        "{}",
        got.resolved_model
    );
    let rows = usage_rows(&pool, &flow).await;
    assert_eq!(rows.len(), 1, "{rows:?}");
    let (key, model, _, provider_id, _, cost) = &rows[0];
    assert_eq!(
        (key.as_str(), model.as_str(), provider_id.as_str()),
        ("wf:r2:decide", "fast", "default")
    );
    // 単価は元のモデル（$1/Mtok）で数える（判定用のモデルの単価 0 ではない）。
    assert!(*cost > 0, "{cost}");
    cleanup(&pool, &flow).await;
}

#[tokio::test]
async fn a_failed_decision_does_not_hide_the_later_success() {
    let Some(pool) = pool().await else { return };
    let gw = gateway(pool.clone(), "paid-unused", UsageLimitsConfig::default());
    let flow = workflow();
    let key = "wf:r3:decide";
    // 1 回目は決定器が失敗する（使用量つき）。step の再試行で、同じキーのまま 2 回目が成功する。
    let err = gw
        .decide(&flow, &request("decide-stub", "stub:fail"), &call(key))
        .await
        .unwrap_err();
    assert!(matches!(err, LlmError::Unavailable(_)), "{err:?}");
    // 決定器が要求と合わない答えを返したものも、使えなかった判定として別のキーで残す。
    let err = gw
        .decide(&flow, &request("decide-stub", "stub:malformed"), &call(key))
        .await
        .unwrap_err();
    assert!(matches!(err, LlmError::Internal(_)), "{err:?}");
    // 応答の不整合と分かる定型文の頭が付く（呼び出し側が再試行しない decision_invalid_response に写す）。
    assert_eq!(
        llm_gateway::decide_error_kind(&err),
        Some(llm_gateway::DecideErrorKind::InvalidResponse),
        "{err:?}"
    );
    gw.decide(&flow, &request("decide-stub", "不具合"), &call(key))
        .await
        .unwrap();
    let rows = usage_rows(&pool, &flow).await;
    let keys: Vec<&str> = rows.iter().map(|r| r.0.as_str()).collect();
    assert_eq!(rows.len(), 3, "失敗 2 回と成功の 3 行: {keys:?}");
    assert_eq!(keys.iter().filter(|k| **k == key).count(), 1);
    assert_eq!(
        keys.iter()
            .filter(|k| k.starts_with(&format!("{key}#f")))
            .count(),
        2
    );
    cleanup(&pool, &flow).await;
}

#[tokio::test]
async fn both_the_decision_model_and_its_base_model_must_be_enabled() {
    let Some(pool) = pool().await else { return };
    let gw = gateway(pool.clone(), "paid-unused", UsageLimitsConfig::default());
    let flow = workflow();
    let admin = admin(&flow);
    // 元の chat モデル（fast）だけを無効にする。判定用の decide-fast は有効のまま。
    gw.update_tenant_settings(
        &admin,
        &SettingsPatch {
            models: vec![("fast".into(), false)],
            ..Default::default()
        },
    )
    .await
    .unwrap()
    .unwrap();
    let err = gw
        .decide(&flow, &request("decide-fast", "x"), &call("wf:r4:a"))
        .await
        .unwrap_err();
    assert!(
        matches!(&err, LlmError::Config(m) if m.contains("not enabled") && m.contains("fast")),
        "{err:?}"
    );
    // 判定用のモデルを無効にしても止まる（元のモデルを戻しても）。
    gw.update_tenant_settings(
        &admin,
        &SettingsPatch {
            models: vec![("fast".into(), true), ("decide-fast".into(), false)],
            ..Default::default()
        },
    )
    .await
    .unwrap()
    .unwrap();
    let err = gw
        .decide(&flow, &request("decide-fast", "x"), &call("wf:r4:b"))
        .await
        .unwrap_err();
    assert!(
        matches!(&err, LlmError::Config(m) if m.contains("decide-fast")),
        "{err:?}"
    );
    // 拒否した判定は決定器を呼ばないので、会計に何も残らない。
    assert!(usage_rows(&pool, &flow).await.is_empty());
    cleanup(&pool, &flow).await;
}

#[tokio::test]
async fn switching_the_chat_model_keeps_decision_models_allowed() {
    let Some(pool) = pool().await else { return };
    let gw = gateway(pool.clone(), "paid-unused", UsageLimitsConfig::default());
    let flow = workflow();
    let admin = admin(&flow);
    // 管理者がチャットの既定を smart に切り替えて保存する（判定用のモデルには触れない）。
    let saved = gw
        .update_tenant_settings(
            &admin,
            &SettingsPatch {
                default_model: Some("smart".into()),
                ..Default::default()
            },
        )
        .await
        .unwrap()
        .unwrap();
    assert!(saved
        .allowed_models
        .as_ref()
        .unwrap()
        .contains(&"decide-stub".to_string()));
    gw.decide(&flow, &request("decide-stub", "不具合"), &call("wf:r5"))
        .await
        .expect("保存の後も判定できる");
    // 判定用のモデルだけを残した保存は拒否する（チャットに使えるモデルが無くなる）。
    let only_decisions = gw
        .update_tenant_settings(
            &admin,
            &SettingsPatch {
                models: ["fast", "smart", "paid-chat"]
                    .iter()
                    .map(|m| ((*m).to_string(), false))
                    .collect(),
                ..Default::default()
            },
        )
        .await
        .unwrap();
    assert_eq!(
        only_decisions,
        Err(llm_gateway::SettingsError::NoModelEnabled)
    );
    cleanup(&pool, &flow).await;
}

#[tokio::test]
async fn usage_limits_are_checked_on_the_billing_provider() {
    let Some(pool) = pool().await else { return };
    // 環境総枠はテナント横断で数えるので、プロバイダ ID はテストごとに一意にする。
    let paid = format!("paid-{}", uuid::Uuid::new_v4().simple());
    let gw = gateway(
        pool.clone(),
        &paid,
        UsageLimitsConfig {
            enabled: true,
            per_user_usd_micros: None,
            providers: vec![ProviderLimit {
                providers: vec![paid.clone()],
                usd_micros: 1,
            }],
        },
    );
    let flow = workflow();
    // 枠の内なら通り、使った分（元のモデルの単価）が枠に数えられる。
    gw.decide(&flow, &request("decide-paid", "x"), &call("wf:r6:a"))
        .await
        .unwrap();
    let err = gw
        .decide(&flow, &request("decide-paid", "x"), &call("wf:r6:b"))
        .await
        .unwrap_err();
    assert!(matches!(err, LlmError::RateLimited(_)), "{err:?}");
    assert!(decide_error_kind(&err).is_none());
    // 別のプロバイダの判定は止まらない。
    gw.decide(&flow, &request("decide-stub", "x"), &call("wf:r6:c"))
        .await
        .unwrap();
    cleanup(&pool, &flow).await;
}
