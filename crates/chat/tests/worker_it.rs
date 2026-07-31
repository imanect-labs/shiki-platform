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

/// ワーカーの依存束（全 None＝ツール無しの素の生成）。テスト間で共有する。
fn test_deps(gateway: LlmGateway) -> chat::WorkerDeps {
    chat::WorkerDeps {
        gateway,
        search: None,
        sandbox: None,
        artifacts: None,
        web_search: None,
        storage: None,
        ui_validator: None,
        skill_artifacts: None,
        skill_catalog: None,
        workflow_store: None,
        workflow_catalog: None,
        collab: None,
        tabular: None,
        office: None,
        office_live: None,
        office_creator: None,
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
    let mut rx = store.event_stream(res.run_id, 0);
    let mut text = String::new();
    let mut done = false;
    for _ in 0..500 {
        let next = tokio::time::timeout(Duration::from_secs(15), rx.next())
            .await
            .expect("イベント待ちがタイムアウト（ワーカーが生成しない）");
        let Some(ev) = next else { break };
        match ev.event {
            StreamEventKind::Token { text: t } => text.push_str(&t),
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

    // 会計が刻まれている（tenant スコープ・冪等キー）。
    let usage_count: i64 =
        sqlx::query_scalar("SELECT count(*) FROM llm_usage WHERE tenant_id = $1")
            .bind(&tenant)
            .fetch_one(&pool)
            .await
            .unwrap();
    assert!(usage_count >= 1, "llm_usage に会計行が刻まれること");
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
    let mut completed = false;
    for _ in 0..600 {
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
    store.force_fail_run(runs[0], "test").await.unwrap();
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
