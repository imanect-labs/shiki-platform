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
    ChatGptAccount, ChatGptSettings, GatewayConfig, LlmGateway, Modality, ModelCatalog, ModelEntry,
    ProviderConfig, ProviderKind, SettingsPatch, CHATGPT_PROVIDER_ID,
};
use serde_json::json;
use sqlx::postgres::PgPoolOptions;
use sqlx::PgPool;

async fn pool() -> Option<PgPool> {
    let Ok(url) = shiki_testenv::var("STORAGE_TEST_DATABASE_URL") else {
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
        authz::TenantId::parse(tenant).unwrap(),
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
    let models = WorkflowModels::new(
        Some(gw.clone()),
        vec![("from-config".into(), llm_gateway::Modality::Chat)],
    );

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
    let models = WorkflowModels::new(
        None,
        vec![
            ("m1".into(), Modality::Chat),
            ("judge".into(), Modality::Decision),
            ("m2".into(), Modality::Chat),
        ],
    );
    let catalog = build_catalog_from(None, None, &models, &ctx).await.unwrap();
    // 種類で振り分ける（llm.invoke は chat だけ・ai.decide は判定用だけ・FEAT-1）。
    assert_eq!(
        catalog.models.as_deref(),
        Some(&["m1", "m2"].map(String::from)[..])
    );
    assert_eq!(
        catalog.decision_models.as_deref(),
        Some(&["judge".to_string()][..])
    );
    workflow_engine::validate(&decide_ir("judge"), &catalog).unwrap();
    let errs = workflow_engine::validate(&llm_ir("judge"), &catalog).unwrap_err();
    assert!(
        errs.iter().any(|e| e.code == "ir.model_modality_mismatch"),
        "{errs:?}"
    );
    let none = WorkflowModels::new(None, Vec::new());
    let catalog = build_catalog_from(None, None, &none, &ctx).await.unwrap();
    assert_eq!(catalog.models, None, "空なら照合しない（従来どおり）");
    assert_eq!(catalog.decision_models, None);
    workflow_engine::validate(&decide_ir("anything"), &catalog).unwrap();
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

/// ノードに書いた思考の深さが使われない組織の設定（省略して Auto・選択の禁止）では、保存時に警告する。材料は保存 API と
/// emit_workflow が共有するカタログ（実行時の解決と同じ設定）から読む（D-75）。
#[tokio::test]
async fn effort_warnings_follow_the_organization_settings() {
    let Some(pool) = pool().await else { return };
    let gw = gateway(pool.clone());
    let ctx = user();
    let models = WorkflowModels::new(Some(gw.clone()), Vec::new());
    let with_effort = |model: Option<&str>| {
        let mut ir = llm_ir_without_model();
        ir["nodes"][0]["params"]["effort"] = json!("low");
        if let Some(m) = model {
            ir["nodes"][0]["params"]["model"] = json!(m);
        }
        ir
    };
    let warned = |ir: &serde_json::Value, catalog: &workflow_engine::Catalog| -> bool {
        let parsed = workflow_engine::validate(ir, catalog).unwrap();
        workflow_engine::save_warnings(&parsed, catalog)
            .iter()
            .any(|w| w.code == workflow_engine::EFFORT_IGNORED)
    };

    // 未保存の組織は Auto: 省略は警告し、モデルを選べば使われるので警告しない。
    let catalog = build_catalog_from(None, None, &models, &ctx).await.unwrap();
    assert!(catalog.default_is_auto);
    assert!(warned(&with_effort(None), &catalog));
    assert!(!warned(&with_effort(Some("fast")), &catalog));

    // 既定が特定のモデル（選べる）: 省略でも使われる。
    let patch = SettingsPatch {
        default_model: Some("local".into()),
        ..Default::default()
    };
    gw.update_tenant_settings(&ctx, &patch)
        .await
        .unwrap()
        .unwrap();
    let catalog = build_catalog_from(None, None, &models, &ctx).await.unwrap();
    assert!(!catalog.default_is_auto);
    assert!(!warned(&with_effort(None), &catalog));

    // 選択の禁止: 省略でも既定と同じモデルの明示でも使われない。
    let patch = SettingsPatch {
        allow_user_choice: Some(false),
        ..Default::default()
    };
    gw.update_tenant_settings(&ctx, &patch)
        .await
        .unwrap()
        .unwrap();
    let catalog = build_catalog_from(None, None, &models, &ctx).await.unwrap();
    assert!(warned(&with_effort(None), &catalog));
    assert!(warned(&with_effort(Some("local")), &catalog));

    for sql in [
        "DELETE FROM tenant_llm_model WHERE tenant_id = $1",
        "DELETE FROM tenant_llm_settings WHERE tenant_id = $1",
    ] {
        sqlx::query(sql)
            .bind(ctx.tenant_id())
            .execute(&pool)
            .await
            .unwrap();
    }
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
            "owner": {"tenant_id": ctx.tenant_id(), "org": ctx.org(), "principal_id": ctx.principal().id},
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

/// 判定のノード 1 つの IR（ai.decide の model の照合・FEAT-1）。
fn decide_ir(model: &str) -> serde_json::Value {
    json!({
        "ir_version": 1, "name": "wf", "triggers": [{ "kind": "interactive" }],
        "nodes": [{ "id": "judge", "type": "ai.decide", "params": {
            "model": model, "state": { "$from": "input" },
            "questions": [{ "key": "angry", "type": "boolean", "instructions": "強い不満があるか" }] } }],
        "edges": []
    })
}

/// stub の chat モデル fast・local と、local を元にする判定用のモデル judge（provider は local と同じ既定）。
fn gateway_with_decision(pool: PgPool) -> LlmGateway {
    let chat = |id: &str| ModelEntry {
        id: id.into(),
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
                    chat("fast"),
                    chat("local"),
                    ModelEntry {
                        id: "judge".into(),
                        display_name: Some("local で判定".into()),
                        modality: Modality::Decision,
                        decision_via: Some("local".into()),
                        ..Default::default()
                    },
                ],
            },
            ..Default::default()
        },
    )
    .expect("gateway")
}

