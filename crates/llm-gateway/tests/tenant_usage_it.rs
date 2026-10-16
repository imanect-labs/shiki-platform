//! テナントの AI 設定と利用量集計の結合テスト（#553・実 Postgres）。
//!
//! `STORAGE_TEST_DATABASE_URL` が設定されている時のみ実行（未設定はスキップ）。
//! - 設定の既定（全モデル有効・Auto）と、更新→読み戻し→モデル指定の解決（許可外は拒否）。
//! - 無効化したモデルは gateway の `stream` が拒否し、別テナント・別 org には効かない。
//! - 許可リスト: 保存後にカタログへ足したモデルは、管理者が有効にするまで使えない。
//! - 同時更新が直列化される（変更が消えない・既定が無効のまま残らない）。
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
    ChoiceError, Effort, GatewayConfig, GenerateRequest, GenerationRecord, LlmError, LlmGateway,
    Message, ModelCatalog, ModelChoice, ModelEntry, Role, SettingsPatch, Usage, UsageFilter,
    UsageSource, AUTO_MODEL,
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
    gateway_with(pool, vec![])
}

/// [`gateway`] のカタログに `extra` を足したもの（デプロイ設定にモデルを追加した後の状態）。
fn gateway_with(pool: PgPool, extra: Vec<ModelEntry>) -> LlmGateway {
    let mut config = GatewayConfig {
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
    config.catalog.models.extend(extra);
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
                ..Default::default()
            },
            trace_id: None,
            input_preview: String::new(),
            output_preview: String::new(),
            app_id: None,
            source,
            first_in_trace: true,
            started_at: None,
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
    assert_eq!(s.allowed_models, None);
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
    assert_eq!(next.allowed_models, Some(vec!["a".to_string()]));
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

    // チョークポイント: 無効化したモデルは経路によらず gateway が生成を拒否する。
    let mut req = GenerateRequest::new(vec![Message::text(Role::User, "hi")]);
    req.model = Some("b".into());
    assert!(matches!(
        gw.stream(&alice, req.clone()).await,
        Err(LlmError::Config(_))
    ));

    // 別テナント・同じテナントの別 org は影響を受けない（設定の粒度は tenant_id ＋ org）。
    let other = ctx(&format!("t-{}", uuid::Uuid::new_v4()), "acme", "bob");
    let sibling = ctx(&tenant, "other-org", "carol");
    for c in [&other, &sibling] {
        assert_eq!(gw.tenant_settings(c).await.unwrap().allowed_models, None);
        assert!(gw.stream(c, req.clone()).await.is_ok());
    }
}

/// 許可リスト: 保存した組織では、後からカタログに足したモデルは管理者が有効にするまで使えない。
/// 未保存の組織はカタログの既定（`requires_opt_in` 以外）を使える。
#[tokio::test]
async fn models_added_after_saving_stay_off_until_enabled() {
    let Some(pool) = pool().await else { return };
    let tenant = format!("t-{}", uuid::Uuid::new_v4());
    let saved = ctx(&tenant, "saved-org", "alice");
    let unsaved = ctx(&tenant, "unsaved-org", "bob");

    // 追加前に保存（レートだけ変えても許可リストは確定する）。
    let before = gateway(pool.clone());
    let rate = SettingsPatch {
        jpy_per_usd_centi: Some(Some(15_000)),
        ..Default::default()
    };
    before
        .update_tenant_settings(&saved, &rate)
        .await
        .unwrap()
        .unwrap();

    let after = gateway_with(
        pool,
        vec![
            ModelEntry {
                id: "new".into(),
                ..Default::default()
            },
            ModelEntry {
                id: "abroad".into(),
                processed_abroad: true,
                requires_opt_in: true,
                ..Default::default()
            },
        ],
    );
    let req = |m: &str| {
        let mut r = GenerateRequest::new(vec![Message::text(Role::User, "hi")]);
        r.model = Some(m.into());
        r
    };
    assert!(matches!(
        after.stream(&saved, req("new")).await,
        Err(LlmError::Config(_))
    ));
    assert!(after.stream(&unsaved, req("new")).await.is_ok());
    assert!(matches!(
        after.stream(&unsaved, req("abroad")).await,
        Err(LlmError::Config(_))
    ));

    // 管理者が有効にすれば使える。
    let enable = SettingsPatch {
        models: vec![("new".into(), true), ("abroad".into(), true)],
        ..Default::default()
    };
    after
        .update_tenant_settings(&saved, &enable)
        .await
        .unwrap()
        .unwrap();
    assert!(after.stream(&saved, req("new")).await.is_ok());
    assert!(after.stream(&saved, req("abroad")).await.is_ok());
}

