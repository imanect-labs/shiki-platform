//! テナントの AI 設定と利用量集計の結合テスト（#553・実 Postgres）。
//!
//! `STORAGE_TEST_DATABASE_URL` が設定されている時のみ実行（未設定はスキップ）。
//! - 設定の既定（全モデル有効・Auto）と、更新→読み戻し→モデル指定の解決（許可外は拒否）。
//! - 会計（record_generation）に刻んだ行が、利用量集計（合計・日別・モデル/ユーザー/利用元別）に
//!   そのまま出て、別テナント・別 org の行が混ざらないこと。

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::print_stderr
)]

use authz::{AuthContext, Principal};
use chrono::{Duration, Utc};
use llm_gateway::{
    ChoiceError, Effort, GatewayConfig, GenerationRecord, LlmGateway, ModelCatalog, ModelChoice,
    ModelEntry, SettingsPatch, Usage, UsageFilter, UsageSource, AUTO_MODEL,
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

/// stub プロバイダ 1 つに 2 モデル（a: 1 Mtok あたり $1 / $2、b: 単価なし）。
fn gateway(pool: PgPool) -> LlmGateway {
    let config = GatewayConfig {
        catalog: ModelCatalog {
            default_model: "a".into(),
            models: vec![
                ModelEntry {
                    id: "a".into(),
                    display_name: Some("モデル A".into()),
                    prompt_price_micros_per_mtok: 1_000_000,
                    completion_price_micros_per_mtok: 2_000_000,
                    ..Default::default()
                },
                ModelEntry {
                    id: "b".into(),
                    ..Default::default()
                },
            ],
        },
        ..Default::default()
    };
    LlmGateway::build(pool, reqwest::Client::new(), config).expect("gateway")
}

async fn record(
    gw: &LlmGateway,
    ctx: &AuthContext,
    model: &str,
    tokens: (u64, u64),
    source: UsageSource,
) {
    gw.record_generation(
        ctx,
        &GenerationRecord {
            idempotency_key: uuid::Uuid::new_v4().to_string(),
            model: model.into(),
            usage: Usage {
                prompt_tokens: tokens.0,
                completion_tokens: tokens.1,
            },
            trace_id: None,
            input_preview: String::new(),
            output_preview: String::new(),
            app_id: None,
            source,
        },
    )
    .await;
}

#[tokio::test]
async fn tenant_settings_default_update_and_resolve() {
    let Some(pool) = pool().await else { return };
    let gw = gateway(pool);
    let tenant = format!("t-{}", uuid::Uuid::new_v4());
    let alice = ctx(&tenant, "acme", "alice");

    // 既定: 全モデル有効・Auto。Auto の振り先（gpt-6-luna）はカタログに無いので既定モデルへ落ちる。
    let s = gw.tenant_settings(&alice).await.unwrap();
    assert_eq!(s.default_model, AUTO_MODEL);
    assert!(s.disabled_models.is_empty());
    let r = gw
        .resolve_choice(&alice, &ModelChoice::default())
        .await
        .unwrap()
        .unwrap();
    assert_eq!((r.model.as_str(), r.auto), ("a", true));

    // 更新: b を無効化・Auto の振り先を a/high・円換算 150.25 円。
    let next = gw
        .update_tenant_settings(
            &alice,
            &SettingsPatch {
                models: vec![("b".into(), false)],
                auto_model: Some(Some("a".into())),
                auto_effort: Some(Some(Effort::High)),
                jpy_per_usd_centi: Some(Some(15_025)),
                ..Default::default()
            },
        )
        .await
        .unwrap()
        .unwrap();
    assert_eq!(next.disabled_models, vec!["b".to_string()]);
    let reloaded = gw.tenant_settings(&alice).await.unwrap();
    assert_eq!(reloaded, next);
    assert_eq!(reloaded.jpy_per_usd_centi, Some(15_025));

    // 無効化したモデルの指定は拒否される（API は 400）。
    let denied = gw
        .resolve_choice(
            &alice,
            &ModelChoice {
                model: Some("b".into()),
                effort: None,
            },
        )
        .await
        .unwrap();
    assert_eq!(denied, Err(ChoiceError::ModelNotAllowed("b".into())));

    // 全モデル無効は検証で止まり、保存されない。
    let invalid = gw
        .update_tenant_settings(
            &alice,
            &SettingsPatch {
                models: vec![("a".into(), false)],
                ..Default::default()
            },
        )
        .await
        .unwrap();
    assert!(invalid.is_err());
    assert_eq!(gw.tenant_settings(&alice).await.unwrap(), next);

    // 別テナントは影響を受けない。
    let other = ctx(&format!("t-{}", uuid::Uuid::new_v4()), "acme", "bob");
    assert!(gw
        .tenant_settings(&other)
        .await
        .unwrap()
        .disabled_models
        .is_empty());
}

#[tokio::test]
async fn usage_report_matches_accounting_and_stays_in_tenant_and_org() {
    let Some(pool) = pool().await else { return };
    let gw = gateway(pool);
    let tenant = format!("t-{}", uuid::Uuid::new_v4());
    let alice = ctx(&tenant, "acme", "alice");
    let bob = ctx(&tenant, "acme", "bob");

    // a: 1M+0.5M トークン → $1 + $1 = 1_000_000 + 1_000_000 マイクロ USD。
    record(&gw, &alice, "a", (1_000_000, 500_000), UsageSource::Chat).await;
    record(&gw, &bob, "a", (1_000_000, 0), UsageSource::Agent).await;
    record(&gw, &bob, "b", (10, 20), UsageSource::Workflow).await;
    // 混ざってはいけない行: 別 org・別テナント。
    record(
        &gw,
        &ctx(&tenant, "other-org", "carol"),
        "a",
        (5_000_000, 0),
        UsageSource::Chat,
    )
    .await;
    record(
        &gw,
        &ctx(&format!("t-{}", uuid::Uuid::new_v4()), "acme", "dave"),
        "a",
        (5_000_000, 0),
        UsageSource::Chat,
    )
    .await;

    let today = (Utc::now() + Duration::hours(9)).date_naive();
    let report = gw
        .usage_report(
            &alice,
            &UsageFilter {
                from: today - Duration::days(1),
                to: today,
                model: None,
                source: None,
            },
        )
        .await
        .unwrap();
    assert_eq!(report.totals.calls, 3);
    assert_eq!(report.totals.users, 2);
    assert_eq!(report.totals.prompt_tokens, 2_000_010);
    assert_eq!(report.totals.completion_tokens, 500_020);
    assert_eq!(report.totals.cost_usd_micros, 3_000_000);

    assert_eq!(report.daily.len(), 1);
    assert_eq!(report.daily[0].day, today);
    assert_eq!(report.daily[0].calls, 3);

    // 内訳はコストの大きい順。
    assert_eq!(report.by_model[0].key, "a");
    assert_eq!(report.by_model[0].cost_usd_micros, 3_000_000);
    assert_eq!(report.by_model[1].key, "b");
    let sources: Vec<&str> = report.by_source.iter().map(|r| r.key.as_str()).collect();
    assert_eq!(sources, vec!["chat", "agent", "workflow"]);
    let users: Vec<&str> = report.by_user.iter().map(|r| r.key.as_str()).collect();
    assert_eq!(users, vec!["alice", "bob"]);

    // 絞り込み（モデル・利用元）。
    let only_b = gw
        .usage_report(
            &alice,
            &UsageFilter {
                from: today,
                to: today,
                model: Some("b".into()),
                source: None,
            },
        )
        .await
        .unwrap();
    assert_eq!(only_b.totals.calls, 1);
    let only_agent = gw
        .usage_report(
            &alice,
            &UsageFilter {
                from: today,
                to: today,
                model: None,
                source: Some("agent".into()),
            },
        )
        .await
        .unwrap();
    assert_eq!(only_agent.totals.calls, 1);
    assert_eq!(only_agent.by_user[0].key, "bob");

    // 期間外（明日以降）は 0 件。
    let future = gw
        .usage_report(
            &alice,
            &UsageFilter {
                from: today + Duration::days(1),
                to: today + Duration::days(2),
                model: None,
                source: None,
            },
        )
        .await
        .unwrap();
    assert_eq!(future.totals.calls, 0);
    assert!(future.daily.is_empty());
}
