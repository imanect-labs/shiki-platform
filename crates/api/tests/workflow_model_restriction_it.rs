//! ワークフローの保存時のモデルの照合が、保存する人の利用者ごとのプロバイダの制限を通すことの結合テスト
//! （D-169・D-185・実 Postgres）。
//!
//! `STORAGE_TEST_DATABASE_URL` が設定されている時のみ実行（未設定はスキップ）。
//! - 制限された人は、使えないプロバイダのモデル（llm.invoke・agent.invoke・ai.decide）を保存できない。組織で使えない
//!   モデルの `ir.unknown_model` と言い分けた `ir.model_unavailable_for_user` で、一覧から選び直すよう言う。
//! - モデルを省いたノードは、実行時に動く組織の既定（Auto の振り先）が使えなければ断る。使えるモデルに変われば通る。
//! - 制限の無い人と、ワークフローの主体（schedule・event の実行）は、今までどおり保存でき、実行時の解決も変えない。
//! - 保存 API（`build_catalog_from`）と AI 編集（emit_workflow のカタログ源）は、同じ人に同じ結果を返す。
//! - 制限の無い人が保存したワークフローは、制限された人も読める（開くだけでは壊れない）。制限された人が同じ IR で
//!   保存し直すと断られ、使えるモデルに直せば保存できる。
//! - instructions だけのスキルを呼ぶ skill.invoke も組織の既定のモデルで動くので、既定が使えなければ断る。shiki script で
//!   動くスキル（LLM を呼ばない）は断らない。

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::print_stderr
)]

use std::sync::Arc;

use api::workflow_catalog::{build_catalog_from, ApiWorkflowCatalogSource, WorkflowModels};
use async_trait::async_trait;
use authz::{
    AuthContext, AuthzClient, AuthzError, Consistency, FgaObject, ObjectType, Principal,
    PrincipalKind, ReadTupleKey, Relation, Subject,
};
use llm_gateway::config::UserProviderRule;
use llm_gateway::{
    GatewayConfig, LlmGateway, Modality, ModelCatalog, ModelEntry, ProviderConfig, ProviderKind,
    SettingsPatch,
};
use serde_json::json;
use sqlx::postgres::PgPoolOptions;
use sqlx::PgPool;
use workflow_engine::{Catalog, WorkflowStore, WorkflowStoreError, MODEL_UNAVAILABLE_FOR_USER};

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

fn user(tenant: &str, id: &str) -> AuthContext {
    AuthContext::new(
        Principal {
            kind: PrincipalKind::User,
            id: id.into(),
            email: None,
            groups: vec![],
            roles: vec![],
            tenant_id: Some(tenant.into()),
        },
        tenant.into(),
        authz::TenantId::parse(tenant).unwrap(),
    )
}

