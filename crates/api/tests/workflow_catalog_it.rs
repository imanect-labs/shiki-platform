//! ワークフロー検証カタログのモデル照合先（BUG-11）の結合テスト。
//!
//! 保存 API と emit_workflow が共有する `build_catalog_from` が、デプロイ設定ではなく
//! **呼び出し主体から見た組織の許可リスト**でモデルを照合することを、実 DB の組織の AI 設定で確かめる。
//! `STORAGE_TEST_DATABASE_URL` が設定されている時のみ実行。

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::print_stderr
)]

use api::workflow_catalog::{build_catalog_from, WorkflowModels};
use authz::{AuthContext, Principal, PrincipalKind};
use llm_gateway::{
    GatewayConfig, LlmGateway, ModelCatalog, ModelEntry, ProviderConfig, ProviderKind,
    SettingsPatch,
};
use serde_json::json;
use sqlx::postgres::PgPoolOptions;
use sqlx::PgPool;

async fn pool() -> Option<PgPool> {
    let Ok(url) = std::env::var("STORAGE_TEST_DATABASE_URL") else {
        eprintln!("STORAGE_TEST_DATABASE_URL 未設定のためスキップ");
        return None;
    };
    let pool = PgPoolOptions::new()
        .max_connections(4)
        .connect(&url)
        .await
        .expect("pg");
    sqlx::migrate!("../../migrations")
        .run(&pool)
        .await
        .expect("migrate");
    Some(pool)
}

/// 組織ごとに分けた利用者（テストどうしで組織の AI 設定を共有しない）。
fn user() -> AuthContext {
    let tenant = format!("wfcat-{}", uuid::Uuid::new_v4().simple());
    AuthContext::new(
        Principal {
            kind: PrincipalKind::User,
            id: "alice".into(),
            email: None,
            groups: vec![],
            roles: vec![],
            tenant_id: Some(tenant.clone()),
        },
        tenant.clone(),
        tenant,
    )
}

/// stub プロバイダに 3 モデル（`gated` は管理者が有効にするまで使えない）。
fn gateway(pool: PgPool) -> LlmGateway {
    let entry = |id: &str, requires_opt_in: bool| ModelEntry {
        id: id.into(),
        requires_opt_in,
        ..Default::default()
    };
    LlmGateway::build(
        pool,
        reqwest::Client::new(),
        GatewayConfig {
            provider: ProviderConfig {
                kind: ProviderKind::Stub,
                ..Default::default()
            },
            catalog: ModelCatalog {
                default_model: "fast".into(),
                models: vec![
                    entry("fast", false),
                    entry("local", false),
                    entry("gated", true),
                ],
            },
            ..Default::default()
        },
    )
    .expect("gateway")
}

fn llm_ir(model: &str) -> serde_json::Value {
    json!({
        "ir_version": 1, "name": "wf", "triggers": [{ "kind": "interactive" }],
        "nodes": [{ "id": "ask", "type": "llm.invoke", "params": { "model": model, "prompt": "hi" } }],
        "edges": []
    })
}

/// 未保存の組織はカタログの既定（opt-in でないもの）、保存後は許可リストで照合する。
#[tokio::test]
async fn models_follow_the_organization_allow_list() {
    let Some(pool) = pool().await else { return };
    let gw = gateway(pool);
    let ctx = user();
    let models = WorkflowModels::new(Some(gw.clone()), vec!["from-config".into()]);

    // 未保存: opt-in のモデルは使えない。デプロイ設定の id（from-config）は照合先にならない。
    assert_eq!(models.ids(&ctx).await.unwrap(), ["fast", "local"]);

    // 管理者が local を止め、gated を有効にする。
    let patch = SettingsPatch {
        models: vec![("local".into(), false), ("gated".into(), true)],
        ..Default::default()
    };
    gw.update_tenant_settings(&ctx, &patch)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(models.ids(&ctx).await.unwrap(), ["fast", "gated"]);

    let catalog = build_catalog_from(None, None, &models, &ctx).await.unwrap();
    assert_eq!(catalog.models, ["fast", "gated"]);
    // 止めたモデルは保存時に弾かれ、モデル欄の位置と使えるモデルが返る。
    let errs = workflow_engine::validate(&llm_ir("local"), &catalog).unwrap_err();
    let model_err = errs
        .iter()
        .find(|e| e.code == "ir.unknown_model")
        .unwrap_or_else(|| panic!("{errs:?}"));
    assert_eq!(model_err.path.as_deref(), Some("/params/model"));
    assert!(
        model_err.message.contains("使えるモデル: fast, gated"),
        "{}",
        model_err.message
    );
    // 有効にしたモデルは通る。
    workflow_engine::validate(&llm_ir("gated"), &catalog).unwrap();
    workflow_engine::validate(&llm_ir("fast"), &catalog).unwrap();

    // 別の組織には影響しない（組織ごとの設定）。
    let other = user();
    assert_eq!(models.ids(&other).await.unwrap(), ["fast", "local"]);
}

/// gateway が無い構成（LLM 未構成）は従来どおりデプロイ設定のモデル id で照合する。
#[tokio::test]
async fn without_a_gateway_the_configured_models_are_used() {
    let Some(_pool) = pool().await else { return };
    let ctx = user();
    let models = WorkflowModels::new(None, vec!["m1".into(), "m2".into()]);
    let catalog = build_catalog_from(None, None, &models, &ctx).await.unwrap();
    assert_eq!(catalog.models, ["m1", "m2"]);
    let none = WorkflowModels::new(None, Vec::new());
    let catalog = build_catalog_from(None, None, &none, &ctx).await.unwrap();
    assert!(catalog.models.is_empty(), "空なら照合しない（従来どおり）");
    workflow_engine::validate(&llm_ir("anything"), &catalog).unwrap();
}
