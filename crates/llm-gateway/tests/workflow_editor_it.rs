//! ワークフローを編集する人から見たモデルの設定（`settings_for_workflow_editor`・D-169・D-185）の結合テスト（実 Postgres）。
//!
//! `STORAGE_TEST_DATABASE_URL` が設定されている時のみ実行（未設定はスキップ）。
//! - 利用者ごとのプロバイダの制限（`user_provider_rules`）のある人は、許されないプロバイダのモデルを書けるモデルに
//!   持たない。既定・Auto の振り先は実行時の設定のまま残り、モデルを省いたノードが実行時に動くモデルが使えないことが
//!   分かる。
//! - 制限の無い人・ワークフローの主体（schedule・event の実行）には、実行時の設定そのもの。
//! - 実行時の解決（`resolve_workflow_choice`）は変えない: ワークフローの主体は制限されたプロバイダのモデルで動く
//!   （main の決まり）。組織が無効にしたモデルを個人の許可で復活させない。

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::print_stderr
)]

use authz::{AuthContext, Principal};
use llm_gateway::config::UserProviderRule;
use llm_gateway::{
    GatewayConfig, LlmGateway, Modality, ModelCatalog, ModelEntry, ProviderConfig, SettingsPatch,
    AUTO_MODEL,
};
use sqlx::postgres::PgPoolOptions;
use sqlx::PgPool;

async fn pool() -> Option<PgPool> {
    let Ok(db_url) = shiki_testenv::var("STORAGE_TEST_DATABASE_URL") else {
        eprintln!("STORAGE_TEST_DATABASE_URL 未設定のためスキップ");
        return None;
    };
    let pool = PgPoolOptions::new()
        .max_connections(3)
        .connect(&db_url)
        .await
        .expect("DB 接続");
    sqlx::migrate!("../../migrations")
        .run(&pool)
        .await
        .expect("マイグレーション適用");
    Some(pool)
}

fn user(tenant: &str, id: &str) -> AuthContext {
    AuthContext::new(
        Principal {
            kind: authz::PrincipalKind::User,
            id: id.into(),
            email: None,
            groups: vec![],
            roles: vec![],
            tenant_id: Some(tenant.into()),
        },
        "org".into(),
        authz::TenantId::parse(tenant).unwrap(),
    )
}

/// 既定のプロバイダの hidden（組織の既定・Auto の振り先）と judge（hidden を元にした判定用）、制限された人にも
/// 許すプロバイダ（allowed）の visible と decide（visible を元にした判定用）。
fn gateway(pool: PgPool, tenant: &str) -> LlmGateway {
    let decision = |id: &str, provider: Option<&str>, via: &str| ModelEntry {
        id: id.into(),
        provider: provider.map(str::to_string),
        modality: Modality::Decision,
        decision_via: Some(via.into()),
        ..Default::default()
    };
    let mut config = GatewayConfig {
        extra_providers: vec![ProviderConfig {
            id: "allowed".into(),
            ..Default::default()
        }],
        user_provider_rules: vec![UserProviderRule {
            tenant_id: tenant.into(),
            user_sub: "restricted".into(),
            providers: vec!["allowed".into()],
        }],
        catalog: ModelCatalog {
            default_model: "hidden".into(),
            models: vec![
                ModelEntry {
                    id: "hidden".into(),
                    ..Default::default()
                },
                ModelEntry {
                    id: "visible".into(),
                    provider: Some("allowed".into()),
                    ..Default::default()
                },
                decision("judge", None, "hidden"),
                decision("decide", Some("allowed"), "visible"),
            ],
        },
        ..Default::default()
    };
    config.auto.model = "hidden".into();
    LlmGateway::build(pool, reqwest::Client::new(), config).unwrap()
}

fn ids(models: Vec<&ModelEntry>) -> Vec<&str> {
    models.into_iter().map(|m| m.id.as_str()).collect()
}