/// 既定のプロバイダ（stub）の hidden（Auto の振り先）と judge（hidden を元にした判定用）、`restricted` にも許す
/// プロバイダ allowed の visible と decide（visible を元にした判定用）。
fn gateway(pool: PgPool, tenant: &str) -> LlmGateway {
    let decision = |id: &str, provider: Option<&str>, via: &str| ModelEntry {
        id: id.into(),
        provider: provider.map(str::to_string),
        modality: Modality::Decision,
        decision_via: Some(via.into()),
        ..Default::default()
    };
    let mut config = GatewayConfig {
        provider: ProviderConfig {
            kind: ProviderKind::Stub,
            ..Default::default()
        },
        extra_providers: vec![ProviderConfig {
            id: "allowed".into(),
            kind: ProviderKind::Stub,
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
    LlmGateway::build(pool, reqwest::Client::new(), config).expect("gateway")
}

/// llm.invoke・agent.invoke・ai.decide を 1 つずつ持つ IR（`None` はモデルを省く）。
fn ir(llm: Option<&str>, agent: Option<&str>, decide: &str) -> serde_json::Value {
    let with_model = |mut params: serde_json::Value, model: Option<&str>| {
        if let Some(m) = model {
            params["model"] = json!(m);
        }
        params
    };
    json!({
        "ir_version": 1, "name": "restricted-models", "triggers": [{ "kind": "interactive" }],
        "nodes": [
            { "id": "ask", "type": "llm.invoke", "params": with_model(json!({ "prompt": "hi" }), llm) },
            { "id": "work", "type": "agent.invoke", "params": with_model(json!({ "instruction": "調べて" }), agent) },
            { "id": "judge", "type": "ai.decide", "params": {
                "model": decide, "state": { "$from": "input" },
                "questions": [{ "key": "angry", "type": "boolean", "instructions": "強い不満があるか" }] } }
        ],
        "edges": [{ "from": "ask", "to": "work" }, { "from": "work", "to": "judge" }]
    })
}

/// モデルの誤り（ノード id・コード・文）。
fn model_errors(ir: &serde_json::Value, catalog: &Catalog) -> Vec<(String, String, String)> {
    let (ir, _) = workflow_engine::complete_declared_scopes(ir);
    workflow_engine::validate(&ir, catalog)
        .err()
        .unwrap_or_default()
        .into_iter()
        .filter(|e| e.path.as_deref() == Some("/params/model"))
        .map(|e| (e.node_id.unwrap_or_default(), e.code, e.message))
        .collect()
}

async fn cleanup(pool: &PgPool, tenant: &str) {
    for table in [
        "tenant_llm_model",
        "tenant_llm_settings",
        "skill_installation",
        "registry_entry",
        "artifact_version",
        "artifact",
        "audit_log",
    ] {
        sqlx::query(&format!("DELETE FROM {table} WHERE tenant_id = $1"))
            .bind(tenant)
            .execute(pool)
            .await
            .unwrap();
    }
}

#[tokio::test]
async fn saving_follows_the_savers_provider_restriction() {
    let Some(pool) = pool().await else { return };
    let tenant = format!("wfrestrict-{}", uuid::Uuid::new_v4().simple());
    let gw = gateway(pool.clone(), &tenant);
    let models = WorkflowModels::new(Some(gw.clone()), Vec::new());
    let restricted = user(&tenant, "restricted");
    let other = user(&tenant, "other");
    // schedule・event の実行の主体（制限しない・main の決まり）。
    let workflow = AuthContext::for_workflow(
        authz::TenantId::parse(&tenant).unwrap(),
        tenant.clone(),
        "wf-1",
    );

    // 材料: 制限された人には書けるモデルだけ（llm.invoke・ai.decide）と、使えないモデル・省いたときに動く既定。
    let check = models.check(&restricted).await.unwrap();
    assert_eq!(check.ids.as_deref(), Some(&["visible".to_string()][..]));
    assert_eq!(
        check.decision_ids.as_deref(),
        Some(&["decide".to_string()][..])
    );
    assert_eq!(check.restricted, ["hidden"]);
    assert_eq!(check.default_unavailable.as_deref(), Some("hidden"));
    assert!(check.default_is_auto, "既定・Auto は実行時の設定のまま");

    let catalog = build_catalog_from(None, None, &models, &restricted)
        .await
        .unwrap();
    // 使えないモデルは、組織で使えないモデルと言い分けて断る。省いたノードも、実行時に動く既定（hidden）で断る。
    let errs = model_errors(&ir(Some("hidden"), None, "judge"), &catalog);
    assert_eq!(errs.len(), 3, "{errs:#?}");
    let at = |node: &str| {
        let (_, code, message) = errs
            .iter()
            .find(|(n, _, _)| n == node)
            .unwrap_or_else(|| panic!("{node}: {errs:#?}"));
        assert_eq!(code, MODEL_UNAVAILABLE_FOR_USER, "{node}");
        message.clone()
    };
    assert_eq!(
        at("ask"),
        "このモデルはあなたのアカウントでは使えません: hidden。一覧からモデルを選び直してください（使えるモデル: visible）"
    );
    assert_eq!(
        at("work"),
        "モデルを省くと組織の既定のモデル（hidden）で動きますが、このモデルはあなたのアカウントでは使えません。\
         一覧からモデルを選び直してください（使えるモデル: visible）"
    );
    assert!(
        at("judge").starts_with("この判定用のモデルはあなたのアカウントでは使えません: judge。")
    );
    // 組織に無いモデルは今までどおり。
    assert_eq!(
        model_errors(&ir(Some("ghost"), Some("visible"), "decide"), &catalog)[0].1,
        "ir.unknown_model"
    );
    assert!(model_errors(&ir(Some("visible"), Some("visible"), "decide"), &catalog).is_empty());

    // AI 編集（emit_workflow のカタログ源・チャットの利用者の文脈）も同じ結果。説明文に載せるモデルも同じ集合。
    let source = ApiWorkflowCatalogSource::new(
        None,
        None,
        WorkflowModels::new(Some(gw.clone()), Vec::new()),
    );
    let emitted =
        chat::WorkflowCatalogSource::catalog_for(&source, &restricted, &ir(None, None, "judge"))
            .await
            .unwrap();
    for ir in [
        ir(Some("hidden"), None, "judge"),
        ir(Some("visible"), Some("visible"), "decide"),
    ] {
        assert_eq!(model_errors(&ir, &emitted), model_errors(&ir, &catalog));
    }
    assert_eq!(
        chat::WorkflowCatalogSource::models(&source, &restricted)
            .await
            .unwrap(),
        ["visible"]
    );
    let decision: Vec<String> = chat::WorkflowCatalogSource::decision_models(&source, &restricted)
        .await
        .unwrap()
        .into_iter()
        .map(|m| m.id)
        .collect();
    assert_eq!(decision, ["decide"]);

    // 制限の無い人とワークフローの主体は今までどおり（何も外さない）。
    for ctx in [&other, &workflow] {
        let catalog = build_catalog_from(None, None, &models, ctx).await.unwrap();
        assert!(catalog.user_restricted_models.is_empty());
        assert_eq!(catalog.default_model_unavailable, None);
        assert!(model_errors(&ir(Some("hidden"), None, "judge"), &catalog).is_empty());
    }
    // 実行時の解決は変えない: ワークフローの主体は、省いたモデルを hidden で動かす。
    let resolved = gw
        .resolve_workflow_choice(&workflow, None, None)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(resolved.model, "hidden");

    // 組織の既定を使えるモデル（visible）にすると、制限された人もモデルを省ける。
    gw.update_tenant_settings(
        &other,
        &SettingsPatch {
            default_model: Some("visible".into()),
            ..Default::default()
        },
    )
    .await
    .unwrap()
    .unwrap();
    let catalog = build_catalog_from(None, None, &models, &restricted)
        .await
        .unwrap();
    assert_eq!(catalog.default_model_unavailable, None);
    assert!(model_errors(&ir(None, None, "decide"), &catalog).is_empty());

    // 選択を禁じた組織の既定が使えない: ほかを選べないので、管理者に相談するよう言う。
    gw.update_tenant_settings(
        &other,
        &SettingsPatch {
            default_model: Some("hidden".into()),
            allow_user_choice: Some(false),
            ..Default::default()
        },
    )
    .await
    .unwrap()
    .unwrap();
    let catalog = build_catalog_from(None, None, &models, &restricted)
        .await
        .unwrap();
    let errs = model_errors(&ir(None, None, "decide"), &catalog);
    assert_eq!(errs.len(), 2, "{errs:#?}");
    for (_, code, message) in &errs {
        assert_eq!(code, MODEL_UNAVAILABLE_FOR_USER);
        assert!(
            message.ends_with("この組織ではほかのモデルを選べないため、管理者に相談してください"),
            "{message}"
        );
    }
    cleanup(&pool, &tenant).await;
}

/// 使えないモデルを種類の違うノードに書いたときは、制限の無い人と同じく種類の違いを言う（判定用のモデルを文章の
/// モデルと呼ばない・その逆も）。AI 編集の説明文が省略を勧めずに model を書かせる材料（省いたときに動く既定が使えない
/// こと）も、照合と同じ材料から出る（D-185）。
#[tokio::test]
async fn a_restricted_model_of_the_other_kind_is_told_as_a_kind_mismatch() {
    let Some(pool) = pool().await else { return };
    let tenant = format!("wfrestrict-{}", uuid::Uuid::new_v4().simple());
    let gw = gateway(pool.clone(), &tenant);
    let models = WorkflowModels::new(Some(gw.clone()), Vec::new());
    let restricted = user(&tenant, "restricted");
    let other = user(&tenant, "other");
    let check = models.check(&restricted).await.unwrap();
    assert_eq!(check.restricted, ["hidden"]);
    assert_eq!(check.restricted_decision, ["judge"]);
    for ctx in [&restricted, &other] {
        let catalog = build_catalog_from(None, None, &models, ctx).await.unwrap();
        let errs = model_errors(&ir(Some("judge"), Some("visible"), "hidden"), &catalog);
        assert_eq!(errs.len(), 2, "{errs:#?}");
        assert!(
            errs.iter()
                .all(|(_, code, _)| code == "ir.model_modality_mismatch"),
            "{errs:#?}"
        );
    }
    let source =
        ApiWorkflowCatalogSource::new(None, None, WorkflowModels::new(Some(gw), Vec::new()));
    assert_eq!(
        chat::WorkflowCatalogSource::default_model_unavailable(&source, &restricted)
            .await
            .unwrap()
            .as_deref(),
        Some("hidden")
    );
    assert_eq!(
        chat::WorkflowCatalogSource::default_model_unavailable(&source, &other)
            .await
            .unwrap(),
        None
    );
    cleanup(&pool, &tenant).await;
}

/// skill の誤り（ノード id・コード・文）。
fn skill_errors(ir: &serde_json::Value, catalog: &Catalog) -> Vec<(String, String, String)> {
    workflow_engine::validate(ir, catalog)
        .err()
        .unwrap_or_default()
        .into_iter()
        .filter(|e| e.path.as_deref() == Some("/params/skill"))
        .map(|e| (e.node_id.unwrap_or_default(), e.code, e.message))
        .collect()
}

/// instructions だけのスキルは組織の既定のモデル（実行時の解決）で動くので、既定が保存する人に使えなければ、
/// skill.invoke もモデルを省いた llm.invoke と同じく断る（schedule・event の実行で制限を回り込ませない・D-185）。
/// shiki script で動くスキルは LLM を呼ばないので断らない。既定が使えるモデルに変われば通る。AI 編集のカタログ源も
/// 同じ結果。
#[tokio::test]
async fn skills_on_an_unavailable_default_model_are_refused() {
    let Some(pool) = pool().await else { return };
    let tenant = format!("wfrestrict-{}", uuid::Uuid::new_v4().simple());
    let gw = gateway(pool.clone(), &tenant);
    let models = WorkflowModels::new(Some(gw.clone()), Vec::new());
    let restricted = user(&tenant, "restricted");
    let artifacts = Arc::new(artifact::ArtifactStore::new(
        pool.clone(),
        Arc::new(AllowAll),
    ));
    let skills = gui::SkillStore::new(Arc::clone(&artifacts));
    let installs = Arc::new(app_platform::SkillInstallService::new(
        pool.clone(),
        app_platform::Registry::new(pool.clone()),
        app_platform::TrustedKeyStore::new(pool.clone()),
        Arc::clone(&artifacts),
        Arc::new(AllowAll),
    ));
    // 保存する人が作って公開し、インストールしたスキル（instructions だけのものと、shiki script を持つもの）。
    for (name, body) in [
        (
            "memo",
            json!({ "description": "メモを整える", "instructions": "箇条書きに整えてください。" }),
        ),
        (
            "tidy",
            json!({ "description": "空白を詰める", "instructions": "空白を詰める。",
                    "scripts": [{ "path": "scripts/tidy.shiki", "kind": "shiki",
                                  "source": "function main(i) { return i; }" }] }),
        ),
    ] {
        let (id, _) = skills.create(&restricted, name, &body, None).await.unwrap();
        installs
            .publish(&restricted, id, Some("1.0.0"), "in_house", None, None)
            .await
            .unwrap();
        installs
            .install(&restricted, name, Some("1.0.0"), None)
            .await
            .unwrap();
    }
    let ir = json!({
        "ir_version": 1, "name": "restricted-skills", "triggers": [{ "kind": "interactive" }],
        "nodes": [
            { "id": "memo", "type": "skill.invoke", "params": { "skill": "skill:memo@1.0.0" } },
            { "id": "tidy", "type": "skill.invoke", "params": { "skill": "skill:tidy@1.0.0" } }
        ],
        "edges": [{ "from": "memo", "to": "tidy" }]
    });
    let (ir, _) = workflow_engine::complete_declared_scopes(&ir);

    let catalog = build_catalog_from(None, Some(&installs), &models, &restricted)
        .await
        .unwrap();
    assert_eq!(
        catalog.script_skills,
        std::collections::BTreeSet::from(["tidy@1.0.0".to_string()])
    );
    let errs = skill_errors(&ir, &catalog);
    assert_eq!(errs.len(), 1, "{errs:#?}");
    let (node, code, message) = &errs[0];
    assert_eq!(
        (node.as_str(), code.as_str()),
        ("memo", MODEL_UNAVAILABLE_FOR_USER)
    );
    assert!(
        message.starts_with(
            "このスキルは組織の既定のモデル（hidden）で動きますが、このモデルはあなたのアカウントでは使えません。"
        ),
        "{message}"
    );
    // AI 編集（emit_workflow のカタログ源）も同じ結果。
    let source = ApiWorkflowCatalogSource::new(
        None,
        Some(Arc::clone(&installs)),
        WorkflowModels::new(Some(gw.clone()), Vec::new()),
    );
    let emitted = chat::WorkflowCatalogSource::catalog_for(&source, &restricted, &ir)
        .await
        .unwrap();
    assert_eq!(skill_errors(&ir, &emitted), errs);

    // 組織の既定を使えるモデル（visible）にすると、instructions だけのスキルも保存できる。
    gw.update_tenant_settings(
        &user(&tenant, "other"),
        &SettingsPatch {
            default_model: Some("visible".into()),
            ..Default::default()
        },
    )
    .await
    .unwrap()
    .unwrap();
    let catalog = build_catalog_from(None, Some(&installs), &models, &restricted)
        .await
        .unwrap();
    assert!(catalog.script_skills.is_empty(), "既定が使えれば引かない");
    assert!(skill_errors(&ir, &catalog).is_empty());
    cleanup(&pool, &tenant).await;
}

/// 全許可（保存の照合だけを見るので、ワークフローの読み書きの権限は通す）。
struct AllowAll;

#[async_trait]
impl AuthzClient for AllowAll {
    async fn check(
        &self,
        _s: &Subject,
        _r: Relation,
        _o: &FgaObject,
        _c: Consistency,
    ) -> Result<bool, AuthzError> {
        Ok(true)
    }
    async fn write_tuple(
        &self,
        _s: &Subject,
        _r: Relation,
        _o: &FgaObject,
    ) -> Result<bool, AuthzError> {
        Ok(true)
    }
    async fn delete_tuple(
        &self,
        _s: &Subject,
        _r: Relation,
        _o: &FgaObject,
    ) -> Result<bool, AuthzError> {
        Ok(true)
    }
    async fn read_tuples(
        &self,
        _o: &FgaObject,
        _r: Option<Relation>,
    ) -> Result<Vec<ReadTupleKey>, AuthzError> {
        Ok(vec![])
    }
    async fn list_objects(
        &self,
        _s: &Subject,
        _r: Relation,
        _t: ObjectType,
    ) -> Result<Vec<String>, AuthzError> {
        Ok(vec![])
    }
    async fn delete_object_tuples(&self, _o: &FgaObject) -> Result<u32, AuthzError> {
        Ok(0)
    }
    async fn read_subject_objects(
        &self,
        _s: &Subject,
        _t: ObjectType,
    ) -> Result<Vec<String>, AuthzError> {
        Ok(vec![])
    }
}

/// 制限の無い人が使えないプロバイダのモデルで保存したワークフローは、制限された人も開ける（読むときは照合しない）。
/// 同じ IR で保存し直すと断られ、使えるモデルに直すと保存できる。
#[tokio::test]
async fn a_saved_workflow_opens_but_resaving_needs_a_usable_model() {
    let Some(pool) = pool().await else { return };
    let tenant = format!("wfrestrict-{}", uuid::Uuid::new_v4().simple());
    let gw = gateway(pool.clone(), &tenant);
    let models = WorkflowModels::new(Some(gw), Vec::new());
    let restricted = user(&tenant, "restricted");
    let other = user(&tenant, "other");
    let artifacts = Arc::new(artifact::ArtifactStore::new(
        pool.clone(),
        Arc::new(AllowAll),
    ));
    let store = WorkflowStore::new(artifacts);
    let saved_ir = ir(Some("hidden"), None, "judge");

    let other_catalog = build_catalog_from(None, None, &models, &other)
        .await
        .unwrap();
    let (id, _) = store
        .create(&other, &saved_ir, &other_catalog, None)
        .await
        .expect("制限の無い人は保存できる");
    // 開くだけでは壊れない。
    let (version, opened) = store.get_latest(&restricted, id, None).await.unwrap();
    assert_eq!(version, 1);
    assert_eq!(opened.nodes.len(), 3);
    // 保存し直すと断られる。
    let catalog = build_catalog_from(None, None, &models, &restricted)
        .await
        .unwrap();
    let refused = store
        .update(&restricted, id, &saved_ir, &catalog, Some(1), None)
        .await;
    let Err(WorkflowStoreError::Validation(errors)) = refused else {
        panic!("断る: {refused:?}");
    };
    assert!(
        errors.iter().all(|e| e.code == MODEL_UNAVAILABLE_FOR_USER),
        "{errors:#?}"
    );
    assert_eq!(errors.len(), 3, "{errors:#?}");
    // 使えるモデルに直せば保存できる。
    let (version, _) = store
        .update(
            &restricted,
            id,
            &ir(Some("visible"), Some("visible"), "decide"),
            &catalog,
            Some(1),
            None,
        )
        .await
        .expect("直した IR は保存できる");
    assert_eq!(version, 2);
    cleanup(&pool, &tenant).await;
}
