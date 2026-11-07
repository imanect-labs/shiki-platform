//! 判定の決定キャッシュ（`LlmGateway::decide_cached`・ai.decide の step 属性 `cache`・T10）の結合テスト
//! （実 Postgres・LLM は stub）。
//!
//! `STORAGE_TEST_DATABASE_URL` が設定されている時のみ実行（未設定はスキップ）。
//! - 同じ版・同じ質問・同じ判定材料の 2 回目は当たり、決定器を呼ばず会計もしない（使用量 0）。
//! - 当たった後に判定用のモデル（または元の chat モデル）を組織で無効にすると `Config` で止まる（当たりでも
//!   許可は毎回確かめる・#553 の約束）。
//! - 範囲はテナント＋組織＋ワークフロー: 別のワークフロー・別の組織では当たらない。
//! - 期限（書き込みからの ttl と、ノードの今の ttl）を過ぎた答えは当たらない。期限切れの行は書き込みで消える。
//! - 当たりは利用上限を確かめない（費用が出ない）。失敗した判定は使い回さない。
//! - 引き上げた質問（BUG-124・D-114）は答えと一緒に残る。項目の無い古い行も当たり、空として読む。
//!
//! テストで作った行（会計・組織の AI 設定・キャッシュ）はテナントごとに最後に消す。

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::print_stderr
)]

use std::time::Duration;

use authz::AuthContext;
use llm_gateway::{
    Answer, ChoiceOption, DecideCall, DecideRequest, DecisionCache, GatewayConfig, LlmError,
    LlmGateway, Modality, ModelCatalog, ModelEntry, ProviderConfig, ProviderKind, ProviderLimit,
    Question, QuestionKind, SettingsPatch, UsageLimitsConfig, UsageSource,
};
use sqlx::postgres::PgPoolOptions;
use sqlx::PgPool;
use uuid::Uuid;

