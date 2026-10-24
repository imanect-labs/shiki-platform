//! 生成ワーカーのエンドツーエンド結合テスト（Task 3.5 / 3.11）。
//!
//! `STORAGE_TEST_DATABASE_URL` が設定されている時のみ実行。実 LLM の代わりに決定的 stub
//! プロバイダを使い、**ChatWorker → llm-gateway(stub) → sink(append+projection) → SSE event_stream**
//! の実コード経路を走らせて、送信→ストリーミング→確定→復元購読が通ることを検証する。

#![allow(
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

/// 既定プロバイダが OpenAI 互換の `base_url`（[`rejecting_provider`]）を指す gateway。
fn rejecting_gateway(pool: PgPool, base_url: String) -> LlmGateway {
    let config = GatewayConfig {
        provider: ProviderConfig {
            kind: ProviderKind::Openai,
            base_url: Some(base_url),
            api_key: Some("k".into()),
            ..Default::default()
        },
        catalog: ModelCatalog {
            default_model: "m".into(),
            models: vec![ModelEntry {
                id: "m".into(),
                ..Default::default()
            }],
        },
        ..Default::default()
    };
    LlmGateway::build(pool, reqwest::Client::new(), config).expect("gateway")
}

/// ワーカーの依存束（全 None＝ツール無しの素の生成）。テスト間で共有する。
fn test_deps(gateway: LlmGateway) -> chat::WorkerDeps {
    chat::WorkerDeps {
        gateway,
        search: None,
        sandbox: None,
        artifacts: None,
        web_search: None,
        parser: None,
        storage: None,
        workflow_resolver: None,
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
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn worker_generates_streams_and_persists_projection() {
    let Some(pool) = setup().await else { return };
    let tenant = format!("t-{}", uuid::Uuid::new_v4());
    let store = ChatStore::connect(pool.clone(), Arc::new(AllowAll), None)
        .await
        .unwrap();
    let gateway = stub_gateway(pool.clone());
    let worker = ChatWorker::new(
        pool.clone(),
        store.clone(),
        test_deps(gateway),
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
    // 生成ワーカーを起動（jobq を消費）。
    worker.spawn(1);

    let c = ctx(&tenant);
    let thread = store
        .create_thread(&c, "t", false, None, None)
        .await
        .unwrap();
    let res = store
        .post_message(
            &c,
            thread.id,
            "hello world",
            &[],
            None,
            Some(false),
            false,
            &[],
            None,
        )
        .await
        .unwrap();

    // SSE 相当の event_stream を drain し、トークン→done を受け取る。
    let mut rx = store.event_stream(&c.tenant_id, res.run_id, 0);
    let mut text = String::new();
    let mut done = false;
    let mut title_event = None;
    for _ in 0..500 {
        let next = tokio::time::timeout(Duration::from_secs(15), rx.next())
            .await
            .expect("イベント待ちがタイムアウト（ワーカーが生成しない）");
        let Some(ev) = next else { break };
        match ev.event {
            StreamEventKind::Token { text: t } => text.push_str(&t),
            StreamEventKind::ThreadTitle { title } => title_event = Some(title),
            StreamEventKind::Done { .. } => {
                done = true;
                break;
            }
            StreamEventKind::Error { message } => panic!("生成失敗: {message}"),
            _ => {}
        }
    }
    assert!(done, "done イベントを受け取ること");
    assert!(
        text.contains("hello world"),
        "stub 応答が本文を含むこと: {text:?}"
    );

    // message.content に projection が書き戻されている（接続非依存生成の確定）。
    let msgs = store.get_messages(&c, thread.id, None).await.unwrap();
    let asst = msgs
        .iter()
        .find(|m| m.id == res.assistant_message_id)
        .expect("assistant メッセージ");
    let has_text = asst.content.iter().any(|b| match b {
        ContentBlock::Text { text } => text.contains("hello world"),
        _ => false,
    });
    assert!(has_text, "確定メッセージに本文 projection が残ること");

    // 自動タイトル: 最初の発話から付き、`done` より前にイベントが届く（stub は包みの中の
    // 最初の行＝発話をそのまま返す）。
    assert_eq!(
        title_event.as_deref(),
        Some("hello world"),
        "thread_title イベントが done より前に届くこと"
    );
    let titled = store.get_thread(&c, thread.id, None).await.unwrap();
    assert_eq!(
        titled.title, "hello world",
        "仮タイトルが生成タイトルに置き換わる"
    );
    // 確定後は付け直さない（材料を返さない）。
    assert!(store
        .title_seed(thread.id, &tenant)
        .await
        .unwrap()
        .is_none());

    // 会計が刻まれている（tenant スコープ・冪等キー）。
    let usage_count: i64 =
        sqlx::query_scalar("SELECT count(*) FROM llm_usage WHERE tenant_id = $1")
            .bind(&tenant)
            .fetch_one(&pool)
            .await
            .unwrap();
    assert!(usage_count >= 1, "llm_usage に会計行が刻まれること");

    // コンテキスト使用量が記録され、再訪時に引ける（チャット画面のコンテキスト表示）。
    let usage = store
        .latest_context_usage(thread.id, &tenant)
        .await
        .unwrap()
        .expect("context_usage が記録されること");
    assert!(usage.used_tokens > 0);
    assert_eq!(
        usage.used_tokens,
        usage.categories.iter().map(|c| c.tokens).sum::<u64>(),
        "内訳の合計が総量と一致すること"
    );
    // 別テナントからは引けない（tenant スコープ）。
    assert!(store
        .latest_context_usage(thread.id, "other-tenant")
        .await
        .unwrap()
        .is_none());
}

/// system プロンプトの末尾に、run の作成時刻から決めた「今日の日付」が入り、実際にモデルへ
/// 届く（#546・stub の `echosystem:` で受け取った system を本文に返させて確かめる）。
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn system_prompt_carries_todays_date_from_run_creation() {
    let Some(pool) = setup().await else { return };
    let tenant = format!("t-{}", uuid::Uuid::new_v4());
    let store = ChatStore::connect(pool.clone(), Arc::new(AllowAll), None)
        .await
        .unwrap();
    let worker = ChatWorker::new(
        pool.clone(),
        store.clone(),
        test_deps(stub_gateway(pool.clone())),
        WorkerConfig {
            system_prompt: "あなたはアシスタントです。".into(),
            model: Some("m".into()),
            lease_secs: 120,
            max_steps: 4,
            ..Default::default()
        },
    );
    worker.spawn(1);

    let c = ctx(&tenant);
    let thread = store
        .create_thread(&c, "t", false, None, None)
        .await
        .unwrap();
    let res = store
        .post_message(
            &c,
            thread.id,
            "echosystem:",
            &[],
            None,
            Some(false),
            false,
            &[],
            None,
        )
        .await
        .unwrap();

    let mut rx = store.event_stream(&c.tenant_id, res.run_id, 0);
    let mut text = String::new();
    for _ in 0..500 {
        let next = tokio::time::timeout(Duration::from_secs(15), rx.next())
            .await
            .expect("イベント待ちがタイムアウト（ワーカーが生成しない）");
        let Some(ev) = next else { break };
        match ev.event {
            StreamEventKind::Token { text: t } => text.push_str(&t),
            StreamEventKind::Done { .. } => break,
            StreamEventKind::Error { message } => panic!("生成失敗: {message}"),
            _ => {}
        }
    }
    // 期待する日付は run の作成時刻を既定のタイムゾーン（Asia/Tokyo）で見たもの。
    let created: chrono::DateTime<chrono::Utc> =
        sqlx::query_scalar("SELECT created_at FROM generation_run WHERE run_id = $1")
            .bind(res.run_id)
            .fetch_one(&pool)
            .await
            .unwrap();
    let d = created.with_timezone(&chrono_tz::Asia::Tokyo).date_naive();
    let want = format!(
        "{}年{}月{}日",
        chrono::Datelike::year(&d),
        chrono::Datelike::month(&d),
        chrono::Datelike::day(&d)
    );
    assert!(
        text.starts_with("system: あなたはアシスタントです。"),
        "設定の system がそのまま先頭に来ること: {text:?}"
    );
    assert!(
        text.contains(&want) && text.contains("（Asia/Tokyo）"),
        "今日の日付（{want}）が system に入ること: {text:?}"
    );
}

/// 本人のカスタム指示（#900）が区切り付きで system に入り、実際にモデルへ届く。置く位置は
/// 設定の system の後・今日の日付の前。エージェント経路と古典経路の両方で確かめる。
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn custom_instructions_reach_the_model_in_system_prompt() {
    for classic_rag in [false, true] {
        // 経路ごとに専用 DB（ほかのテストのワーカーに run を拾わせない＝確かめたい経路で生成させる）。
        let Some(pool) = isolated_setup("custom_instr").await else {
            return;
        };
        let tenant = format!("t-{}", uuid::Uuid::new_v4());
        let store = ChatStore::connect(pool.clone(), Arc::new(AllowAll), None)
            .await
            .unwrap();
        let worker = ChatWorker::new(
            pool.clone(),
            store.clone(),
            test_deps(stub_gateway(pool.clone())),
            WorkerConfig {
                system_prompt: "あなたはアシスタントです。".into(),
                model: Some("m".into()),
                lease_secs: 120,
                max_steps: 4,
                classic_rag,
                ..Default::default()
            },
        );
        let handles = worker.spawn(1);

        let c = ctx(&tenant);
        store
            .put_custom_instructions(
                &c,
                chat::CustomInstructions {
                    about: "法人営業。提案書の作成が多い。".into(),
                    style: "結論を先に。".into(),
                    tone: chat::custom_instructions::CustomInstructionTone::Concise,
                },
                None,
            )
            .await
            .unwrap();
        let thread = store
            .create_thread(&c, "t", false, None, None)
            .await
            .unwrap();
        let res = store
            .post_message(
                &c,
                thread.id,
                "echosystem:",
                &[],
                None,
                Some(false),
                false,
                &[],
                None,
            )
            .await
            .unwrap();
        let mut rx = store.event_stream(&c.tenant_id, res.run_id, 0);
        let mut text = String::new();
        for _ in 0..500 {
            let next = tokio::time::timeout(Duration::from_secs(15), rx.next())
                .await
                .expect("イベント待ちがタイムアウト（ワーカーが生成しない）");
            let Some(ev) = next else { break };
            match ev.event {
                StreamEventKind::Token { text: t } => text.push_str(&t),
                StreamEventKind::Done { .. } => break,
                StreamEventKind::Error { message } => panic!("生成失敗: {message}"),
                _ => {}
            }
        }
        assert!(
            text.starts_with("system: あなたはアシスタントです。\n\n# ユーザーのカスタム指示\n"),
            "設定の system の直後にカスタム指示の節が来ること（classic_rag={classic_rag}）: {text:?}"
        );
        for want in [
            "<custom_instructions>",
            "[ユーザーについて]\n法人営業。提案書の作成が多い。",
            "[答え方の希望]\n結論を先に。",
            "[口調]\n前置きや繰り返しを省き",
        ] {
            assert!(
                text.contains(want),
                "{want} が届くこと（classic_rag={classic_rag}）: {text:?}"
            );
        }
        let section_end = text.find("</custom_instructions>").expect("閉じ区切り");
        let today = text.find("今日は ").expect("今日の日付");
        assert!(section_end < today, "日付は最後に置く: {text:?}");
        for h in handles {
            h.abort();
        }
    }
}

/// 生成中に送った発話は**受理され、順番に 1 本ずつ**生成される。
///
/// 並行に走らせると後の run が前の run の出力を含まない履歴で生成し（発話順と応答が食い違う）、
/// 同じワークスペースへ同時に書き、承認カードが 2 本同時に出る。UI の「順番待ち」はこの
/// サーバ側直列化が前提で、ページを離れても消えないのはメッセージがサーバにあるため。
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn messages_sent_during_generation_are_queued_and_run_in_order() {
    let Some(pool) = setup().await else { return };
    let tenant = format!("t-{}", uuid::Uuid::new_v4());
    let store = ChatStore::connect(pool.clone(), Arc::new(AllowAll), None)
        .await
        .unwrap();
    let gateway = stub_gateway(pool.clone());
    // 並列 consumer を複数立てて「同時に claim され得る」状況を作る。
    ChatWorker::new(
        pool.clone(),
        store.clone(),
        test_deps(gateway),
        WorkerConfig {
            system_prompt: "あなたはアシスタントです。".into(),
            model: Some("m".into()),
            lease_secs: 120,
            max_steps: 4,
            ..Default::default()
        },
    )
    .spawn(4);

    let c = ctx(&tenant);
    let thread = store
        .create_thread(&c, "t", false, None, None)
        .await
        .unwrap();
    let post = |text: &'static str| {
        let store = store.clone();
        let c = c.clone();
        let thread_id = thread.id;
        async move {
            store
                .post_message(
                    &c,
                    thread_id,
                    text,
                    &[],
                    None,
                    Some(false),
                    false,
                    &[],
                    None,
                )
                .await
                .unwrap()
        }
    };
    // 1 本目は `slow:` で数秒かかる生成にし、その最中に 2・3 本目を積む
    // （速い stub のままだと 3 本が瞬時に流れ、順番待ちの経路を通らないことがある）。
    let first = post("slow:3 first").await;
    let second = post("second").await;
    let third = post("third").await;

    // 全部終わるまで待つ。**この間ずっと同時実行は 1 本まで**（直列化の本体）。
    // 「先行が走っている間、後続が queued のまま」という瞬間の観測は標本抽出になるため、
    // 決定的な検証は `earlier_unfinished_run_blocks_later_ones_until_it_ends` が持つ。
    // 待ちは**計装ビルド基準**で取る。素のビルドなら 1 秒で終わるが、Coverage ジョブ
    // （cargo-llvm-cov）では同じ 3 本が桁で遅くなり、60 秒だと `slow:3` の 1 本目を待つ間に
    // 尽きて落ちる（実測でそうなった）。ここが見ているのは順序と直列性であって速度ではない。
    let mut completed = false;
    for _ in 0..1_800 {
        let running: i64 = sqlx::query_scalar(
            "SELECT count(*) FROM generation_run WHERE thread_id = $1 AND status = 'running'",
        )
        .bind(thread.id)
        .fetch_one(&pool)
        .await
        .unwrap();
        assert!(running <= 1, "同一スレッドで同時に走る run は 1 本まで");
        let done: i64 = sqlx::query_scalar(
            "SELECT count(*) FROM generation_run WHERE thread_id = $1 AND status = 'done'",
        )
        .bind(thread.id)
        .fetch_one(&pool)
        .await
        .unwrap();
        if done == 3 {
            completed = true;
            break;
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    assert!(completed, "積んだ 3 本すべてが生成されること");

    // 3 本とも完走し、発話順どおりに終わっている。
    let order: Vec<uuid::Uuid> = sqlx::query_scalar(
        "SELECT run_id FROM generation_run WHERE thread_id = $1 AND status = 'done' \
         ORDER BY updated_at, run_id",
    )
    .bind(thread.id)
    .fetch_all(&pool)
    .await
    .unwrap();
    assert_eq!(
        order,
        vec![first.run_id, second.run_id, third.run_id],
        "発話順に生成されること"
    );

    // 後続の履歴には先行の応答が含まれる（＝直列化が履歴として効いている）。
    let msgs = store.get_messages(&c, thread.id, None).await.unwrap();
    let asst_bodies: Vec<String> = msgs
        .iter()
        .filter(|m| m.role == chat::Role::Assistant)
        .map(|m| {
            m.content
                .iter()
                .filter_map(|b| match b {
                    ContentBlock::Text { text } => Some(text.clone()),
                    _ => None,
                })
                .collect()
        })
        .collect();
    assert_eq!(asst_bodies.len(), 3, "3 本とも確定していること");
    assert!(
        asst_bodies.iter().all(|t| !t.is_empty()),
        "空の応答が残らないこと: {asst_bodies:?}"
    );
}

/// 直列化の述語そのものを決定的に検証する。
///
/// 上の完走テストは stub が速すぎて「実際に待たされた」瞬間を捉えられないことがあるため、
/// 順番待ちの判定と**待ちが解けること**をここで直接押さえる。run 行は SQL で直接作る:
/// `post_message` は jobq へも載せるので、同じバイナリの他テストのワーカー（共有レーン）が
/// 拾ってしまい状態が動く。
#[tokio::test]
async fn earlier_unfinished_run_blocks_later_ones_until_it_ends() {
    let Some(pool) = setup().await else { return };
    let tenant = format!("t-{}", uuid::Uuid::new_v4());
    let store = ChatStore::connect(pool.clone(), Arc::new(AllowAll), None)
        .await
        .unwrap();
    let c = ctx(&tenant);
    let thread = store
        .create_thread(&c, "t", false, None, None)
        .await
        .unwrap();

    /// `queued` な run を 1 本作る（jobq へは載せない）。
    async fn seed_run(pool: &PgPool, tenant: &str, thread_id: uuid::Uuid) -> uuid::Uuid {
        let msg: uuid::Uuid = sqlx::query_scalar(
            "INSERT INTO message (thread_id, org, tenant_id, role, content) \
             VALUES ($1, 'o', $2, 'assistant', '[]'::jsonb) RETURNING id",
        )
        .bind(thread_id)
        .bind(tenant)
        .fetch_one(pool)
        .await
        .unwrap();
        sqlx::query_scalar(
            "INSERT INTO generation_run (message_id, thread_id, org, tenant_id, actor, status) \
             VALUES ($1, $2, 'o', $3, 'u', 'queued') RETURNING run_id",
        )
        .bind(msg)
        .bind(thread_id)
        .bind(tenant)
        .fetch_one(pool)
        .await
        .unwrap()
    }

    let mut runs = Vec::new();
    for _ in 0..3 {
        runs.push(seed_run(&pool, &tenant, thread.id).await);
    }

    assert!(
        !store.blocked_by_earlier_run(runs[0]).await.unwrap(),
        "先頭は誰も待たない"
    );
    for later in &runs[1..] {
        assert!(
            store.blocked_by_earlier_run(*later).await.unwrap(),
            "後続は先行が終わるまで待つ"
        );
    }

    // 先頭が端末化すると 2 本目の待ちだけが解ける（3 本目はまだ 2 本目を待つ）。
    let detail = "agent: llm error: openai 502: {\"workspace\":\"wrk_01\"} https://api.example.com";
    assert!(store
        .force_fail_run(runs[0], "表示用の定型文", detail)
        .await
        .unwrap());
    // 会話へ出る Error イベントには定型文だけが載り、上流の詳細は last_error にだけ残る（#592）。
    let shown: String = sqlx::query_scalar(
        "SELECT payload->>'message' FROM generation_event WHERE run_id = $1 AND type = 'error'",
    )
    .bind(runs[0])
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!(shown, "表示用の定型文");
    let (status, last_error): (String, Option<String>) =
        sqlx::query_as("SELECT status, last_error FROM generation_run WHERE run_id = $1")
            .bind(runs[0])
            .fetch_one(&pool)
            .await
            .unwrap();
    assert_eq!(status, "failed");
    assert_eq!(last_error.as_deref(), Some(detail));
    assert!(!store.blocked_by_earlier_run(runs[1]).await.unwrap());
    assert!(store.blocked_by_earlier_run(runs[2]).await.unwrap());

    // 別スレッドの run は互いに待たない（直列化はスレッド単位）。
    let other = store
        .create_thread(&c, "other", false, None, None)
        .await
        .unwrap();
    let other_run = seed_run(&pool, &tenant, other.id).await;
    assert!(
        !store.blocked_by_earlier_run(other_run).await.unwrap(),
        "スレッドを跨いで待たせない"
    );
}

/// このテスト専用のデータベースを作って migration を当てる。
///
/// 生成ジョブの待ち行列は全テストで共有なので、同じ DB では他のテスト（並列に走る別プロセス）の
/// ワーカーがジョブを拾い、自分の gateway（stub）で生成を成功させてしまう。
async fn isolated_setup(prefix: &str) -> Option<PgPool> {
    let Ok(db_url) = std::env::var("STORAGE_TEST_DATABASE_URL") else {
        eprintln!("STORAGE_TEST_DATABASE_URL 未設定のためスキップ");
        return None;
    };
    let admin = PgPoolOptions::new()
        .max_connections(1)
        .connect(&db_url)
        .await
        .expect("Postgres へ接続できること");
    let name = format!("{prefix}_{}", uuid::Uuid::new_v4().simple());
    sqlx::query(&format!("CREATE DATABASE {name}"))
        .execute(&admin)
        .await
        .expect("テスト用 DB を作れること");
    let mut url = reqwest::Url::parse(&db_url).expect("DB URL");
    url.set_path(&name);
    let pool = PgPoolOptions::new()
        .max_connections(5)
        .connect(url.as_str())
        .await
        .expect("テスト用 DB へ接続できること");
    sqlx::migrate!("../../migrations")
        .run(&pool)
        .await
        .expect("マイグレーション適用");
    Some(pool)
}

/// どの要求にも 400 を返す OpenAI 互換のエンドポイント（プロバイダの拒否を再現する）。
async fn rejecting_provider() -> String {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        while let Ok((mut sock, _)) = listener.accept().await {
            tokio::spawn(async move {
                let mut buf = [0u8; 16384];
                let _ = sock.read(&mut buf).await;
                let body = r#"{"error":{"message":"The model is not supported when using Codex with a ChatGPT account.","type":"invalid_request_error"}}"#;
                let resp = format!(
                    "HTTP/1.1 400 Bad Request\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{body}",
                    body.len()
                );
                let _ = sock.write_all(resp.as_bytes()).await;
            });
        }
    });
    format!("http://{addr}/v1")
}

/// プロバイダが要求を受け付けない誤り（400・設定の誤り）は再試行せず、1 回目で失敗を確定する
/// （#877）。再試行の間は「生成中」に見え続けるため。カタログに無いモデルを指定すると gateway が
/// `LlmError::Config("unknown model")` を返すので、それで拒否を起こす。
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn rejected_generation_fails_once_without_retrying() {
    let Some(pool) = isolated_setup("chat_rejected").await else {
        return;
    };
    let tenant = format!("t-{}", uuid::Uuid::new_v4());
    let store = ChatStore::connect(pool.clone(), Arc::new(AllowAll), None)
        .await
        .unwrap();
    let worker = ChatWorker::new(
        pool.clone(),
        store.clone(),
        test_deps(rejecting_gateway(pool.clone(), rejecting_provider().await)),
        WorkerConfig {
            system_prompt: "あなたはアシスタントです。".into(),
            model: Some("m".into()),
            lease_secs: 120,
            max_steps: 4,
            ..Default::default()
        },
    );
    worker.spawn(1);

    let c = ctx(&tenant);
    let thread = store
        .create_thread(&c, "t", false, None, None)
        .await
        .unwrap();
    let res = store
        .post_message(
            &c,
            thread.id,
            "hi",
            &[],
            None,
            Some(false),
            false,
            &[],
            None,
        )
        .await
        .unwrap();

    // 再試行していれば backoff（数十秒）を挟むので、ここで待つ間に Error は届かない。
    let mut rx = store.event_stream(&c.tenant_id, res.run_id, 0);
    let mut error = None;
    for _ in 0..200 {
        let next = tokio::time::timeout(Duration::from_secs(20), rx.next())
            .await
            .expect("Error イベント待ちがタイムアウト（再試行して確定が遅れている）");
        let Some(ev) = next else { break };
        if let StreamEventKind::Error { message } = ev.event {
            error = Some(message);
            break;
        }
    }
    let message = error.expect("Error イベントを受け取ること");
    assert!(
        message.contains("別のモデル"),
        "次に何をすればよいかが分かる文言: {message}"
    );

    // Error の追記・run の failed 化・DLQ への移送は別々に進むので、Error の受信を同期点に
    // せず、期限付きで両方がそろうまで待つ（再試行していれば backoff の間そろわない）。
    let deadline = tokio::time::Instant::now() + Duration::from_secs(20);
    let (status, attempt, last_error, dead) = loop {
        let (status, attempt, last_error): (String, i32, Option<String>) = sqlx::query_as(
            "SELECT status, attempt, last_error FROM generation_run WHERE run_id = $1",
        )
        .bind(res.run_id)
        .fetch_one(&pool)
        .await
        .unwrap();
        let dead: i64 = sqlx::query_scalar(
            "SELECT count(*) FROM job_queue_dead WHERE tenant_id = $1 AND payload->>'run_id' = $2",
        )
        .bind(&tenant)
        .bind(res.run_id.to_string())
        .fetch_one(&pool)
        .await
        .unwrap();
        if (status == "failed" && dead == 1) || tokio::time::Instant::now() >= deadline {
            break (status, attempt, last_error, dead);
        }
        tokio::time::sleep(Duration::from_millis(200)).await;
    };
    assert_eq!(status, "failed");
    assert_eq!(attempt, 1, "再試行していないこと");
    assert!(
        last_error
            .as_deref()
            .is_some_and(|e| e.contains("llm rejected") && e.contains("not supported")),
        "詳細はサーバ側にだけ残る: {last_error:?}"
    );
    // ジョブは再試行の待ち行列に戻らず DLQ へ移る。
    assert_eq!(dead, 1, "DLQ へ移ること");
}