#[tokio::test]
async fn editors_are_restricted_but_the_workflow_principal_is_not() {
    let Some(pool) = pool().await else {
        return;
    };
    let tenant = format!("wf-editor-{}", uuid::Uuid::new_v4().simple());
    let gw = gateway(pool, &tenant);
    let catalog = gw.catalog();
    let restricted = user(&tenant, "restricted");
    let other = user(&tenant, "other");
    let workflow = AuthContext::for_workflow(
        authz::TenantId::parse(&tenant).unwrap(),
        "org".into(),
        "restricted",
    );

    // 制限された人: 書けるのは許されたプロバイダのモデルだけ。組織の既定（未保存の組織は Auto）の振り先（hidden）は
    // 実行時のまま残り、省くと使えないモデルで動くことが分かる。
    let view = gw.settings_for_workflow_editor(&restricted).await.unwrap();
    assert_eq!(ids(view.chat_models(catalog)), ["visible"]);
    assert_eq!(ids(view.decision_models(catalog)), ["decide"]);
    assert_eq!(view.restricted(), ["hidden", "judge"]);
    assert_eq!(view.runtime().default_model, AUTO_MODEL);
    assert_eq!(view.default_target(), Some("hidden"));
    assert_eq!(view.default_restricted(), Some("hidden"));
    // チャットの見え方は Auto を使えるモデルに絞る（比べるため・#1014）。
    assert_eq!(
        gw.resolve_choice(&restricted, &llm_gateway::ModelChoice::default())
            .await
            .unwrap()
            .unwrap()
            .model,
        "visible"
    );

    // 制限の無い人とワークフローの主体: 実行時の設定そのもの。
    for ctx in [&other, &workflow] {
        let view = gw.settings_for_workflow_editor(ctx).await.unwrap();
        assert_eq!(ids(view.chat_models(catalog)), ["hidden", "visible"]);
        assert_eq!(ids(view.decision_models(catalog)), ["judge", "decide"]);
        assert!(view.restricted().is_empty());
        assert_eq!(view.default_restricted(), None);
    }

    // 実行時の解決は変えない: ワークフローの主体は、省いたモデルも明示したモデルも hidden で動く。
    let omitted = gw
        .resolve_workflow_choice(&workflow, None, None)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(omitted.model, "hidden");
    let explicit = gw
        .resolve_workflow_choice(&workflow, Some("hidden"), None)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(explicit.model, "hidden");

    // 既定を特定のモデル（hidden）にしても同じ。チャットの見え方は既定を Auto に差し替えるが、ワークフローは
    // 差し替えない（実行時に hidden で動く）。
    gw.update_tenant_settings(
        &other,
        &SettingsPatch {
            default_model: Some("hidden".into()),
            ..Default::default()
        },
    )
    .await
    .unwrap()
    .unwrap();
    let view = gw.settings_for_workflow_editor(&restricted).await.unwrap();
    assert_eq!(view.runtime().default_model, "hidden");
    assert_eq!(view.default_restricted(), Some("hidden"));
    assert_eq!(
        gw.settings_for(&restricted).await.unwrap().default_model,
        AUTO_MODEL
    );
    // 組織が hidden を無効にすると、Auto の振り先は visible に移り、省いてよくなる。組織が無効にしたモデルを制限の
    // 判定で復活させない。
    gw.update_tenant_settings(
        &other,
        &SettingsPatch {
            default_model: Some(AUTO_MODEL.into()),
            models: vec![("hidden".into(), false)],
            ..Default::default()
        },
    )
    .await
    .unwrap()
    .unwrap();
    let view = gw.settings_for_workflow_editor(&restricted).await.unwrap();
    assert_eq!(view.default_target(), Some("visible"));
    assert_eq!(view.default_restricted(), None);
    assert!(view.restricted().is_empty(), "{:?}", view.restricted());
    assert_eq!(ids(view.chat_models(catalog)), ["visible"]);
}
