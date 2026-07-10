//! skill のチャット適用の結合テスト（Task 6.7/6.9 受け入れ条件）。
//!
//! stub LLM で **thread ピン → run コピー → ワーカー適用（system/few-shot/モデル既定）** の
//! 実経路を走らせる。`STORAGE_TEST_DATABASE_URL` が設定されている時のみ実行。
//! - skill を選んでチャットを開始すると system/モデル既定が適用される（llm_usage の実効モデルで観測）
//! - skill.apply が監査に残る（Task 6.12）
//!
//! ⚠️ jobq の `chat_generation` キューはプロセス内で共有されるため、**本バイナリには
//! ワーカー能力の異なるテストを同居させない**（未配線ワーカーが他テストの run を
//! 横取りすると偽陽性になる）。fail-closed は `crate::skill` の unit で担保する。

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
            models: vec![
                ModelEntry {
                    id: "m".into(),
                    real_id: None,
                    prompt_price_micros_per_mtok: 0,
                    completion_price_micros_per_mtok: 0,
                },
                ModelEntry {
                    id: "skill-model".into(),
                    real_id: None,
                    prompt_price_micros_per_mtok: 0,
                    completion_price_micros_per_mtok: 0,
                },
            ],
        },
        langfuse: None,
    };
    LlmGateway::build(pool, reqwest::Client::new(), config).expect("gateway")
}

/// skill artifact を直接作る（body は gui の保存時検証を通る形）。
async fn seed_skill(pool: &PgPool, c: &AuthContext) -> Uuid {
    let artifacts = Arc::new(artifact::ArtifactStore::new(
        pool.clone(),
        Arc::new(AllowAll),
    ));
    let skills = gui::SkillStore::new(artifacts);
    let body = json!({
        "description": "経費精算アシスタント",
        "instructions": "あなたは経費規程の専門家です。",
        "allowed_tools": ["doc_search"],
        "model": { "model": "skill-model", "temperature": 0.1, "max_tokens": 512 },
        "few_shot": [ { "user": "こんにちは", "assistant": "経費のご質問をどうぞ。" } ]
    });
    let (id, _) = skills
        .create(c, "expense-skill", &body, None)
        .await
        .expect("skill");
    id
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn skill_pin_applies_model_defaults_and_audits() {
    let Some(pool) = setup().await else { return };
    let tenant = format!("t-{}", Uuid::new_v4());
    let c = ctx(&tenant);
    let skill_id = seed_skill(&pool, &c).await;

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
            skill_artifacts: Some(artifacts),
            workflow_store: None,
            workflow_catalog: None,
        },
        WorkerConfig {
            system_prompt: "あなたはアシスタントです。".into(),
            model: Some("m".into()),
            lease_secs: 30,
            max_steps: 4,
            ..Default::default()
        },
    );
    worker.spawn(1);

    // skill をピンした thread（通常チャット＝classic 経路にも適用される）。
    let thread = store.create_thread(&c, "t", false, None).await.unwrap();
    store
        .set_thread_pins(&c, thread.id, Some((skill_id, 1)), None, None)
        .await
        .unwrap();

    let res = store
        .post_message(&c, thread.id, "出張費は？", &[], Some(false), false, None)
        .await
        .unwrap();
    let mut rx = store.event_stream(res.run_id, 0);
    let mut done = false;
    for _ in 0..500 {
        let next = tokio::time::timeout(Duration::from_secs(60), rx.next())
            .await
            .expect("イベント待ちがタイムアウト");
        let Some(ev) = next else { break };
        match ev.event {
            StreamEventKind::Done { .. } => {
                done = true;
                break;
            }
            StreamEventKind::Error { message } => panic!("生成失敗: {message}"),
            _ => {}
        }
    }
    assert!(done);

    // run 行に skill ピンがコピーされている（thread → run・0029）。
    let (run_skill, run_skill_v): (Option<Uuid>, Option<i64>) =
        sqlx::query_as("SELECT skill_id, skill_version FROM generation_run WHERE run_id = $1")
            .bind(res.run_id)
            .fetch_one(&pool)
            .await
            .unwrap();
    assert_eq!(run_skill, Some(skill_id));
    assert_eq!(run_skill_v, Some(1));

    // モデル既定が適用され、会計が実効モデル（skill-model）で刻まれる（6.9 受け入れ条件②）。
    let models: Vec<String> =
        sqlx::query_scalar("SELECT model FROM llm_usage WHERE tenant_id = $1")
            .bind(&tenant)
            .fetch_all(&pool)
            .await
            .unwrap();
    assert!(
        models.iter().any(|m| m == "skill-model"),
        "skill のモデル既定が実効モデルとして会計に載ること: {models:?}"
    );

    // skill.apply が監査に残り、run の trace 系列と突合できる（6.12）。
    let applies: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM audit_log \
         WHERE tenant_id = $1 AND action = 'skill.apply' AND object_id = $2",
    )
    .bind(&tenant)
    .bind(skill_id.to_string())
    .fetch_one(&pool)
    .await
    .unwrap();
    assert!(applies >= 1, "skill.apply 監査が残ること");
}
