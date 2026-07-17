//! emit_workflow / workflow_ref content block の結合テスト（Task 10.13 受け入れ条件）。
//!
//! stub LLM の `emitwf:` プレフィックスで **ChatWorker → emit_workflow（実 V1〜V7 検証）→
//! WorkflowStore（artifact 保存）→ sink → SSE/projection** の実経路を走らせる。
//! `STORAGE_TEST_DATABASE_URL` が設定されている時のみ実行。
//! - `emitwf:ok <name>`: 検証を通った IR が artifact 保存され、workflow_ref が SSE 配信・永続化される
//! - `emitwf:bad <name>`: 検証拒否 → 保存されず・ブロック化されず、is_error の tool_result が返る

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
use chat::{ChatStore, ChatWorker, ContentBlock, StreamEventKind, WorkerConfig};
use futures::stream::StreamExt;
use llm_gateway::{
    GatewayConfig, LlmGateway, ModelCatalog, ModelEntry, ProviderConfig, ProviderKind,
};
use sqlx::{postgres::PgPoolOptions, PgPool};
use uuid::Uuid;
use workflow_engine::{Catalog, WorkflowStore};

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

/// 空カタログ（secrets なし・モデルは stub の "m" のみ）。保存 API と同じ形の材料。
struct TestCatalogSource;

#[async_trait]
impl chat::WorkflowCatalogSource for TestCatalogSource {
    async fn catalog(&self, _ctx: &AuthContext) -> Result<Catalog, String> {
        Ok(Catalog {
            models: vec!["m".into()],
            ..Catalog::default()
        })
    }
}

/// ワーカー＋WorkflowStore 一式を組む。
async fn spawn_worker(pool: &PgPool) -> (ChatStore, Arc<WorkflowStore>) {
    let store = ChatStore::connect(pool.clone(), Arc::new(AllowAll), None)
        .await
        .unwrap();
    let artifacts = Arc::new(artifact::ArtifactStore::new(
        pool.clone(),
        Arc::new(AllowAll),
    ));
    let workflows = Arc::new(WorkflowStore::new(artifacts));
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
            skill_artifacts: None,
            workflow_store: Some(Arc::clone(&workflows)),
            workflow_catalog: Some(Arc::new(TestCatalogSource)),
            collab: None,
            tabular: None,
            office: None,
        },
        WorkerConfig {
            system_prompt: "あなたはアシスタントです。".into(),
            model: Some("m".into()),
            // Coverage（cargo-llvm-cov）は計装＋全テスト並列で 1 step が大きく遅くなる。
            // emit_workflow は WorkflowStore.create（V1〜V7 検証＋artifact 書込）を伴い重いため、
            // 30s ではリース失効で run が orphan 化して flake る。十分な余裕を持たせる。
            lease_secs: 120,
            max_steps: 4,
            ..Default::default()
        },
    );
    worker.spawn(1);
    (store, workflows)
}