/// 同時に来た更新は直列化され、両方の変更が残り、検証済みの不変条件も崩れない。
#[tokio::test]
async fn concurrent_updates_do_not_lose_changes_or_break_invariants() {
    let Some(pool) = pool().await else { return };
    let gw = gateway(pool);
    let tenant = format!("t-{}", uuid::Uuid::new_v4());
    let alice = ctx(&tenant, "acme", "alice");
    let bob = ctx(&tenant, "acme", "bob");

    // 「b を無効化」と「既定を b に」は片方しか通らない（両方通ると既定が無効のまま残る）。
    let disable_b = SettingsPatch {
        models: vec![("b".into(), false)],
        ..Default::default()
    };
    let default_b = SettingsPatch {
        default_model: Some("b".into()),
        ..Default::default()
    };
    let (r1, r2) = tokio::join!(
        gw.update_tenant_settings(&alice, &disable_b),
        gw.update_tenant_settings(&bob, &default_b),
    );
    let oks = [r1.unwrap().is_ok(), r2.unwrap().is_ok()];
    assert_eq!(oks.iter().filter(|ok| **ok).count(), 1, "{oks:?}");
    let s = gw.tenant_settings(&alice).await.unwrap();
    let b_allowed = s
        .allowed_models
        .as_ref()
        .is_some_and(|l| l.contains(&"b".to_string()));
    assert!(s.default_model != "b" || b_allowed);

    // 別項目の同時更新は両方残る。
    let rate = SettingsPatch {
        jpy_per_usd_centi: Some(Some(14_000)),
        ..Default::default()
    };
    let choice = SettingsPatch {
        allow_user_choice: Some(false),
        ..Default::default()
    };
    let (r1, r2) = tokio::join!(
        gw.update_tenant_settings(&alice, &rate),
        gw.update_tenant_settings(&bob, &choice),
    );
    assert!(r1.unwrap().is_ok() && r2.unwrap().is_ok());
    let s = gw.tenant_settings(&alice).await.unwrap();
    assert_eq!(s.jpy_per_usd_centi, Some(14_000));
    assert!(!s.allow_user_choice);
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

/// キャッシュの内訳（#581）: 会計の行に読み出し・書き込みのトークンが刻まれ、金額はカタログの
/// キャッシュ単価で数え、利用量の合計にも内訳が出る。
#[tokio::test]
async fn cache_tokens_are_recorded_and_priced_separately() {
    let Some(pool) = pool().await else { return };
    // 入力 $1・読み出し $0.1・書き込み $1.25・出力 $2（per Mtok）。
    let gw = gateway_with(
        pool,
        vec![ModelEntry {
            id: "cached".into(),
            prompt_price_micros_per_mtok: 1_000_000,
            completion_price_micros_per_mtok: 2_000_000,
            cache_read_price_micros_per_mtok: Some(100_000),
            cache_write_price_micros_per_mtok: Some(1_250_000),
            ..Default::default()
        }],
    );
    let tenant = format!("t-{}", uuid::Uuid::new_v4());
    let alice = ctx(&tenant, "acme", "alice");
    let usage = |prompt, completion, read, write| Usage {
        prompt_tokens: prompt,
        completion_tokens: completion,
        cache_read_tokens: read,
        cache_write_tokens: write,
    };
    let rec = |u: Usage| GenerationRecord {
        idempotency_key: uuid::Uuid::new_v4().to_string(),
        model: "cached".into(),
        usage: u,
        trace_id: None,
        input_preview: String::new(),
        output_preview: String::new(),
        app_id: None,
        source: UsageSource::Chat,
        first_in_trace: true,
        started_at: None,
    };
    // 1 回目: 1M 入力をすべて書き込み → $1.25。2 回目: 0.9M 読み出し＋0.1M 定価＋出力 0.5M
    // → $0.09 + $0.1 + $1 = $1.19。キャッシュ単価なしで数えると $1 と $2.1。
    gw.record_generation(&alice, &rec(usage(1_000_000, 0, 0, 1_000_000)))
        .await;
    gw.record_generation(&alice, &rec(usage(1_000_000, 500_000, 900_000, 0)))
        .await;
    assert_eq!(
        gw.estimate_cost_usd_micros("cached", usage(1_000_000, 500_000, 900_000, 0)),
        1_190_000
    );

    let today = (Utc::now() + Duration::hours(9)).date_naive();
    let report = gw
        .usage_report(
            &alice,
            &UsageFilter {
                from: today,
                to: today,
                model: None,
                source: None,
            },
        )
        .await
        .unwrap();
    assert_eq!(report.totals.calls, 2);
    assert_eq!(report.totals.prompt_tokens, 2_000_000);
    assert_eq!(report.totals.cache_read_tokens, 900_000);
    assert_eq!(report.totals.cache_write_tokens, 1_000_000);
    assert_eq!(report.totals.cost_usd_micros, 1_250_000 + 1_190_000);
}
