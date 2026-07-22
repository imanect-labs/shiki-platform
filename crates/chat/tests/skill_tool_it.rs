//! skill ツール（カタログ引き）の結合テスト（#344 Task 10.11 受け入れ条件）。
//!
//! stub LLM の `useskill:<name>` 駆動で **カタログ掲載 → skill ツール呼び出し →
//! instructions の観測 → 発動イベント（skill_invoked）の generation_event 記録 →
//! skill.invoke 監査** の実経路を走らせる。`STORAGE_TEST_DATABASE_URL` 設定時のみ実行。
//!
//! ⚠️ jobq の `chat_generation` キューはプロセス内で共有されるため、**本バイナリには
//! ワーカー能力の異なるテストを同居させない**（skill_apply_it と同じ規約）。

#![allow(
    clippy::pedantic,
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::print_stderr
)]

use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use authz::{
    AuthContext, AuthzClient, AuthzError, Consistency, FgaObject, ObjectType, Principal,
    ReadTupleKey, Relation, Subject,
};
use chat::{ChatStore, ChatWorker, StreamEventKind, WorkerConfig};
use futures::stream::StreamExt;
use llm_gateway::{
    GatewayConfig, LlmGateway, ModelCatalog, ModelEntry, ProviderConfig, ProviderKind,
};
use serde_json::json;
use sqlx::{postgres::PgPoolOptions, PgPool};
use uuid::Uuid;

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

async fn setup() -> Option<PgPool> {
    let Ok(db_url) = std::env::var("STORAGE_TEST_DATABASE_URL") else {
        eprintln!("STORAGE_TEST_DATABASE_URL 未設定のためスキップ");
        return None;
    };
    let pool = PgPoolOptions::new()
        .max_connections(5)
        .connect(&db_url)
        .await
        .expect("Postgres へ接続できること");
    sqlx::migrate!("../../migrations")
        .run(&pool)
        .await
        .expect("マイグレーション適用");
    Some(pool)
}

fn ctx(tenant: &str) -> AuthContext {
    AuthContext::new(
        Principal {
            kind: authz::PrincipalKind::User,
            id: "alice".into(),
            email: None,
            groups: vec![],
            roles: vec![],
            tenant_id: Some(tenant.into()),
        },
        "acme".into(),
        tenant.into(),
    )
}

fn stub_gateway(pool: PgPool) -> LlmGateway {
    let config = GatewayConfig {
        provider: ProviderConfig {
            kind: ProviderKind::Stub,
            base_url: None,
            api_key: None,
            timeout_secs: 120,
        },
        catalog: ModelCatalog {
            default_model: "m".into(),
            models: vec![ModelEntry {
                id: "m".into(),
                real_id: None,
                prompt_price_micros_per_mtok: 0,
                completion_price_micros_per_mtok: 0,
            }],
        },
        langfuse: None,
    };
    LlmGateway::build(pool, reqwest::Client::new(), config).expect("gateway")
}

/// skill artifact を直接作る（body は gui の保存時検証を通る形）。
async fn seed_skill(pool: &PgPool, c: &AuthContext, name: &str) -> Uuid {
    let artifacts = Arc::new(artifact::ArtifactStore::new(
        pool.clone(),
        Arc::new(AllowAll),
    ));
    let skills = gui::SkillStore::new(artifacts);
    let body = json!({
        "description": "経費精算の規程に基づいて確認・回答するスキル",
        "instructions": "あなたは経費規程の専門家です。",
        "allowed_tools": ["doc_search"]
    });
    let (id, _) = skills.create(c, name, &body, None).await.expect("skill");
    id
}