/// 発話 → done まで drain し、観測イベント種を返す。
async fn run_to_done(
    store: &ChatStore,
    c: &AuthContext,
    thread_id: Uuid,
    text: &str,
) -> (Uuid, Vec<StreamEventKind>) {
    let res = store
        .post_message(c, thread_id, text, &[], None, Some(true), false, None)
        .await
        .unwrap();
    let mut rx = store.event_stream(res.run_id, 0);
    let mut events = Vec::new();
    for _ in 0..500 {
        // Coverage（cargo-llvm-cov）実行時は計装＋並列テストバイナリで大きく遅くなるため
        // 余裕を持つ（60s では稀に claim 前にタイムアウトする・genui_it と同型の flake 対策）。
        let next = tokio::time::timeout(Duration::from_secs(180), rx.next())
            .await
            .expect("イベント待ちがタイムアウト");
        let Some(ev) = next else { break };
        let is_done = matches!(ev.event, StreamEventKind::Done { .. });
        if let StreamEventKind::Error { message } = &ev.event {
            panic!("生成失敗: {message}");
        }
        events.push(ev.event);
        if is_done {
            break;
        }
    }
    (res.assistant_message_id, events)
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn valid_ir_is_saved_and_workflow_ref_streamed_and_persisted() {
    let Some(pool) = setup().await else { return };
    let tenant = format!("t-{}", Uuid::new_v4());
    let (store, workflows) = spawn_worker(&pool).await;
    let c = ctx(&tenant);
    let thread = store
        .create_thread(&c, "t", true, None, None)
        .await
        .unwrap();

    let name = format!("emitwf-{}", Uuid::new_v4().simple());
    let (asst_id, events) = run_to_done(&store, &c, thread.id, &format!("emitwf:ok {name}")).await;

    // SSE に workflow_ref イベントが流れる。
    let sse_ref = events
        .iter()
        .find_map(|e| match e {
            StreamEventKind::WorkflowRef { workflow } => Some(workflow.clone()),
            _ => None,
        })
        .expect("SSE に workflow_ref イベントが出ること");
    assert_eq!(sse_ref["name"], name.as_str());
    assert_eq!(sse_ref["version"], 1);

    // 永続化された message.content にも参照ブロックが残る。
    let msgs = store.get_messages(&c, thread.id, None).await.unwrap();
    let asst = msgs.iter().find(|m| m.id == asst_id).unwrap();
    let block_ref = asst
        .content
        .iter()
        .find_map(|b| match b {
            ContentBlock::WorkflowRef { workflow } => Some(workflow.clone()),
            _ => None,
        })
        .expect("workflow_ref ブロックが確定保存されること");

    // 参照先の artifact が実在し、保存された IR が読める（保存パイプライン通過の裏取り）。
    let id = Uuid::parse_str(block_ref["id"].as_str().unwrap()).unwrap();
    let (version, ir) = workflows.get_latest(&c, id, None).await.unwrap();
    assert_eq!(version, 1);
    assert_eq!(ir.name, name);
    assert_eq!(ir.nodes.len(), 1);

    // ツール結果は成功（is_error=false）。
    assert!(events
        .iter()
        .any(|e| matches!(e, StreamEventKind::ToolResult { ok: true, .. })));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn invalid_ir_is_rejected_with_all_errors_and_not_saved() {
    let Some(pool) = setup().await else { return };
    let tenant = format!("t-{}", Uuid::new_v4());
    let (store, _workflows) = spawn_worker(&pool).await;
    let c = ctx(&tenant);
    let thread = store
        .create_thread(&c, "t", true, None, None)
        .await
        .unwrap();

    let name = format!("emitwf-bad-{}", Uuid::new_v4().simple());
    let (asst_id, events) = run_to_done(&store, &c, thread.id, &format!("emitwf:bad {name}")).await;

    // workflow_ref イベントは一切流れない（未検証 IR がブロック化される経路は無い）。
    assert!(
        !events
            .iter()
            .any(|e| matches!(e, StreamEventKind::WorkflowRef { .. })),
        "不正 IR がブロック化されないこと"
    );

    // is_error の tool_result に検証エラー（コード付き）がモデルへ返る（自己修正の観測）。
    let err_content = events
        .iter()
        .find_map(|e| match e {
            StreamEventKind::ToolResult {
                ok: false, content, ..
            } => Some(content.clone()),
            _ => None,
        })
        .expect("is_error の tool_result が返ること");
    assert!(
        err_content.contains("ir."),
        "検証エラーコードが観測に含まれること: {err_content}"
    );

    // artifact は保存されない。
    let saved: i64 =
        sqlx::query_scalar("SELECT count(*) FROM artifact WHERE tenant_id = $1 AND name = $2")
            .bind(&tenant)
            .bind(&name)
            .fetch_one(&pool)
            .await
            .unwrap();
    assert_eq!(saved, 0, "検証拒否された IR が保存されないこと");

    // テキストのフォールバック応答は残る。
    let msgs = store.get_messages(&c, thread.id, None).await.unwrap();
    let asst = msgs.iter().find(|m| m.id == asst_id).unwrap();
    assert!(
        asst.content
            .iter()
            .any(|b| matches!(b, ContentBlock::Text { .. })),
        "テキストのフォールバック応答が残ること"
    );
}
