//! generative_ui content block の結合テスト（Task 6.4/6.5 受け入れ条件）。
//!
//! stub LLM の `genui:` プレフィックスで **ChatWorker → emit_ui（実検証）→ sink → SSE/projection**
//! の実経路を走らせる。`STORAGE_TEST_DATABASE_URL` が設定されている時のみ実行。
//! - `genui:form`: 検証済みブロックが SSE 配信・永続化される
//! - `genui:bad`: 検証拒否 → ブロック化されずテキストでフォールバック・監査 Deny が残る
//! - chat.submit / 未宣言アクションのディスパッチ（実 ChatStore・実監査）

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

/// ワーカー＋検証層＋ストア一式を組む（emit_ui は agent_mode でのみ提示される）。
async fn spawn_worker(pool: &PgPool) -> (ChatStore, Arc<gui::SpecValidator>) {
    let store = ChatStore::connect(pool.clone(), Arc::new(AllowAll), None)
        .await
        .unwrap();
    let artifacts = Arc::new(artifact::ArtifactStore::new(
        pool.clone(),
        Arc::new(AllowAll),
    ));
    let validator = Arc::new(gui::SpecValidator::new(artifacts, pool.clone()));
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
            ui_validator: Some(Arc::clone(&validator)),
            skill_artifacts: None,
            skill_catalog: None,
            workflow_store: None,
            workflow_catalog: None,
            collab: None,
            tabular: None,
            office: None,
            office_live: None,
            office_creator: None,
        },
        WorkerConfig {
            system_prompt: "あなたはアシスタントです。".into(),
            model: Some("m".into()),
            // Coverage（cargo-llvm-cov）は計装＋全テスト並列で 1 step が大きく遅くなる。
            // 30s では worker のリース失効で run が orphan 化して flake るため余裕を持たせる。
            lease_secs: 120,
            max_steps: 4,
            ..Default::default()
        },
    );
    worker.spawn(1);
    (store, validator)
}