/// カタログ引き（未ピン・本人 owner の skill をツールで途中読み込み）の一連の流れ。
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn skill_tool_loads_instructions_and_records_invocation() {
    let Some(pool) = setup().await else { return };
    let tenant = format!("t-{}", Uuid::new_v4());
    let c = ctx(&tenant);
    let skill_id = seed_skill(&pool, &c, "expense-skill").await;

    let store = ChatStore::connect(pool.clone(), Arc::new(AllowAll), None)
        .await
        .unwrap();
    let artifacts = Arc::new(artifact::ArtifactStore::new(
        pool.clone(),
        Arc::new(AllowAll),
    ));
    let worker = ChatWorker::new(
        pool.clone(),
        store.clone(),
        chat::WorkerDeps {
            gateway: stub_gateway(pool.clone()),
            search: None,
            sandbox: None,
            artifacts: None,
            web_search: None,
            storage: None,
            ui_validator: None,
            skill_artifacts: Some(artifacts.clone()),
            skill_catalog: Some(Arc::new(chat::OwnedSkillCatalog::new(artifacts))),
            workflow_store: None,
            workflow_catalog: None,
            collab: None,
            tabular: None,
            office: None,
            authz: None,
        },
        WorkerConfig {
            system_prompt: "あなたはアシスタントです。".into(),
            model: Some("m".into()),
            lease_secs: 120,
            max_steps: 4,
            ..Default::default()
        },
    );
    worker.spawn(1);

    // ピン無しの thread（skill はカタログ＝本人 owner から引く）。
    let thread = store
        .create_thread(&c, "t", true, None, None)
        .await
        .unwrap();
    let res = store
        .post_message(
            &c,
            thread.id,
            "useskill:expense-skill",
            &[],
            None,
            Some(true),
            false,
            None,
        )
        .await
        .unwrap();

    let mut rx = store.event_stream(res.run_id, 0);
    let mut done = false;
    let mut invoked: Option<serde_json::Value> = None;
    let mut tool_result_content = String::new();
    for _ in 0..500 {
        let next = tokio::time::timeout(Duration::from_secs(180), rx.next())
            .await
            .expect("イベント待ちがタイムアウト");
        let Some(ev) = next else { break };
        match ev.event {
            StreamEventKind::SkillInvoked { skill } => invoked = Some(skill),
            StreamEventKind::ToolResult { content, ok, .. } => {
                assert!(ok, "skill ツールが成功すること: {content}");
                tool_result_content = content;
            }
            StreamEventKind::Done { .. } => {
                done = true;
                break;
            }
            StreamEventKind::Error { message } => panic!("生成失敗: {message}"),
            _ => {}
        }
    }
    assert!(done);

    // instructions がツール結果としてモデルに観測される（本文はカタログに載らず必要時に引く）。
    assert!(
        tool_result_content.contains("経費規程の専門家"),
        "instructions が観測に載ること: {tool_result_content}"
    );
    // 発動記録（skill_invoked イベント）が (skill_id, version) 付きで流れる。
    let invoked = invoked.expect("skill_invoked イベントが流れること");
    assert_eq!(
        invoked.get("skill_id").and_then(|v| v.as_str()),
        Some(skill_id.to_string().as_str())
    );
    assert_eq!(
        invoked.get("skill_version").and_then(|v| v.as_i64()),
        Some(1)
    );

    // 真実のソース（generation_event）にも append されている（replay 可能・再現性）。
    let recorded: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM generation_event WHERE run_id = $1 AND type = 'skill_invoked'",
    )
    .bind(res.run_id)
    .fetch_one(&pool)
    .await
    .unwrap();
    assert!(
        recorded >= 1,
        "generation_event に skill_invoked が残ること"
    );

    // skill.invoke 監査が残る。
    let invokes: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM audit_log \
         WHERE tenant_id = $1 AND action = 'skill.invoke' AND object_id = $2",
    )
    .bind(&tenant)
    .bind(skill_id.to_string())
    .fetch_one(&pool)
    .await
    .unwrap();
    assert!(invokes >= 1, "skill.invoke 監査が残ること");
}

/// 未知スキル名は閉集合照合で弾かれ、エラー観測（is_error）になる（fail-closed・実行はされない）。
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn unknown_skill_name_is_observed_error() {
    let Some(pool) = setup().await else { return };
    let tenant = format!("t-{}", Uuid::new_v4());
    let c = ctx(&tenant);
    // カタログに 1 件だけ入れておく（候補提示の検証）。
    seed_skill(&pool, &c, "expense-skill").await;

    let store = ChatStore::connect(pool.clone(), Arc::new(AllowAll), None)
        .await
        .unwrap();
    let artifacts = Arc::new(artifact::ArtifactStore::new(
        pool.clone(),
        Arc::new(AllowAll),
    ));
    let worker = ChatWorker::new(
        pool.clone(),
        store.clone(),
        chat::WorkerDeps {
            gateway: stub_gateway(pool.clone()),
            search: None,
            sandbox: None,
            artifacts: None,
            web_search: None,
            storage: None,
            ui_validator: None,
            skill_artifacts: Some(artifacts.clone()),
            skill_catalog: Some(Arc::new(chat::OwnedSkillCatalog::new(artifacts))),
            workflow_store: None,
            workflow_catalog: None,
            collab: None,
            tabular: None,
            office: None,
            authz: None,
        },
        WorkerConfig {
            system_prompt: "あなたはアシスタントです。".into(),
            model: Some("m".into()),
            lease_secs: 120,
            max_steps: 4,
            ..Default::default()
        },
    );
    worker.spawn(1);

    let thread = store
        .create_thread(&c, "t", true, None, None)
        .await
        .unwrap();
    let res = store
        .post_message(
            &c,
            thread.id,
            "useskill:no-such-skill",
            &[],
            None,
            Some(true),
            false,
            None,
        )
        .await
        .unwrap();

    let mut rx = store.event_stream(res.run_id, 0);
    let mut done = false;
    let mut saw_error_result = false;
    let mut saw_invoked = false;
    for _ in 0..500 {
        let next = tokio::time::timeout(Duration::from_secs(180), rx.next())
            .await
            .expect("イベント待ちがタイムアウト");
        let Some(ev) = next else { break };
        match ev.event {
            StreamEventKind::SkillInvoked { .. } => saw_invoked = true,
            StreamEventKind::ToolResult { content, ok, .. } => {
                assert!(!ok, "未知スキルはエラー観測になること");
                assert!(
                    content.contains("expense-skill"),
                    "候補が提示されること: {content}"
                );
                saw_error_result = true;
            }
            StreamEventKind::Done { .. } => {
                done = true;
                break;
            }
            StreamEventKind::Error { message } => panic!("生成失敗: {message}"),
            _ => {}
        }
    }
    assert!(done);
    assert!(saw_error_result, "エラー観測がイベントとして流れること");
    assert!(!saw_invoked, "未知スキルで発動記録が残らないこと");
}
