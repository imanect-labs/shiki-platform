//! 利用制限（月次コスト上限・二段）の結合テスト（実 Postgres）。
//!
//! `STORAGE_TEST_DATABASE_URL` が設定されている時のみ実行（未設定はスキップ）。
//! - 既定（無効）では会計をいくら積んでも `stream` が止まらない。
//! - ユーザー単位の上限: 超えた本人だけが止まり、同じ組織の別ユーザーは通る。
//! - プロバイダ別の環境総枠: **テナント横断**で数え、枠のあるプロバイダのモデルなら通る。
//! - 会計に `provider_id`（デプロイ設定のプロバイダ ID）が刻まれる。

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::print_stderr
)]

use authz::{AuthContext, Principal};
use llm_gateway::{
    GatewayConfig, GenerateRequest, GenerationRecord, LlmError, LlmGateway, Message, ModelCatalog,
    ModelEntry, ProviderConfig, ProviderKind, ProviderLimit, Role, Usage, UsageLimitsConfig,
    UsageSource,
};
use sqlx::postgres::PgPoolOptions;
use sqlx::PgPool;

async fn pool() -> Option<PgPool> {
    let Ok(db_url) = std::env::var("STORAGE_TEST_DATABASE_URL") else {
        eprintln!("STORAGE_TEST_DATABASE_URL 未設定のためスキップ");
        return None;
    };
    let pool = PgPoolOptions::new()
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

fn ctx(tenant: &str, org: &str, user: &str) -> AuthContext {
    AuthContext::new(
        Principal {
            kind: authz::PrincipalKind::User,
            id: user.into(),
            email: None,
            groups: vec![],
            roles: vec![],
            tenant_id: Some(tenant.into()),
        },
        org.into(),
        tenant.into(),
    )
}

/// テストごとに一意な追加プロバイダ ID。
fn paid_id() -> String {
    format!("paid-{}", uuid::Uuid::new_v4())
}

/// 既定プロバイダ（`default`）のモデル `cheap` と、追加プロバイダ `paid_id` のモデル `costly`。
/// どちらも stub（外部依存なし）。単価は 1 Mtok あたり $1。
///
/// **プロバイダ ID はテストごとに一意にする。** 環境総枠はテナント横断で数えるので、ID を
/// 共有すると同じ DB を使う他のテストの消費が混ざる（制限の仕様どおりの振る舞い）。
fn gateway(pool: PgPool, paid_id: &str, limits: UsageLimitsConfig) -> LlmGateway {
    let config = GatewayConfig {
        extra_providers: vec![ProviderConfig {
            id: paid_id.into(),
            display_name: Some("有料プロバイダ".into()),
            kind: ProviderKind::Stub,
            ..Default::default()
        }],
        catalog: ModelCatalog {
            default_model: "cheap".into(),
            models: vec![
                ModelEntry {
                    id: "cheap".into(),
                    prompt_price_micros_per_mtok: 1_000_000,
                    ..Default::default()
                },
                ModelEntry {
                    id: "costly".into(),
                    provider: Some(paid_id.into()),
                    prompt_price_micros_per_mtok: 1_000_000,
                    ..Default::default()
                },
            ],
        },
        limits,
        ..Default::default()
    };
    LlmGateway::build(pool, reqwest::Client::new(), config).expect("gateway")
}

/// 会計へ `prompt_tokens` 分（＝同額のマイクロ USD）積む。
async fn record(gw: &LlmGateway, ctx: &AuthContext, model: &str, prompt_tokens: u64) {
    gw.record_generation(
        ctx,
        &GenerationRecord {
            idempotency_key: uuid::Uuid::new_v4().to_string(),
            model: model.into(),
            usage: Usage {
                prompt_tokens,
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
}

async fn stream(gw: &LlmGateway, ctx: &AuthContext, model: &str) -> Result<(), LlmError> {
    let mut req = GenerateRequest::new(vec![Message::text(Role::User, "hi")]);
    req.model = Some(model.into());
    gw.stream(ctx, req).await.map(|_| ())
}

/// 既定（無効）: 会計を積んでも止まらない。
#[tokio::test]
async fn disabled_limits_never_reject() {
    let Some(pool) = pool().await else { return };
    let gw = gateway(pool, &paid_id(), UsageLimitsConfig::default());
    let tenant = format!("t-{}", uuid::Uuid::new_v4());
    let alice = ctx(&tenant, "acme", "alice");

    record(&gw, &alice, "cheap", 10_000_000).await; // $10
    stream(&gw, &alice, "cheap").await.expect("無効なら通る");
}

/// ユーザー単位の上限は超えた本人だけを止める（同じ組織の別ユーザーは通る）。
#[tokio::test]
async fn per_user_limit_stops_only_that_user() {
    let Some(pool) = pool().await else { return };
    let gw = gateway(
        pool,
        &paid_id(),
        UsageLimitsConfig {
            enabled: true,
            per_user_usd_micros: Some(1_000_000), // $1
            providers: vec![],
        },
    );
    let tenant = format!("t-{}", uuid::Uuid::new_v4());
    let alice = ctx(&tenant, "acme", "alice");
    let bob = ctx(&tenant, "acme", "bob");

    // 上限未満は通る。
    record(&gw, &alice, "cheap", 999_999).await;
    stream(&gw, &alice, "cheap").await.expect("上限未満は通る");

    // 到達したら 429 相当。文言は金額を含み、そのままユーザーへ出せる。
    record(&gw, &alice, "cheap", 1).await;
    let err = stream(&gw, &alice, "cheap").await.unwrap_err();
    match err {
        LlmError::RateLimited(msg) => {
            assert!(msg.contains("$1.00"), "{msg}");
            assert!(msg.contains("今月"), "{msg}");
        }
        other => panic!("RateLimited を期待: {other:?}"),
    }

    // 別ユーザーは自分の枠で通る（1 人の使い過ぎで全員が止まらない）。
    stream(&gw, &bob, "cheap").await.expect("別ユーザーは通る");
}

/// 本人の当月の利用額（#965）: 上限の判定と同じ集計で、本人の分だけを返す。上限が無効なら
/// 集計しない（`None`）。期間は日本時間の今月 1 日から来月 1 日まで。
#[tokio::test]
async fn month_usage_reports_only_my_spend_and_the_limit() {
    let Some(pool) = pool().await else { return };
    let limited = gateway(
        pool.clone(),
        &paid_id(),
        UsageLimitsConfig {
            enabled: true,
            per_user_usd_micros: Some(5_000_000), // $5
            providers: vec![],
        },
    );
    let tenant = format!("t-{}", uuid::Uuid::new_v4());
    let alice = ctx(&tenant, "acme", "alice");
    let bob = ctx(&tenant, "acme", "bob");

    record(&limited, &alice, "cheap", 1_250_000).await; // $1.25
    record(&limited, &alice, "costly", 250_000).await; // 別プロバイダも合算する
    record(&limited, &bob, "cheap", 9_000_000).await; // 他人の分は混ざらない

    let mine = limited.month_usage(&alice).await.unwrap();
    assert_eq!(mine.used_usd_micros, Some(1_500_000));
    assert_eq!(mine.limit_usd_micros, Some(5_000_000));
    assert!(mine.period_start <= chrono::Utc::now() && chrono::Utc::now() < mine.period_end);
    let span = mine.period_end - mine.period_start;
    assert!(
        span >= chrono::Duration::days(28) && span <= chrono::Duration::days(31),
        "{span}"
    );

    // 上限が無効なら集計しない（利用額・上限とも None。期間は返す）。
    let unlimited = gateway(pool, &paid_id(), UsageLimitsConfig::default());
    let mine = unlimited.month_usage(&alice).await.unwrap();
    assert_eq!(mine.used_usd_micros, None);
    assert_eq!(mine.limit_usd_micros, None);
    assert!(mine.period_start < mine.period_end);
}

/// プロバイダ別の環境総枠はテナント横断で数え、別プロバイダのモデルは通る。
#[tokio::test]
async fn provider_limit_counts_across_tenants() {
    let Some(pool) = pool().await else { return };
    let provider_budget = 1_000_000; // $1
    let paid = paid_id();
    let gw = gateway(
        pool,
        &paid,
        UsageLimitsConfig {
            enabled: true,
            per_user_usd_micros: None,
            providers: vec![ProviderLimit {
                providers: vec![paid.clone()],
                usd_micros: provider_budget,
            }],
        },
    );
    // 別テナント・別ユーザーで積んでも、同じプロバイダなら同じ枠を食う。
    let t1 = format!("t-{}", uuid::Uuid::new_v4());
    let t2 = format!("t-{}", uuid::Uuid::new_v4());
    let alice = ctx(&t1, "acme", "alice");
    let carol = ctx(&t2, "globex", "carol");

    record(&gw, &alice, "costly", 600_000).await;
    stream(&gw, &carol, "costly").await.expect("枠内は通る");

    record(&gw, &carol, "costly", 400_000).await;
    let err = stream(&gw, &alice, "costly").await.unwrap_err();
    match err {
        LlmError::RateLimited(msg) => {
            // 文言は表示名で言う（プロバイダ ID は出さない）。
            assert!(msg.contains("有料プロバイダ"), "{msg}");
            assert!(!msg.contains(&paid), "{msg}");
        }
        other => panic!("RateLimited を期待: {other:?}"),
    }

    // 枠を設定していないプロバイダ（既定）のモデルは通る。
    stream(&gw, &alice, "cheap")
        .await
        .expect("枠のないプロバイダは通る");
}

/// 会計に振り先プロバイダの ID が刻まれる（環境総枠がこの列で数える）。
#[tokio::test]
async fn accounting_records_provider_id() {
    let Some(pool) = pool().await else { return };
    let paid = paid_id();
    let gw = gateway(pool.clone(), &paid, UsageLimitsConfig::default());
    let tenant = format!("t-{}", uuid::Uuid::new_v4());
    let alice = ctx(&tenant, "acme", "alice");

    record(&gw, &alice, "costly", 1_000).await;
    record(&gw, &alice, "cheap", 1_000).await;

    let rows: Vec<(String, Option<String>)> = sqlx::query_as(
        "SELECT model, provider_id FROM llm_usage WHERE tenant_id = $1 ORDER BY model",
    )
    .bind(&tenant)
    .fetch_all(&pool)
    .await
    .unwrap();
    assert_eq!(
        rows,
        vec![
            ("cheap".to_string(), Some("default".to_string())),
            ("costly".to_string(), Some(paid)),
        ]
    );
}

/// 1 つの API キーを複数のプロバイダ設定で使う場合、枠は**合算**で数える
/// （OpenCode Go のように API 形式ごとにプロバイダを分けても請求は 1 つ）。
#[tokio::test]
async fn one_budget_is_shared_across_provider_ids() {
    let Some(pool) = pool().await else { return };
    let paid = paid_id();
    let other = format!("{paid}-alt");
    // `paid`（モデル costly）と `other`（モデル costly_alt）で枠を共有する。
    let config = GatewayConfig {
        extra_providers: vec![
            ProviderConfig {
                id: paid.clone(),
                display_name: Some("同じ提供元".into()),
                kind: ProviderKind::Stub,
                ..Default::default()
            },
            ProviderConfig {
                id: other.clone(),
                display_name: Some("同じ提供元".into()),
                kind: ProviderKind::Stub,
                ..Default::default()
            },
        ],
        catalog: ModelCatalog {
            default_model: "costly".into(),
            models: vec![
                ModelEntry {
                    id: "costly".into(),
                    provider: Some(paid.clone()),
                    prompt_price_micros_per_mtok: 1_000_000,
                    ..Default::default()
                },
                ModelEntry {
                    id: "costly_alt".into(),
                    provider: Some(other.clone()),
                    prompt_price_micros_per_mtok: 1_000_000,
                    ..Default::default()
                },
            ],
        },
        limits: UsageLimitsConfig {
            enabled: true,
            per_user_usd_micros: None,
            providers: vec![ProviderLimit {
                providers: vec![paid.clone(), other.clone()],
                usd_micros: 1_000_000, // $1 を 2 つで共有
            }],
        },
        ..Default::default()
    };
    let gw = LlmGateway::build(pool, reqwest::Client::new(), config).expect("gateway");
    let tenant = format!("t-{}", uuid::Uuid::new_v4());
    let alice = ctx(&tenant, "acme", "alice");

    // 片方で $0.6 使っても、まだ枠は残っている。
    record(&gw, &alice, "costly", 600_000).await;
    stream(&gw, &alice, "costly_alt").await.expect("枠内は通る");

    // もう片方で $0.4 使うと合算で $1 到達 → どちらのモデルも止まる。
    record(&gw, &alice, "costly_alt", 400_000).await;
    assert!(matches!(
        stream(&gw, &alice, "costly").await,
        Err(LlmError::RateLimited(_))
    ));
    assert!(matches!(
        stream(&gw, &alice, "costly_alt").await,
        Err(LlmError::RateLimited(_))
    ));
}