/// 発話 → done まで drain し、観測イベント種を返す。
async fn run_to_done(
    store: &ChatStore,
    c: &AuthContext,
    thread_id: Uuid,
    text: &str,
) -> (Uuid, Vec<StreamEventKind>) {
    let res = store
        .post_message(c, thread_id, text, &[], None, Some(true), false, &[], None)
        .await
        .unwrap();
    let mut rx = store.event_stream(res.run_id, 0);
    let mut events = Vec::new();
    for _ in 0..500 {
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
async fn validated_generative_ui_is_streamed_and_persisted() {
    let Some(pool) = setup().await else { return };
    let tenant = format!("t-{}", Uuid::new_v4());
    let (store, _validator) = spawn_worker(&pool).await;
    let c = ctx(&tenant);
    let thread = store
        .create_thread(&c, "t", true, None, None)
        .await
        .unwrap();

    let (asst_id, events) = run_to_done(&store, &c, thread.id, "genui:form").await;

    // 通常チャット（agent_mode=false）でも generative UI が出る＝モデル裁量ループが既定
    // （issue #102 の核・旧・無条件 RAG の classic 経路ではツールが提示されなかった）。同じ
    // worker を使い回して jobq 相乗りのフレークを避ける。
    {
        let res = store
            .post_message(
                &c,
                thread.id,
                "genui:chart",
                &[],
                None,
                Some(false),
                false,
                &[],
                None,
            )
            .await
            .unwrap();
        let mut rx = store.event_stream(res.run_id, 0);
        let mut saw_ui = false;
        for _ in 0..500 {
            let next = tokio::time::timeout(Duration::from_secs(180), rx.next())
                .await
                .expect("イベント待ちタイムアウト");
            let Some(ev) = next else { break };
            match ev.event {
                StreamEventKind::GenerativeUi { .. } => saw_ui = true,
                StreamEventKind::Done { .. } => break,
                StreamEventKind::Error { message } => panic!("生成失敗: {message}"),
                _ => {}
            }
        }
        assert!(
            saw_ui,
            "通常チャットでもモデルが emit_ui を呼べる（Chat プロファイルループが既定）"
        );
    }

    // SSE に generative_ui イベントが流れる（6.4 受け入れ条件①）。
    let sse_spec = events.iter().find_map(|e| match e {
        StreamEventKind::GenerativeUi { spec } => Some(spec.clone()),
        _ => None,
    });
    let sse_spec = sse_spec.expect("SSE に generative_ui イベントが出ること");
    assert_eq!(sse_spec["root"]["component"], "form");

    // 永続化された message.content にも検証済みブロックが残る（6.4 受け入れ条件③）。
    let msgs = store.get_messages(&c, thread.id, None).await.unwrap();
    let asst = msgs.iter().find(|m| m.id == asst_id).unwrap();
    let block_spec = asst.content.iter().find_map(|b| match b {
        ContentBlock::GenerativeUi { spec } => Some(spec.clone()),
        _ => None,
    });
    let block_spec = block_spec.expect("generative_ui ブロックが確定保存されること");
    // 保存されたスペックは検証済み形式（actions が宣言され、参照が解決可能）。
    assert_eq!(block_spec["version"], 1);
    assert_eq!(block_spec["actions"][0]["handler"], "chat.submit");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn invalid_spec_falls_back_to_text_and_is_audited() {
    let Some(pool) = setup().await else { return };
    let tenant = format!("t-{}", Uuid::new_v4());
    let (store, _validator) = spawn_worker(&pool).await;
    let c = ctx(&tenant);
    let thread = store
        .create_thread(&c, "t", true, None, None)
        .await
        .unwrap();

    let (asst_id, events) = run_to_done(&store, &c, thread.id, "genui:bad").await;

    // generative_ui イベントは一切流れない（検証拒否・6.4 受け入れ条件②）。
    assert!(
        !events
            .iter()
            .any(|e| matches!(e, StreamEventKind::GenerativeUi { .. })),
        "不正スペックがブロック化されないこと"
    );

    // ブロックは保存されず、テキスト（フォールバック応答）が残る。
    let msgs = store.get_messages(&c, thread.id, None).await.unwrap();
    let asst = msgs.iter().find(|m| m.id == asst_id).unwrap();
    assert!(
        !asst
            .content
            .iter()
            .any(|b| matches!(b, ContentBlock::GenerativeUi { .. })),
        "未検証スペックが永続化されないこと"
    );
    assert!(
        asst.content
            .iter()
            .any(|b| matches!(b, ContentBlock::Text { .. })),
        "テキストのフォールバック応答が残ること"
    );

    // 検証拒否が監査に残る（6.12）。
    let denies: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM audit_log \
         WHERE tenant_id = $1 AND action = 'ui_spec.validate' AND decision = 'deny'",
    )
    .bind(&tenant)
    .fetch_one(&pool)
    .await
    .unwrap();
    assert!(denies >= 1, "ui_spec.validate の Deny 監査が残ること");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn chat_submit_action_posts_message_and_undeclared_is_denied() {
    let Some(pool) = setup().await else { return };
    let tenant = format!("t-{}", Uuid::new_v4());
    let (store, _validator) = spawn_worker(&pool).await;
    let c = ctx(&tenant);
    let thread = store
        .create_thread(&c, "t", true, None, None)
        .await
        .unwrap();

    // UI ブロックを実経路で作る。
    let (asst_id, _) = run_to_done(&store, &c, thread.id, "genui:form").await;
    let msg = store
        .get_message(&c, thread.id, asst_id, None)
        .await
        .unwrap();
    let doc = msg
        .content
        .iter()
        .find_map(|b| match b {
            ContentBlock::GenerativeUi { spec } => {
                serde_json::from_value::<gui::UiSpecDoc>(spec.clone()).ok()
            }
            _ => None,
        })
        .expect("検証済み UI ブロック");

    // dispatcher（chat.submit ハンドラ登録済み）でフォーム送信 → 新しい user メッセージ。
    let mut dispatcher =
        gui::ActionDispatcher::new(storage::audit::AuditRecorder::new(pool.clone()));
    dispatcher.register_handler(Arc::new(chat::ChatSubmitHandler::new(store.clone())));
    // 単発アクションの実行台帳（#410）。本番の配線（wiring_gui）と同じ対で持たせる。
    dispatcher.set_ledger(Arc::new(chat::ChatActionLedger::new(store.clone())));
    let source = gui::ActionSource::ChatMessage {
        thread_id: thread.id,
        message_id: asst_id,
    };
    let before = store.get_messages(&c, thread.id, None).await.unwrap().len();
    let result = dispatcher
        .dispatch(
            &c,
            &source,
            &doc,
            "submit",
            serde_json::json!({ "comment": "とても良い" }),
            None,
        )
        .await
        .expect("chat.submit 実行");
    assert!(result["result"]["run_id"].is_string());
    let msgs = store.get_messages(&c, thread.id, None).await.unwrap();
    assert!(
        msgs.len() > before,
        "フォーム送信で user メッセージが増えること"
    );
    assert!(msgs.iter().any(|m| m
        .content
        .iter()
        .any(|b| matches!(b, ContentBlock::Text { text } if text.contains("とても良い")))));

    // 同じカードの同じアクションは**二度実行できない**（#410）。表示だけ直しても押せて
    // しまうので、二重送信は入口で潰す。計画カードならここが調査 run の二重起動になる。
    let err = dispatcher
        .dispatch(
            &c,
            &source,
            &doc,
            "submit",
            serde_json::json!({ "comment": "もう一度" }),
            None,
        )
        .await
        .expect_err("二度目は拒否される");
    assert!(matches!(err, gui::ActionError::AlreadyInvoked), "{err:?}");
    let after_retry = store.get_messages(&c, thread.id, None).await.unwrap();
    assert_eq!(
        after_retry.len(),
        msgs.len(),
        "拒否された二度目は発話も生成も作らないこと"
    );
    // 実行済みは**メッセージと一緒に返る**（カードの「送信済み」表示の根拠）。
    let card_message = after_retry
        .iter()
        .find(|m| m.id == asst_id)
        .expect("カードを出したメッセージ");
    assert_eq!(card_message.invoked_actions, vec!["submit".to_string()]);
    assert!(
        after_retry
            .iter()
            .filter(|m| m.id != asst_id)
            .all(|m| m.invoked_actions.is_empty()),
        "実行していないメッセージには付かないこと"
    );
    // 台帳には誰が押したか・生まれた run が残る（監査との突合用）。
    let (invoked_by, run_id): (String, Option<Uuid>) = sqlx::query_as(
        "SELECT invoked_by, run_id FROM ui_action_invocation \
         WHERE tenant_id = $1 AND message_id = $2 AND action_id = 'submit'",
    )
    .bind(&tenant)
    .bind(asst_id)
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!(invoked_by, "alice");
    assert!(run_id.is_some(), "生まれた run が紐づくこと");

    // 台帳が未配線なら**実行しない**（二重送信を抑止できない状態で run を作らない）。
    let mut no_ledger =
        gui::ActionDispatcher::new(storage::audit::AuditRecorder::new(pool.clone()));
    no_ledger.register_handler(Arc::new(chat::ChatSubmitHandler::new(store.clone())));
    let err = no_ledger
        .dispatch(&c, &source, &doc, "submit", serde_json::json!({}), None)
        .await
        .expect_err("台帳が無ければ単発アクションは実行しない");
    assert!(matches!(err, gui::ActionError::Unavailable(_)), "{err:?}");

    // 宣言済みアクションのみ実行できる（未宣言 id は NotFound＋Deny 監査・6.5）。
    let err = dispatcher
        .dispatch(
            &c,
            &source,
            &doc,
            "not-declared",
            serde_json::json!({}),
            None,
        )
        .await
        .expect_err("未宣言は拒否");
    assert!(matches!(err, gui::ActionError::NotFound));
    let denies: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM audit_log \
         WHERE tenant_id = $1 AND action = 'ui_action.invoke' AND decision = 'deny'",
    )
    .bind(&tenant)
    .fetch_one(&pool)
    .await
    .unwrap();
    assert!(denies >= 1, "未宣言アクションの Deny 監査が残ること");

    // 実行成功も Allow で監査に残る（誰が・どの束縛を）。
    let allows: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM audit_log \
         WHERE tenant_id = $1 AND action = 'ui_action.invoke' AND decision = 'allow'",
    )
    .bind(&tenant)
    .fetch_one(&pool)
    .await
    .unwrap();
    assert!(allows >= 1, "ui_action.invoke の Allow 監査が残ること");
}

/// カード回答（`chat.submit`）が**カードを出した run の skill ピンを継ぐ**こと（#402）。
///
/// スラッシュコマンド起動の skill は run 単位ピン（thread ピンではない）。ここが落ちると
/// 手順書もフェーズ宣言（`plan_first`）も 2 ターン目から消え、「質問 → 計画 → 実行」は
/// 最初の 1 手だけ守られて残りが素通りになる。実 LLM 検証で実際にそうなった。
#[tokio::test]
async fn card_submit_carries_run_skill_pins_and_variant() {
    use gui::ActionHandler;

    let Some(pool) = setup().await else { return };
    let tenant = format!("t-{}", Uuid::new_v4());
    let (store, _validator) = spawn_worker(&pool).await;
    let c = ctx(&tenant);
    let thread = store
        .create_thread(&c, "t", true, None, None)
        .await
        .unwrap();

    // スラッシュコマンド起動を模す: 発話単位の skill ピンに実行前フェーズが焼かれている。
    let pin = chat::SkillPin {
        skill_id: Uuid::new_v4(),
        skill_version: 1,
        command_args: Some(String::new()),
    };
    let posted = store
        .post_message(
            &c,
            thread.id,
            "/deep-research リモートワークの生産性",
            &[],
            None,
            Some(true),
            true,
            std::slice::from_ref(&pin),
            None,
        )
        .await
        .unwrap();
    let pins_of = |run_id: Uuid| {
        let pool = pool.clone();
        async move {
            sqlx::query_scalar::<_, serde_json::Value>(
                "SELECT skill_pins FROM generation_run WHERE run_id = $1",
            )
            .bind(run_id)
            .fetch_one(&pool)
            .await
            .unwrap()
        }
    };
    assert_eq!(
        pins_of(posted.run_id).await[0]["command_args"],
        serde_json::json!(""),
        "発話時に起動 variant がピンへ焼かれること"
    );

    // カードからの回答（コマンドリテラルを持たない発話）。
    let handler = chat::ChatSubmitHandler::new(store.clone());
    let result = handler
        .invoke(
            &c,
            &gui::ActionSource::ChatMessage {
                thread_id: thread.id,
                message_id: posted.assistant_message_id,
            },
            serde_json::json!({ "対象期間": "2020 年以降" }),
            None,
        )
        .await
        .expect("chat.submit 実行");
    let next_run: Uuid = result["run_id"].as_str().unwrap().parse().unwrap();
    let carried = pins_of(next_run).await;
    assert_eq!(
        carried[0]["skill_id"].as_str().unwrap(),
        pin.skill_id.to_string(),
        "カード回答でも skill ピンが継がれること"
    );
    assert_eq!(
        carried[0]["command_args"],
        serde_json::json!(""),
        "起動 variant も継がれること（ここが落ちると 2 ターン目から門が効かない）"
    );
}
