//! 回答の本文の ```ui（Yui）のボタンの結合テスト（#604）。
//!
//! stub LLM の `yui:app` / `yui:ask` で **ChatWorker → 確定時の束縛の解決 → ui-actions の経路
//! （編集者・本文からの読み直しと値の検証・input_schema・冪等キー・起動の記録）** を実経路で走らせる。
//! `STORAGE_TEST_DATABASE_URL` が設定されている時のみ実行。

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
use chat::{
    ChatStore, ChatWorker, ContentBlock, Message, Role, StreamEventKind, UiActionRunStatus,
    WorkerConfig, YuiActionRequest,
};
use futures::stream::StreamExt;
use llm_gateway::{
    GatewayConfig, LlmGateway, ModelCatalog, ModelEntry, ProviderConfig, ProviderKind,
};
use serde_json::{json, Value};
use sqlx::{postgres::PgPoolOptions, PgPool};
use uuid::Uuid;
use workflow_engine::{Catalog, DelegationStore, RunStore, WorkflowRunLauncher, WorkflowStore};

/// 認可: bob はどのオブジェクトの editor でもない（閲覧者）。それ以外は全て許す。
struct Policy;

#[async_trait]
impl AuthzClient for Policy {
    async fn check(
        &self,
        s: &Subject,
        r: Relation,
        _o: &FgaObject,
        _c: Consistency,
    ) -> Result<bool, AuthzError> {
        Ok(!(r == Relation::Editor && format!("{s:?}").contains("bob")))
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

fn ctx(tenant: &str, uid: &str) -> AuthContext {
    AuthContext::new(
        Principal {
            kind: authz::PrincipalKind::User,
            id: uid.into(),
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
            user_agent: None,
            session_header: None,
            ..Default::default()
        },
        catalog: ModelCatalog {
            default_model: "m".into(),
            models: vec![ModelEntry {
                id: "m".into(),
                real_id: None,
                prompt_price_micros_per_mtok: 0,
                completion_price_micros_per_mtok: 0,
                context_window_tokens: None,
                ..Default::default()
            }],
        },
        langfuse: None,
        ..Default::default()
    };
    LlmGateway::build(pool, reqwest::Client::new(), config).expect("gateway")
}

/// api の LauncherWorkflowStarter と同じ結線（private のため同型を組む）。
struct TestStarter(WorkflowRunLauncher);

#[async_trait]
impl gui::WorkflowStarter for TestStarter {
    async fn start_pinned(
        &self,
        ctx: &AuthContext,
        workflow_id: Uuid,
        version: i64,
        input: &Value,
    ) -> Result<Option<Uuid>, gui::ActionError> {
        self.0
            .start_interactive_version(ctx, workflow_id, version, input)
            .await
            .map_err(|e| match e {
                workflow_engine::run::LauncherError::Ir(_) => gui::ActionError::NotFound,
                workflow_engine::run::LauncherError::Input(m) => gui::ActionError::Invalid(m),
                other => gui::ActionError::Internal(format!("run 起動: {other}")),
            })
    }

    async fn start_pinned_via_bundle(
        &self,
        _ctx: &AuthContext,
        _bundle_id: Uuid,
        _workflow_id: Uuid,
        _version: i64,
        _input: &Value,
    ) -> Result<Option<Uuid>, gui::ActionError> {
        Err(gui::ActionError::Forbidden)
    }
}

/// ワーカーを同時に走らせない（生成の待ち行列は DB 単位で 1 本・genui_it と同じ理由）。
static WORKER_LOCK: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

struct Env {
    pool: PgPool,
    store: ChatStore,
    workflows: WorkflowStore,
    dispatcher: gui::ActionDispatcher,
    _guard: tokio::sync::MutexGuard<'static, ()>,
}

async fn env(pool: PgPool) -> Env {
    let guard = WORKER_LOCK.lock().await;
    let authz: Arc<dyn AuthzClient> = Arc::new(Policy);
    let store = ChatStore::connect(pool.clone(), Arc::clone(&authz), None)
        .await
        .unwrap();
    let artifacts = Arc::new(artifact::ArtifactStore::new(
        pool.clone(),
        Arc::clone(&authz),
    ));
    let validator = Arc::new(gui::SpecValidator::new(
        Arc::clone(&artifacts),
        pool.clone(),
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
            parser: None,
            storage: None,
            ui_validator: Some(validator),
            skill_artifacts: None,
            skill_catalog: None,
            workflow_store: None,
            workflow_catalog: None,
            collab: None,
            tabular: None,
            office: None,
            office_live: None,
            office_creator: None,
            office_engine: None,
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
    let workflows = WorkflowStore::new(Arc::clone(&artifacts));
    let runs = RunStore::new(pool.clone(), workflow_engine::DEFAULT_LEASE_SECS);
    let launcher = WorkflowRunLauncher::new(
        DelegationStore::new(pool.clone(), Arc::clone(&authz)),
        workflows.clone(),
        runs,
    );
    let mut dispatcher =
        gui::ActionDispatcher::new(storage::audit::AuditRecorder::new(pool.clone()));
    dispatcher.register_handler(Arc::new(chat::ChatSubmitHandler::new(store.clone())));
    dispatcher.set_ledger(Arc::new(chat::ChatActionLedger::new(store.clone())));
    dispatcher.set_workflow_starter(Arc::new(TestStarter(launcher)));
    Env {
        pool,
        store,
        workflows,
        dispatcher,
        _guard: guard,
    }
}

/// 入力の契約つきのワークフロー（実行はしない・run が作られるかだけを見る）。
fn expense_ir(name: &str) -> Value {
    json!({
        "ir_version": 1,
        "name": name,
        "declared_scopes": [],
        "nodes": [{
            "id": "compute",
            "type": "script.run",
            "params": {
                "source": { "inline": "function main(i){ return { ok: true }; }" },
                "input": { "$from": "input" }
            }
        }],
        "edges": [],
        "triggers": [{ "kind": "interactive" }],
        "input_schema": {
            "type": "object",
            "required": ["amount", "category"],
            "additionalProperties": false,
            "properties": {
                "amount": { "type": "integer", "minimum": 1, "maximum": 100000 },
                "category": { "type": "string", "enum": ["交通費", "交際費"] }
            }
        }
    })
}

/// 発話 → done まで待ち、回答のメッセージを返す。
async fn answer(store: &ChatStore, c: &AuthContext, thread_id: Uuid, text: &str) -> Message {
    let res = store
        .post_message(c, thread_id, text, &[], None, Some(false), false, &[], None)
        .await
        .unwrap();
    let mut rx = store.event_stream(res.run_id, 0);
    for _ in 0..2000 {
        let next = tokio::time::timeout(Duration::from_secs(180), rx.next())
            .await
            .expect("イベント待ちがタイムアウト");
        let Some(ev) = next else { break };
        if let StreamEventKind::Error { message } = &ev.event {
            panic!("生成失敗: {message}");
        }
        if matches!(ev.event, StreamEventKind::Done { .. }) {
            break;
        }
    }
    message(store, c, thread_id, res.assistant_message_id).await
}

async fn message(store: &ChatStore, c: &AuthContext, thread_id: Uuid, id: Uuid) -> Message {
    store
        .get_messages(c, thread_id, None)
        .await
        .unwrap()
        .into_iter()
        .find(|m| m.id == id)
        .expect("メッセージ")
}

/// 本文の最初の図のブロックの識別子（`{hash}:a1` の hash）。
fn block_hash(m: &Message) -> String {
    let text = m
        .content
        .iter()
        .find_map(|b| match b {
            ContentBlock::Text { text } if text.contains("```ui") => Some(text.clone()),
            _ => None,
        })
        .expect("図のある本文");
    let start = text.find("```ui\n").unwrap() + 6;
    let end = start + text[start..].find("\n```").unwrap();
    yui::block_hash(&text[start..end])
}

fn req(action_id: &str, values: Value, key: Option<&str>) -> YuiActionRequest {
    YuiActionRequest {
        action_id: action_id.into(),
        values: values.as_object().cloned().unwrap_or_default(),
        idempotency_key: key.map(str::to_string),
    }
}

async fn run_count(pool: &PgPool, tenant: &str) -> i64 {
    sqlx::query_scalar("SELECT count(*) FROM workflow_run WHERE tenant_id = $1")
        .bind(tenant)
        .fetch_one(pool)
        .await
        .unwrap()
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn workflow_buttons_are_bound_validated_deduplicated_and_recorded() {
    let Some(pool) = setup().await else { return };
    let e = env(pool).await;
    let tenant = format!("t-{}", Uuid::new_v4().simple());
    let alice = ctx(&tenant, "alice");
    let bob = ctx(&tenant, "bob");
    let wf = format!("expense-{}", &Uuid::new_v4().simple().to_string()[..8]);
    e.workflows
        .create(&alice, &expense_ir(&wf), &Catalog::default(), None)
        .await
        .expect("ワークフローを保存");
    let thread = e
        .store
        .create_thread(&alice, "t", false, None, None)
        .await
        .unwrap();

    // 回答の確定時に、`-> workflow 名前` が回答した主体の権限で解決されて束縛になる。
    let m = answer(&e.store, &alice, thread.id, &format!("yui:app {wf}")).await;
    let hash = block_hash(&m);
    let submit = format!("{hash}:a1");
    let consult = format!("{hash}:a2");
    assert_eq!(
        m.bound_actions,
        vec![submit.clone()],
        "ワークフローのボタンだけが束縛を持つ"
    );
    let call = |c: AuthContext, r: YuiActionRequest| {
        let e = &e;
        async move {
            e.store
                .invoke_yui_action(&c, &e.dispatcher, thread.id, m.id, r, None)
                .await
        }
    };
    let ok_values = json!({ "amount": 1200, "category": "交通費", "ignored": "x" });

    // ① 閲覧者は押せない（値の検証より先に断る）。
    let err = call(
        bob.clone(),
        req(&submit, ok_values.clone(), Some("bob-key-0001")),
    )
    .await
    .unwrap_err();
    assert!(matches!(err, gui::ActionError::Forbidden), "{err:?}");

    // ② 部品の定義で検証する（必須・選択肢）。
    let err = call(
        alice.clone(),
        req(&submit, json!({ "category": "食費" }), Some("key-00000001")),
    )
    .await
    .unwrap_err();
    let gui::ActionError::Invalid(msg) = err else {
        panic!("{err:?}")
    };
    assert!(msg.contains("金額"), "{msg}");
    assert!(msg.contains("区分"), "{msg}");

    // ③ ワークフローのボタンは冪等キーが要る。
    let err = call(alice.clone(), req(&submit, ok_values.clone(), None))
        .await
        .unwrap_err();
    assert!(matches!(err, gui::ActionError::Invalid(_)), "{err:?}");

    // ④ input_schema でも検証する（部品の定義は通るが、ワークフローの上限を超える）。
    let err = call(
        alice.clone(),
        req(
            &submit,
            json!({ "amount": 500000, "category": "交通費" }),
            Some("key-00000002"),
        ),
    )
    .await
    .unwrap_err();
    let gui::ActionError::Invalid(msg) = err else {
        panic!("{err:?}")
    };
    assert!(msg.contains("amount"), "{msg}");
    assert_eq!(
        run_count(&e.pool, &tenant).await,
        0,
        "検証に落ちた送信は run を作らない"
    );

    // ⑤ 起動する。部品の定義に無い値（ignored）は渡らない（additionalProperties: false を通る）。
    let out = call(
        alice.clone(),
        req(&submit, ok_values.clone(), Some("key-00000003")),
    )
    .await
    .expect("起動");
    let run_id: Uuid = serde_json::from_value(out["run_id"].clone()).unwrap();
    let (input, principal): (Value, String) = sqlx::query_as(
        "SELECT input, principal FROM workflow_run WHERE tenant_id = $1 AND run_id = $2",
    )
    .bind(&tenant)
    .bind(run_id)
    .fetch_one(&e.pool)
    .await
    .unwrap();
    assert_eq!(input, json!({ "amount": 1200, "category": "交通費" }));
    assert_eq!(principal, "alice", "実行は押した本人");

    // ⑥ 同じ冪等キーの再送は前の run を返し、run を増やさない。
    let again = call(
        alice.clone(),
        req(&submit, ok_values.clone(), Some("key-00000003")),
    )
    .await
    .expect("再送");
    assert_eq!(again["run_id"], out["run_id"]);
    assert_eq!(again["duplicate"], json!(true));
    assert_eq!(run_count(&e.pool, &tenant).await, 1);

    // 検証に落ちた送信（key-00000002）は記録に残らず、同じキーで押し直せる。
    let retry = call(
        alice.clone(),
        req(
            &submit,
            json!({ "amount": 800, "category": "交際費" }),
            Some("key-00000002"),
        ),
    )
    .await
    .expect("押し直し");
    assert_ne!(retry["run_id"], out["run_id"]);

    // ⑦ 起動の記録はメッセージと一緒に返り、終わった状態は run から読んで書き戻す。
    let m2 = message(&e.store, &alice, thread.id, m.id).await;
    assert_eq!(m2.ui_runs.len(), 2);
    assert_eq!(m2.ui_runs[0].run_id, Some(run_id));
    assert_eq!(m2.ui_runs[0].label, "申請する");
    assert_eq!(m2.ui_runs[0].workflow_name, wf);
    assert_eq!(m2.ui_runs[0].invoked_by, "alice");
    assert!(!m2.ui_runs[0].status.is_terminal());
    let shown: Vec<(String, String)> = m2.ui_runs[0]
        .values
        .iter()
        .map(|v| (v.label.clone(), v.value.clone()))
        .collect();
    assert_eq!(
        shown,
        vec![
            ("金額".into(), "1,200円".into()),
            ("区分".into(), "交通費".into())
        ]
    );
    sqlx::query(
        "UPDATE workflow_run SET status = 'succeeded' WHERE tenant_id = $1 AND run_id = $2",
    )
    .bind(&tenant)
    .bind(run_id)
    .execute(&e.pool)
    .await
    .unwrap();
    let m3 = message(&e.store, &bob, thread.id, m.id).await;
    assert_eq!(
        m3.ui_runs[0].status,
        UiActionRunStatus::Succeeded,
        "閲覧者にも結果は見える"
    );
    let stored: Option<String> =
        sqlx::query_scalar("SELECT status FROM ui_action_run WHERE tenant_id = $1 AND run_id = $2")
            .bind(&tenant)
            .bind(run_id)
            .fetch_one(&e.pool)
            .await
            .unwrap();
    assert_eq!(
        stored.as_deref(),
        Some("succeeded"),
        "終わった状態は記録に写される"
    );
    // 実行履歴の GC で run が消えても、結果は記録に残る。
    sqlx::query("DELETE FROM workflow_run WHERE tenant_id = $1 AND run_id = $2")
        .bind(&tenant)
        .bind(run_id)
        .execute(&e.pool)
        .await
        .unwrap();
    let m4 = message(&e.store, &alice, thread.id, m.id).await;
    assert_eq!(m4.ui_runs[0].status, UiActionRunStatus::Succeeded);
    // 同じキーの再送は起動しないが、監査には残る。
    let replays: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM audit_log WHERE tenant_id = $1 AND action = 'ui_action.invoke' \
         AND metadata ->> 'replay' = 'true'",
    )
    .bind(&tenant)
    .fetch_one(&e.pool)
    .await
    .unwrap();
    assert_eq!(replays, 1);

    // ⑧ 会話に送るボタンは、サーバーが検証した値で文を作り、1 回だけ送れる。
    let sent = call(
        alice.clone(),
        req(&consult, json!({ "内容": "上限を知りたい" }), None),
    )
    .await
    .expect("会話に送る");
    assert!(sent["result"]["run_id"].is_string());
    let err = call(
        alice.clone(),
        req(&consult, json!({ "内容": "もう一度" }), None),
    )
    .await
    .unwrap_err();
    assert!(matches!(err, gui::ActionError::AlreadyInvoked), "{err:?}");
    let all = e.store.get_messages(&alice, thread.id, None).await.unwrap();
    let posted = all
        .iter()
        .filter(|x| x.role == Role::User)
        .find_map(|x| match x.content.first() {
            Some(ContentBlock::Text { text }) if text.starts_with("【相談】") => {
                Some(text.clone())
            }
            _ => None,
        })
        .expect("会話に送った発話");
    assert_eq!(posted, "【相談】相談する\n内容: 上限を知りたい");
    let card = all.iter().find(|x| x.id == m.id).unwrap();
    assert_eq!(card.invoked_actions, vec![consult.clone()]);

    // ⑨ 本文に無いボタン・別のメッセージの id は押せない。
    let err = call(alice.clone(), req(&format!("{hash}:a9"), json!({}), None))
        .await
        .unwrap_err();
    assert!(matches!(err, gui::ActionError::NotFound), "{err:?}");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn unresolved_workflows_and_questions() {
    let Some(pool) = setup().await else { return };
    let e = env(pool).await;
    let tenant = format!("t-{}", Uuid::new_v4().simple());
    let alice = ctx(&tenant, "alice");
    let thread = e
        .store
        .create_thread(&alice, "t", false, None, None)
        .await
        .unwrap();

    // 解決できないワークフローのボタンは束縛を持たず、押せない。
    let m = answer(&e.store, &alice, thread.id, "yui:app no-such-workflow").await;
    assert!(m.bound_actions.is_empty());
    let hash = block_hash(&m);
    let err = e
        .store
        .invoke_yui_action(
            &alice,
            &e.dispatcher,
            thread.id,
            m.id,
            req(
                &format!("{hash}:a1"),
                json!({ "amount": 10, "category": "交通費" }),
                Some("key-00000001"),
            ),
            None,
        )
        .await
        .unwrap_err();
    assert!(matches!(err, gui::ActionError::NotFound), "{err:?}");

    // ask: 回答をサーバーで検証し（選択肢に無い答えは断る）、質問と答えの文として送る。
    let q = answer(&e.store, &alice, thread.id, "yui:ask").await;
    let key = format!("{}:a1", block_hash(&q));
    let err = e
        .store
        .invoke_yui_action(
            &alice,
            &e.dispatcher,
            thread.id,
            q.id,
            req(&key, json!({ "q1": "九州" }), None),
            None,
        )
        .await
        .unwrap_err();
    assert!(matches!(err, gui::ActionError::Invalid(_)), "{err:?}");
    e.store
        .invoke_yui_action(
            &alice,
            &e.dispatcher,
            thread.id,
            q.id,
            req(
                &key,
                json!({ "q1": "関東", "q2": ["費用", "サポート体制"] }),
                None,
            ),
            None,
        )
        .await
        .expect("回答を送る");
    let all = e.store.get_messages(&alice, thread.id, None).await.unwrap();
    assert!(all.iter().any(|x| x.role == Role::User
        && matches!(x.content.first(), Some(ContentBlock::Text { text })
            if text == "対象の地域は\n→ 関東\n\n重視する点\n→ 費用、サポート体制")));
}