/// 判定用のモデルは組織の設定で照合する。元にする chat モデルを止めると、判定用のモデルも書けなくなる
/// （判定のチョークポイントは両方の許可を求めるので、保存でも同じ集合で弾く・fail-closed）。
#[tokio::test]
async fn decision_models_follow_the_organization_settings() {
    let Some(pool) = pool().await else { return };
    let gw = gateway_with_decision(pool);
    let ctx = user();
    let models = WorkflowModels::new(Some(gw.clone()), Vec::new());
    let catalog = build_catalog_from(None, None, &models, &ctx).await.unwrap();
    assert_eq!(
        catalog.models.as_deref(),
        Some(&["fast", "local"].map(String::from)[..])
    );
    assert_eq!(
        catalog.decision_models.as_deref(),
        Some(&["judge".to_string()][..])
    );
    workflow_engine::validate(&decide_ir("judge"), &catalog).unwrap();
    // 文章を生成するモデルを判定に書くと種類の違い、判定用のモデルを llm.invoke に書いても種類の違い。
    let errs = workflow_engine::validate(&decide_ir("fast"), &catalog).unwrap_err();
    assert_eq!(errs[0].code, "ir.model_modality_mismatch", "{errs:?}");
    assert_eq!(errs[0].path.as_deref(), Some("/params/model"));
    let errs = workflow_engine::validate(&llm_ir("judge"), &catalog).unwrap_err();
    assert_eq!(errs[0].code, "ir.model_modality_mismatch", "{errs:?}");

    // 管理者が元の chat モデル local を止める（判定用のモデル judge 自体は許可のまま）。
    let patch = SettingsPatch {
        models: vec![("local".into(), false)],
        ..Default::default()
    };
    gw.update_tenant_settings(&ctx, &patch)
        .await
        .unwrap()
        .unwrap();
    let catalog = build_catalog_from(None, None, &models, &ctx).await.unwrap();
    assert_eq!(catalog.decision_models, Some(Vec::new()));
    let errs = workflow_engine::validate(&decide_ir("judge"), &catalog).unwrap_err();
    assert_eq!(errs[0].code, "ir.unknown_model", "{errs:?}");
    assert!(
        errs[0]
            .message
            .contains("組織で使える判定用のモデルがありません"),
        "{}",
        errs[0].message
    );
}

/// 保存 API（`build_catalog_from`）と AI 編集（emit_workflow のカタログ源）は、判定用のモデルを同じ集合で
/// 照合し、同じ IR に同じ結果を返す（「エディタでは通るが AI 経由では落ちる」を作らない・R6）。
#[tokio::test]
async fn the_save_api_and_emit_workflow_agree_on_decision_models() {
    let Some(pool) = pool().await else { return };
    let gw = gateway_with_decision(pool);
    let ctx = user();
    let source = api::workflow_catalog::ApiWorkflowCatalogSource::new(
        None,
        None,
        WorkflowModels::new(Some(gw.clone()), Vec::new()),
    );
    for step in 0..2 {
        if step == 1 {
            // 判定用のモデルを止めた後でも、両者は同じ結果になる。
            let patch = SettingsPatch {
                models: vec![("judge".into(), false)],
                ..Default::default()
            };
            gw.update_tenant_settings(&ctx, &patch)
                .await
                .unwrap()
                .unwrap();
        }
        let saved = build_catalog_from(
            None,
            None,
            &WorkflowModels::new(Some(gw.clone()), Vec::new()),
            &ctx,
        )
        .await
        .unwrap();
        let emitted = chat::WorkflowCatalogSource::catalog_for(&source, &ctx, &decide_ir("judge"))
            .await
            .unwrap();
        assert_eq!(saved.models, emitted.models, "step {step}");
        assert_eq!(
            saved.decision_models, emitted.decision_models,
            "step {step}"
        );
        // emit_workflow の説明文に載せる判定用のモデル（id と表示名）も同じ集合（D-79）。
        let listed = chat::WorkflowCatalogSource::decision_models(&source, &ctx)
            .await
            .unwrap();
        assert_eq!(
            listed.iter().map(|m| m.id.clone()).collect::<Vec<_>>(),
            saved.decision_models.clone().unwrap_or_default(),
            "step {step}"
        );
        if step == 0 {
            assert_eq!(
                listed,
                [chat::DecisionModel {
                    id: "judge".into(),
                    label: Some("local で判定".into()),
                }]
            );
        }
        for ir in [decide_ir("judge"), decide_ir("fast"), llm_ir("judge")] {
            let codes = |c: &workflow_engine::Catalog| -> Vec<String> {
                workflow_engine::validate(&ir, c)
                    .err()
                    .unwrap_or_default()
                    .into_iter()
                    .map(|e| e.code)
                    .collect()
            };
            assert_eq!(codes(&saved), codes(&emitted), "step {step}: {ir}");
        }
    }
    let after = build_catalog_from(None, None, &WorkflowModels::new(Some(gw), Vec::new()), &ctx)
        .await
        .unwrap();
    assert_eq!(after.decision_models, Some(Vec::new()));
}