async fn pool() -> Option<PgPool> {
    let Ok(db_url) = shiki_testenv::var("STORAGE_TEST_DATABASE_URL") else {
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

/// テストごとのテナント（AI 設定・会計・キャッシュを他のテストと共有しない）。
fn workflow_in(tenant: &str, org: &str) -> AuthContext {
    AuthContext::for_workflow(
        authz::TenantId::parse(tenant.to_string()).unwrap(),
        org.into(),
        "wf-1",
    )
}

fn tenant() -> String {
    format!("dcache-{}", Uuid::new_v4().simple())
}

fn admin(of: &AuthContext) -> AuthContext {
    AuthContext::new(
        authz::Principal {
            kind: authz::PrincipalKind::User,
            id: "admin".into(),
            email: None,
            groups: vec![],
            roles: vec![],
            tenant_id: Some(of.tenant_id().to_string()),
        },
        of.org().to_string(),
        authz::TenantId::parse(of.tenant_id().to_string()).unwrap(),
    )
}

async fn cleanup(pool: &PgPool, tenant: &str) {
    for table in [
        "llm_usage",
        "tenant_llm_model",
        "tenant_llm_settings",
        "llm_decision_cache",
    ] {
        sqlx::query(&format!("DELETE FROM {table} WHERE tenant_id = $1"))
            .bind(tenant)
            .execute(pool)
            .await
            .unwrap();
    }
}

fn chat(id: &str, provider: &str) -> ModelEntry {
    ModelEntry {
        id: id.into(),
        provider: Some(provider.into()),
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

/// decision_it.rs と同じ構成（stub の決定器・fast を元にした llm_decider・利用上限を試す `paid`）。
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
        questions: vec![Question {
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
        }],
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

fn scope(workflow_id: Uuid) -> DecisionCache {
    DecisionCache {
        workflow_id,
        ttl: Duration::from_hours(1),
    }
}

async fn usage_keys(pool: &PgPool, tenant: &str) -> Vec<String> {
    sqlx::query_scalar(
        "SELECT idempotency_key FROM llm_usage WHERE tenant_id = $1 ORDER BY created_at, idempotency_key",
    )
    .bind(tenant)
    .fetch_all(pool)
    .await
    .unwrap()
}

async fn cache_rows(pool: &PgPool, tenant: &str) -> i64 {
    sqlx::query_scalar("SELECT count(*) FROM llm_decision_cache WHERE tenant_id = $1")
        .bind(tenant)
        .fetch_one(pool)
        .await
        .unwrap()
}

/// 2 回目は当たり、決定器を呼ばず会計もしない。答えは 1 回目と同じで、材料のキーの並びが違っても当たる。
/// 判定材料そのものは DB に持たない。
#[tokio::test]
async fn the_second_identical_decision_is_served_from_the_cache_without_billing() {
    let Some(pool) = pool().await else { return };
    let gw = gateway(pool.clone(), "paid-unused", UsageLimitsConfig::default());
    let t = tenant();
    let flow = workflow_in(&t, "org");
    let wf = Uuid::new_v4();
    let first = gw
        .decide_cached(
            &flow,
            &request("decide-fast", "請求について"),
            &call("wf:r1:ai"),
            &scope(wf),
        )
        .await
        .unwrap();
    assert!(!first.cached);
    assert!(first.response.usage.prompt_tokens > 0);
    let second = gw
        .decide_cached(
            &flow,
            &request("decide-fast", "請求について"),
            &call("wf:r2:ai"),
            &scope(wf),
        )
        .await
        .unwrap();
    assert!(second.cached, "同じ版・質問・材料は当たる");
    assert_eq!(second.response.answers, first.response.answers);
    assert_eq!(
        second.response.resolved_model,
        first.response.resolved_model
    );
    assert_eq!(second.response.usage, llm_gateway::Usage::default());
    // 会計は 1 回目だけ（当たりは決定器を呼ばない）。
    assert_eq!(usage_keys(&pool, &t).await, ["wf:r1:ai"]);
    // 材料（本文）は DB に残さない。
    let stored: Vec<String> =
        sqlx::query_scalar("SELECT response::text FROM llm_decision_cache WHERE tenant_id = $1")
            .bind(&t)
            .fetch_all(&pool)
            .await
            .unwrap();
    assert_eq!(stored.len(), 1);
    assert!(!stored[0].contains("請求について"), "{stored:?}");
    // 材料が違えば外れ（決定器を呼び、会計に載る）。
    let other = gw
        .decide_cached(
            &flow,
            &request("decide-fast", "不具合です"),
            &call("wf:r3:ai"),
            &scope(wf),
        )
        .await
        .unwrap();
    assert!(!other.cached);
    assert_eq!(usage_keys(&pool, &t).await.len(), 2);
    // キャッシュを通さない判定（step 属性 cache なし）は当たりを見ない。
    let plain = gw
        .decide(
            &flow,
            &request("decide-fast", "請求について"),
            &call("wf:r4:ai"),
        )
        .await
        .unwrap();
    assert!(plain.usage.prompt_tokens > 0);
    assert_eq!(usage_keys(&pool, &t).await.len(), 3);
    cleanup(&pool, &t).await;
}

/// 当たった後に判定用のモデル・元の chat モデルを組織で無効にすると、当たりでも Config で止まる。
#[tokio::test]
async fn a_cached_decision_still_requires_both_models_to_be_enabled() {
    let Some(pool) = pool().await else { return };
    let gw = gateway(pool.clone(), "paid-unused", UsageLimitsConfig::default());
    let t = tenant();
    let flow = workflow_in(&t, "org");
    let admin = admin(&flow);
    let wf = Uuid::new_v4();
    let req = request("decide-fast", "請求について");
    gw.decide_cached(&flow, &req, &call("wf:r1:ai"), &scope(wf))
        .await
        .unwrap();
    assert!(
        gw.decide_cached(&flow, &req, &call("wf:r2:ai"), &scope(wf))
            .await
            .unwrap()
            .cached
    );
    for (off, why) in [
        ("decide-fast", "判定用のモデル"),
        ("fast", "元の chat モデル"),
    ] {
        gw.update_tenant_settings(
            &admin,
            &SettingsPatch {
                models: vec![
                    ("decide-fast".into(), off != "decide-fast"),
                    ("fast".into(), off != "fast"),
                ],
                ..Default::default()
            },
        )
        .await
        .unwrap()
        .unwrap();
        let err = gw
            .decide_cached(&flow, &req, &call("wf:r3:ai"), &scope(wf))
            .await
            .unwrap_err();
        assert!(
            matches!(&err, LlmError::Config(m) if m.contains("not enabled") && m.ends_with(&format!(": {off}"))),
            "{why}を止めたら当たりでも止まる: {err:?}"
        );
    }
    // 戻せば、また当たる（キャッシュは残っている）。
    gw.update_tenant_settings(
        &admin,
        &SettingsPatch {
            models: vec![("decide-fast".into(), true), ("fast".into(), true)],
            ..Default::default()
        },
    )
    .await
    .unwrap()
    .unwrap();
    assert!(
        gw.decide_cached(&flow, &req, &call("wf:r4:ai"), &scope(wf))
            .await
            .unwrap()
            .cached
    );
    assert_eq!(usage_keys(&pool, &t).await, ["wf:r1:ai"]);
    cleanup(&pool, &t).await;
}

/// 範囲はテナント＋組織＋ワークフロー。別のワークフロー・別の組織では当たらない。
#[tokio::test]
async fn the_cache_is_scoped_to_the_tenant_org_and_workflow() {
    let Some(pool) = pool().await else { return };
    let gw = gateway(pool.clone(), "paid-unused", UsageLimitsConfig::default());
    let t = tenant();
    let flow = workflow_in(&t, "org");
    let req = request("decide-stub", "billing の件");
    let wf = Uuid::new_v4();
    assert!(
        !gw.decide_cached(&flow, &req, &call("wf:a:1"), &scope(wf))
            .await
            .unwrap()
            .cached
    );
    // 別のワークフロー（同じ組織）。
    let other_wf = gw
        .decide_cached(&flow, &req, &call("wf:b:1"), &scope(Uuid::new_v4()))
        .await
        .unwrap();
    assert!(!other_wf.cached, "別のワークフローでは当たらない");
    // 別の組織（同じテナント・同じワークフローの ID）。
    let other_org = gw
        .decide_cached(&workflow_in(&t, "org-2"), &req, &call("wf:c:1"), &scope(wf))
        .await
        .unwrap();
    assert!(!other_org.cached, "別の組織では当たらない");
    // 別のテナント（同じ組織名・同じワークフローの ID）。
    let t2 = tenant();
    let other_tenant = gw
        .decide_cached(&workflow_in(&t2, "org"), &req, &call("wf:d:1"), &scope(wf))
        .await
        .unwrap();
    assert!(!other_tenant.cached, "別のテナントでは当たらない");
    // 元の範囲では当たる。
    assert!(
        gw.decide_cached(&flow, &req, &call("wf:a:2"), &scope(wf))
            .await
            .unwrap()
            .cached
    );
    assert_eq!(cache_rows(&pool, &t).await, 3);
    cleanup(&pool, &t).await;
    cleanup(&pool, &t2).await;
}

/// 期限を過ぎた答え・ノードの今の期限より古い答えは当たらない。期限切れの行は書き込みで消える。
#[tokio::test]
async fn expired_answers_miss_and_are_purged_on_write() {
    let Some(pool) = pool().await else { return };
    let gw = gateway(pool.clone(), "paid-unused", UsageLimitsConfig::default());
    let t = tenant();
    let flow = workflow_in(&t, "org");
    let wf = Uuid::new_v4();
    let req = request("decide-stub", "billing");
    gw.decide_cached(&flow, &req, &call("wf:r1:ai"), &scope(wf))
        .await
        .unwrap();
    // 1 分前に書いたことにする（期限はまだ先）。ノードの ttl を 30 秒に縮めたら、その答えは古いので外れ。
    sqlx::query(
        "UPDATE llm_decision_cache SET created_at = now() - interval '1 minute' WHERE tenant_id = $1",
    )
    .bind(&t)
    .execute(&pool)
    .await
    .unwrap();
    let short = DecisionCache {
        workflow_id: wf,
        ttl: Duration::from_secs(30),
    };
    assert!(
        !gw.decide_cached(&flow, &req, &call("wf:r2:ai"), &short)
            .await
            .unwrap()
            .cached
    );
    // 期限を過ぎた行（別の材料のもの）は、次の書き込みで消える。
    gw.decide_cached(
        &flow,
        &request("decide-stub", "bug"),
        &call("wf:r3:ai"),
        &scope(wf),
    )
    .await
    .unwrap();
    sqlx::query(
        "UPDATE llm_decision_cache SET expires_at = now() - interval '1 second' \
          WHERE tenant_id = $1 AND resolved_model = 'stub-decider/v1'",
    )
    .bind(&t)
    .execute(&pool)
    .await
    .unwrap();
    assert!(
        !gw.decide_cached(&flow, &req, &call("wf:r4:ai"), &scope(wf))
            .await
            .unwrap()
            .cached,
        "期限を過ぎた答えは当たらない"
    );
    // 直前の書き込み（r4 の答え）で期限切れの行は消え、r4 の 1 行だけが残る。
    assert_eq!(cache_rows(&pool, &t).await, 1);
    cleanup(&pool, &t).await;
}

/// 当たりは利用上限を確かめない（費用が出ない）。失敗した判定は使い回さない。
#[tokio::test]
async fn hits_skip_usage_limits_and_failures_are_not_cached() {
    let Some(pool) = pool().await else { return };
    let paid = format!("paid-{}", Uuid::new_v4().simple());
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
    let t = tenant();
    let flow = workflow_in(&t, "org");
    let wf = Uuid::new_v4();
    let req = request("decide-paid", "請求");
    gw.decide_cached(&flow, &req, &call("wf:r1:ai"), &scope(wf))
        .await
        .unwrap();
    // 枠を使い切ったので、外れ（別の材料）は止まる。
    let err = gw
        .decide_cached(
            &flow,
            &request("decide-paid", "別の件"),
            &call("wf:r2:ai"),
            &scope(wf),
        )
        .await
        .unwrap_err();
    assert!(matches!(err, LlmError::RateLimited(_)), "{err:?}");
    // 当たりは費用が出ないので通る。
    assert!(
        gw.decide_cached(&flow, &req, &call("wf:r3:ai"), &scope(wf))
            .await
            .unwrap()
            .cached
    );
    // 失敗した判定は書かない（次も決定器を呼ぶ・使った分は #f で会計に載る）。
    let failing = request("decide-stub", "stub:fail");
    for key in ["wf:r4:ai", "wf:r5:ai"] {
        let err = gw
            .decide_cached(&flow, &failing, &call(key), &scope(wf))
            .await
            .unwrap_err();
        assert!(matches!(err, LlmError::Unavailable(_)), "{err:?}");
    }
    let keys = usage_keys(&pool, &t).await;
    assert_eq!(
        keys.iter().filter(|k| k.contains("#f")).count(),
        2,
        "失敗は 2 回とも決定器を呼ぶ: {keys:?}"
    );
    assert_eq!(cache_rows(&pool, &t).await, 1);
    // 答えの中身は 1 回目と同じ（stub の決定器が選んだ請求）。
    let hit = gw
        .decide_cached(&flow, &req, &call("wf:r6:ai"), &scope(wf))
        .await
        .unwrap();
    assert!(matches!(
        &hit.response.answers["category"],
        Answer::Choice { .. }
    ));
    cleanup(&pool, &t).await;
}

/// 引き上げた質問（BUG-124・D-114）は答えと一緒にキャッシュに残り、当たりでも返る。項目の無い古い行（D-114 の前に
/// 書いた答え）も当たり、引き上げた質問は空として読む。
#[tokio::test]
async fn raised_questions_survive_the_cache_and_old_rows_read_as_none() {
    let Some(pool) = pool().await else { return };
    let gw = gateway(pool.clone(), "paid-unused", UsageLimitsConfig::default());
    let t = tenant();
    let flow = workflow_in(&t, "org");
    let wf = Uuid::new_v4();
    // fast（stub）に判定させる決定器は、stub が自信を 0 で答えるので、選択肢の質問を一様（1/2）へ引き上げる。
    let req = request("decide-fast", "請求について");
    let first = gw
        .decide_cached(&flow, &req, &call("wf:r1:ai"), &scope(wf))
        .await
        .unwrap();
    assert!(!first.cached);
    assert_eq!(first.response.raised_questions, ["category"]);
    let hit = gw
        .decide_cached(&flow, &req, &call("wf:r2:ai"), &scope(wf))
        .await
        .unwrap();
    assert!(hit.cached);
    assert_eq!(hit.response.raised_questions, ["category"]);

    // D-114 の前に書いた行の形（引き上げた質問の項目が無い）に戻しても、当たって読める。
    let stripped = sqlx::query(
        "UPDATE llm_decision_cache SET response = response - 'raised_questions' \
          WHERE tenant_id = $1 AND response ? 'raised_questions'",
    )
    .bind(&t)
    .execute(&pool)
    .await
    .unwrap();
    assert_eq!(stripped.rows_affected(), 1);
    let old = gw
        .decide_cached(&flow, &req, &call("wf:r3:ai"), &scope(wf))
        .await
        .unwrap();
    assert!(old.cached, "古い行も当たる");
    assert!(old.response.raised_questions.is_empty());
    assert_eq!(old.response.answers, first.response.answers);
    assert_eq!(usage_keys(&pool, &t).await, ["wf:r1:ai"]);
    cleanup(&pool, &t).await;
}
