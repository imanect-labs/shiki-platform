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
    ChatGptAccount, ChatGptSettings, GatewayConfig, LlmGateway, ModelCatalog, ModelEntry,
    ProviderConfig, ProviderKind, SettingsPatch, CHATGPT_PROVIDER_ID,
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
    assert_eq!(
        catalog.models.as_deref(),
        Some(&["fast", "gated"].map(String::from)[..])
    );
    assert_eq!(catalog.model_lock, None);
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
    assert_eq!(
        catalog.models.as_deref(),
        Some(&["m1", "m2"].map(String::from)[..])
    );
    let none = WorkflowModels::new(None, Vec::new());
    let catalog = build_catalog_from(None, None, &none, &ctx).await.unwrap();
    assert_eq!(catalog.models, None, "空なら照合しない（従来どおり）");
    workflow_engine::validate(&llm_ir("anything"), &catalog).unwrap();
}

/// 組織が許可したモデルが 0 件（本人専用だけ・LLM 未構成とは別）なら、明示したモデルを拒否する（BUG-37）。
/// model の省略は組織の既定に従うので通る（BUG-12）。
#[tokio::test]
async fn an_organization_without_usable_models_rejects_explicit_models() {
    let Some(pool) = pool().await else { return };
    let dir = std::env::temp_dir().join(format!("shiki-wfcat-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir_all(&dir).unwrap();
    let ctx = user();
    let gw = gateway_with_plan(pool, &ctx, &dir);
    // 管理者が本人専用のモデル以外をすべて止める（連携者本人が保存しても照合の材料は 0 件）。
    let patch = SettingsPatch {
        models: vec![("fast".into(), false), ("chatgpt:gpt".into(), true)],
        ..Default::default()
    };
    gw.update_tenant_settings(&ctx, &patch)
        .await
        .unwrap()
        .unwrap();
    let models = WorkflowModels::new(Some(gw), Vec::new());
    let catalog = build_catalog_from(None, None, &models, &ctx).await.unwrap();
    assert_eq!(catalog.models, Some(Vec::new()), "照合を飛ばさない");
    for model in ["fast", "chatgpt:gpt"] {
        let errs = workflow_engine::validate(&llm_ir(model), &catalog).unwrap_err();
        let e = errs
            .iter()
            .find(|e| e.code == "ir.unknown_model")
            .unwrap_or_else(|| panic!("{model}: {errs:?}"));
        assert!(
            e.message.contains("組織で使えるモデルがありません"),
            "{}",
            e.message
        );
    }
    workflow_engine::validate(&llm_ir_without_model(), &catalog).unwrap();
    std::fs::remove_dir_all(dir).unwrap();
}

/// 組織がモデルの選択を禁じていれば、既定と違うモデルを保存させない（ワークフローでも選ばせない・D-30）。
#[tokio::test]
async fn a_locked_organization_only_accepts_its_default_model() {
    let Some(pool) = pool().await else { return };
    let gw = gateway(pool);
    let ctx = user();
    let patch = SettingsPatch {
        default_model: Some("local".into()),
        allow_user_choice: Some(false),
        ..Default::default()
    };
    gw.update_tenant_settings(&ctx, &patch)
        .await
        .unwrap()
        .unwrap();
    let models = WorkflowModels::new(Some(gw), Vec::new());
    let catalog = build_catalog_from(None, None, &models, &ctx).await.unwrap();
    assert_eq!(catalog.model_lock.as_deref(), Some("local"));
    let errs = workflow_engine::validate(&llm_ir("fast"), &catalog).unwrap_err();
    let e = errs
        .iter()
        .find(|e| e.code == "ir.model_choice_disabled")
        .unwrap_or_else(|| panic!("{errs:?}"));
    assert_eq!(e.path.as_deref(), Some("/params/model"));
    workflow_engine::validate(&llm_ir("local"), &catalog).unwrap();
    workflow_engine::validate(&llm_ir_without_model(), &catalog).unwrap();
}

fn llm_ir_without_model() -> serde_json::Value {
    json!({
        "ir_version": 1, "name": "wf", "triggers": [{ "kind": "interactive" }],
        "nodes": [{ "id": "ask", "type": "llm.invoke", "params": { "prompt": "hi" } }],
        "edges": []
    })
}

/// stub の fast ＋ ChatGPT プラン（chatgpt:gpt）。連携者は `ctx` の本人。
fn gateway_with_plan(pool: PgPool, ctx: &AuthContext, dir: &std::path::Path) -> LlmGateway {
    let path = dir.join("chatgpt.json");
    std::fs::write(
        &path,
        json!({
            "version": 1,
            "client_id": "oaiapp_1",
            "owner": {"tenant_id": ctx.tenant_id, "org": ctx.org, "principal_id": ctx.principal.id},
            "tokens": {
                "access_token": "at",
                "refresh_token": "rt",
                "expires_at": "2999-01-01T00:00:00Z"
            }
        })
        .to_string(),
    )
    .unwrap();
    let account = ChatGptAccount::open(reqwest::Client::new(), ChatGptSettings::new(path));
    LlmGateway::build(
        pool,
        reqwest::Client::new(),
        GatewayConfig {
            provider: ProviderConfig {
                kind: ProviderKind::Stub,
                ..Default::default()
            },
            extra_providers: vec![ProviderConfig {
                id: CHATGPT_PROVIDER_ID.into(),
                kind: ProviderKind::ChatgptPlan,
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
                        id: "chatgpt:gpt".into(),
                        real_id: Some("gpt".into()),
                        provider: Some(CHATGPT_PROVIDER_ID.into()),
                        ..Default::default()
                    },
                ],
            },
            chatgpt: Some(std::sync::Arc::new(account)),
            ..Default::default()
        },
    )
    .expect("gateway")
}
