//! `ChatStore` の thread/message CRUD・ReBAC 共有・SSE リプレイ・エージェントモード生成の結合テスト。
//!
//! `STORAGE_TEST_DATABASE_URL` が設定されている時のみ実行（未設定なら early-return skip）。
//! OpenFGA は使わずモック AuthzClient（AllowAll）で置換し、DB 上の thread/message/共有タプル
//! と `generation_event` リプレイ、およびエージェントモードワーカーの実コード経路を検証する。

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
use chat::{
    ChatError, ChatStore, ChatWorker, ContentBlock, DbApprover, Role, RunStatus, StreamEventKind,
    ThreadOrigin, ThreadRole, WorkerConfig,
};
use futures::stream::StreamExt;
use llm_gateway::{
    GatewayConfig, LlmGateway, ModelCatalog, ModelEntry, ProviderConfig, ProviderKind,
};
use sqlx::{postgres::PgPoolOptions, PgPool};
use storage::model::ShareTarget;

/// 全許可のモック authz（DB ロジック/共有タプルの検証に集中するため）。
///
/// `read_tuples` は共有一覧テストで使うため内部 `Vec` を返せるよう状態を保持する。
struct AllowAll {
    /// `list_thread_shares` が読み出すタプル（テストが事前に注入する）。
    tuples: std::sync::Mutex<Vec<ReadTupleKey>>,
}

impl AllowAll {
    fn new() -> Self {
        Self {
            tuples: std::sync::Mutex::new(vec![]),
        }
    }
}

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
        s: &Subject,
        r: Relation,
        o: &FgaObject,
    ) -> Result<bool, AuthzError> {
        // 共有付与を記録し、後続の read_tuples で観測できるようにする。
        self.tuples.lock().unwrap().push(ReadTupleKey {
            user: s.to_string(),
            relation: r.as_str().to_string(),
            object: o.as_str().to_string(),
        });
        Ok(true)
    }
    async fn delete_tuple(
        &self,
        s: &Subject,
        r: Relation,
        _o: &FgaObject,
    ) -> Result<bool, AuthzError> {
        let subject = s.to_string();
        let relation = r.as_str().to_string();
        self.tuples
            .lock()
            .unwrap()
            .retain(|t| !(t.user == subject && t.relation == relation));
        Ok(true)
    }
    async fn read_tuples(
        &self,
        _o: &FgaObject,
        _r: Option<Relation>,
    ) -> Result<Vec<ReadTupleKey>, AuthzError> {
        Ok(self.tuples.lock().unwrap().clone())
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

/// AllowAll を共有できるよう `Arc` で返す（共有タプル観測のため個々のテストで生成）。
async fn store(pool: &PgPool, authz: Arc<AllowAll>) -> ChatStore {
    ChatStore::connect(pool.clone(), authz, None)
        .await
        .expect("chat store")
}

/// stub プロバイダの llm-gateway（決定的応答・会計 0 円）。
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

/// create_thread → list_threads / get_thread で往復し、未知 id は NotFound になる。
#[tokio::test]
async fn create_list_get_thread_roundtrips() {
    let Some(pool) = setup().await else { return };
    let tenant = format!("t-{}", uuid::Uuid::new_v4());
    let store = store(&pool, Arc::new(AllowAll::new())).await;
    let c = ctx(&tenant);

    let created = store
        .create_thread(&c, "設計相談", false, None, None)
        .await
        .unwrap();
    assert_eq!(created.title, "設計相談");

    // list_threads は作成したスレッドを含む。
    let listed = store.list_threads(&c, None, None, None, 50).await.unwrap();
    assert!(
        listed.iter().any(|t| t.id == created.id),
        "作成したスレッドが一覧に含まれること"
    );

    // get_thread は同一スレッドを返す。
    let got = store.get_thread(&c, created.id, None).await.unwrap();
    assert_eq!(got.id, created.id);
    assert_eq!(got.title, "設計相談");

    // 未知 id は NotFound（AllowAll で authz は通るが行が無い）。
    let missing = store.get_thread(&c, uuid::Uuid::new_v4(), None).await;
    assert!(
        matches!(missing, Err(ChatError::NotFound)),
        "未知スレッドは NotFound: {missing:?}"
    );
}

/// ノート由来（origin_note_id）: 作成時付与・list の origin フィルタ・後付け設定を検証する（issue #282）。
#[tokio::test]
async fn thread_origin_note_filter_and_backfill() {
    let Some(pool) = setup().await else { return };
    let tenant = format!("t-{}", uuid::Uuid::new_v4());
    let store = store(&pool, Arc::new(AllowAll::new())).await;
    let c = ctx(&tenant);
    let note_a = uuid::Uuid::new_v4();
    let note_b = uuid::Uuid::new_v4();

    // note_a 由来を 2 本＋通常 1 本。
    let t1 = store
        .create_thread(
            &c,
            "ノート: A",
            false,
            Some(ThreadOrigin {
                note_id: note_a,
                note_name: "A".into(),
            }),
            None,
        )
        .await
        .unwrap();
    let t2 = store
        .create_thread(
            &c,
            "ノート: A (2)",
            false,
            Some(ThreadOrigin {
                note_id: note_a,
                note_name: "A".into(),
            }),
            None,
        )
        .await
        .unwrap();
    let plain = store
        .create_thread(&c, "通常", false, None, None)
        .await
        .unwrap();

    assert_eq!(t1.origin_note_id, Some(note_a));
    assert_eq!(t1.origin_note_name.as_deref(), Some("A"));
    assert_eq!(plain.origin_note_id, None);

    // origin フィルタ = note_a → t1, t2 のみ（通常は出ない）。
    let a_only = store
        .list_threads(&c, None, None, Some(note_a), 50)
        .await
        .unwrap();
    let ids: std::collections::HashSet<_> = a_only.iter().map(|t| t.id).collect();
    assert!(ids.contains(&t1.id) && ids.contains(&t2.id));
    assert!(
        !ids.contains(&plain.id),
        "通常スレッドは note_a 一覧に出ない"
    );

    // note_b はまだ空。
    let b_only = store
        .list_threads(&c, None, None, Some(note_b), 50)
        .await
        .unwrap();
    assert!(b_only.is_empty());

    // フィルタ無し（全件）は通常スレッドも含む。
    let all = store.list_threads(&c, None, None, None, 50).await.unwrap();
    assert!(all.iter().any(|t| t.id == plain.id));

    // 後付け: 通常スレッドを note_b 由来にする（下書き確定→ノート実体化の紐付け）。
    store
        .set_thread_origin_note(&c, plain.id, note_b, "B", None)
        .await
        .unwrap();
    let b_after = store
        .list_threads(&c, None, None, Some(note_b), 50)
        .await
        .unwrap();
    assert!(
        b_after.iter().any(|t| t.id == plain.id),
        "後付けで note_b 一覧に出る"
    );
    let got = store.get_thread(&c, plain.id, None).await.unwrap();
    assert_eq!(got.origin_note_id, Some(note_b));
    assert_eq!(got.origin_note_name.as_deref(), Some("B"));
}

/// 空タイトルは既定名に正規化される。
#[tokio::test]
async fn create_thread_defaults_blank_title() {
    let Some(pool) = setup().await else { return };
    let tenant = format!("t-{}", uuid::Uuid::new_v4());
    let store = store(&pool, Arc::new(AllowAll::new())).await;
    let c = ctx(&tenant);

    let created = store
        .create_thread(&c, "   ", false, None, None)
        .await
        .unwrap();
    assert_eq!(created.title, "新しいチャット");
}

/// ワークスペース場所の保存が両列を往復する（Phase 6 UX・0030）。
#[tokio::test]
async fn set_thread_workspace_roundtrips_folder_and_parent() {
    let Some(pool) = setup().await else { return };
    let tenant = format!("t-{}", uuid::Uuid::new_v4());
    let store = store(&pool, Arc::new(AllowAll::new())).await;
    let c = ctx(&tenant);

    // new_under: parent 列に入り、folder 列は空のまま（初回 run で lazy 作成）。
    let t1 = store
        .create_thread(&c, "親配下", false, None, None)
        .await
        .unwrap();
    let parent = uuid::Uuid::new_v4();
    store
        .set_thread_workspace(t1.id, &c.tenant_id, None, Some(parent))
        .await
        .unwrap();
    assert_eq!(
        store
            .workspace_parent_folder_id(t1.id, &c.tenant_id)
            .await
            .unwrap(),
        Some(parent)
    );
    assert_eq!(
        store
            .workspace_folder_id(t1.id, &c.tenant_id)
            .await
            .unwrap(),
        None
    );

    // existing: folder 列に直接入る（新規作成しない）。
    let t2 = store
        .create_thread(&c, "既存", false, None, None)
        .await
        .unwrap();
    let folder = uuid::Uuid::new_v4();
    store
        .set_thread_workspace(t2.id, &c.tenant_id, Some(folder), None)
        .await
        .unwrap();
    assert_eq!(
        store
            .workspace_folder_id(t2.id, &c.tenant_id)
            .await
            .unwrap(),
        Some(folder)
    );
}

/// post_message 後に get_messages が作成順・正しい role/content で返す。
#[tokio::test]
async fn get_messages_returns_ordered_roles_and_content() {
    let Some(pool) = setup().await else { return };
    let tenant = format!("t-{}", uuid::Uuid::new_v4());
    let store = store(&pool, Arc::new(AllowAll::new())).await;
    let c = ctx(&tenant);

    let thread = store
        .create_thread(&c, "会話", false, None, None)
        .await
        .unwrap();
    store
        .post_message(&c, thread.id, "1つ目の質問", &[], Some(false), false, None)
        .await
        .unwrap();
    store
        .post_message(&c, thread.id, "2つ目の質問", &[], Some(false), false, None)
        .await
        .unwrap();

    let msgs = store.get_messages(&c, thread.id, None).await.unwrap();
    // 各 post が user+assistant を作るため 4 メッセージ（user 2・assistant 2）。
    // 同一 post 内の user/assistant は created_at が同値（TX 時刻）で id タイブレークのため
    // 位置は固定できない。role の内訳と user 本文の並び順で検証する。
    assert_eq!(msgs.len(), 4, "user+assistant を 2 往復");
    assert_eq!(
        msgs.iter().filter(|m| m.role == Role::User).count(),
        2,
        "user メッセージが 2 件"
    );
    assert_eq!(
        msgs.iter().filter(|m| m.role == Role::Assistant).count(),
        2,
        "assistant メッセージが 2 件"
    );

    // user メッセージ本文が投稿順（post ごとに created_at が異なる）に並ぶ。
    let user_texts: Vec<String> = msgs
        .iter()
        .filter(|m| m.role == Role::User)
        .filter_map(|m| {
            m.content.iter().find_map(|b| match b {
                ContentBlock::Text { text } => Some(text.clone()),
                _ => None,
            })
        })
        .collect();
    assert_eq!(user_texts, vec!["1つ目の質問", "2つ目の質問"]);
}

/// share_thread → list_thread_shares に現れ、unshare_thread で消える。
#[tokio::test]
async fn share_list_and_unshare_thread() {
    let Some(pool) = setup().await else { return };
    let tenant = format!("t-{}", uuid::Uuid::new_v4());
    let authz = Arc::new(AllowAll::new());
    let store = store(&pool, authz.clone()).await;
    let c = ctx(&tenant);

    let thread = store
        .create_thread(&c, "共有対象", false, None, None)
        .await
        .unwrap();

    // create_thread が owner タプルを 1 件書いている（共有前の初期状態）。
    let target = ShareTarget::User { id: "bob".into() };
    store
        .share_thread(&c, thread.id, &target, ThreadRole::Viewer, None)
        .await
        .unwrap();

    let shares = store.list_thread_shares(&c, thread.id, None).await.unwrap();
    assert!(
        shares
            .iter()
            .any(|(t, r)| *t == target && *r == ThreadRole::Viewer),
        "共有相手が一覧に現れること: {shares:?}"
    );

    // 解除すると一覧から消える。
    store
        .unshare_thread(&c, thread.id, &target, ThreadRole::Viewer, None)
        .await
        .unwrap();
    let after = store.list_thread_shares(&c, thread.id, None).await.unwrap();
    assert!(
        !after
            .iter()
            .any(|(t, r)| *t == target && *r == ThreadRole::Viewer),
        "解除後は一覧に現れないこと: {after:?}"
    );
}

/// editor 共有は自律ワークスペースフォルダにも editor を伝播し、viewer は伝播しない。unshare で剥奪（(a)）。
#[tokio::test]
async fn share_thread_propagates_workspace_editor() {
    let Some(pool) = setup().await else { return };
    let tenant = format!("t-{}", uuid::Uuid::new_v4());
    let authz = Arc::new(AllowAll::new());
    let store = store(&pool, authz.clone()).await;
    let c = ctx(&tenant);

    let thread = store
        .create_thread(&c, "ws共有", false, None, None)
        .await
        .unwrap();
    // ワークスペースフォルダを紐づける（通常は初回自律 run が作る）。
    let folder_id = uuid::Uuid::new_v4();
    store
        .set_workspace_folder_if_absent(thread.id, &c.tenant_id, folder_id)
        .await
        .unwrap();
    let folder_obj = c.ns().folder(&folder_id.to_string()).as_str().to_string();
    let has_folder_editor = |subject: String| {
        authz
            .tuples
            .lock()
            .unwrap()
            .iter()
            .any(|t| t.user == subject && t.relation == "editor" && t.object == folder_obj)
    };

    let bob = ShareTarget::User { id: "bob".into() };
    let bob_subject = bob.subject(&c.ns()).to_string();
    store
        .share_thread(&c, thread.id, &bob, ThreadRole::Editor, None)
        .await
        .unwrap();
    assert!(
        has_folder_editor(bob_subject.clone()),
        "editor 共有でワークスペースフォルダに editor が付与される"
    );

    // viewer 共有はフォルダへ伝播しない（自律 run を起こせないため）。
    let carol = ShareTarget::User { id: "carol".into() };
    let carol_subject = carol.subject(&c.ns()).to_string();
    store
        .share_thread(&c, thread.id, &carol, ThreadRole::Viewer, None)
        .await
        .unwrap();
    assert!(
        !has_folder_editor(carol_subject),
        "viewer 共有はワークスペースへ伝播しない"
    );

    // unshare でフォルダ editor も剥奪される。
    store
        .unshare_thread(&c, thread.id, &bob, ThreadRole::Editor, None)
        .await
        .unwrap();
    assert!(
        !has_folder_editor(bob_subject),
        "unshare でワークスペースフォルダの editor が剥奪される"
    );
}

/// grant_workspace_to_members は owner と（作成前に共有された）editor へフォルダ editor をバックフィルする（(a)）。
#[tokio::test]
async fn backfill_grants_workspace_to_thread_members() {
    let Some(pool) = setup().await else { return };
    let tenant = format!("t-{}", uuid::Uuid::new_v4());
    let authz = Arc::new(AllowAll::new());
    let store = store(&pool, authz.clone()).await;
    let c = ctx(&tenant);

    let thread = store
        .create_thread(&c, "backfill", false, None, None)
        .await
        .unwrap();
    // ワークスペース作成**前**に editor 共有（この時点ではフォルダが無く伝播されない）。
    let bob = ShareTarget::User { id: "bob".into() };
    let bob_subject = bob.subject(&c.ns()).to_string();
    store
        .share_thread(&c, thread.id, &bob, ThreadRole::Editor, None)
        .await
        .unwrap();

    // 初回自律 run 相当: フォルダを紐づけてからバックフィルする。
    let folder_id = uuid::Uuid::new_v4();
    store
        .set_workspace_folder_if_absent(thread.id, &c.tenant_id, folder_id)
        .await
        .unwrap();
    store
        .grant_workspace_to_members(&c, thread.id)
        .await
        .unwrap();

    let folder_obj = c.ns().folder(&folder_id.to_string()).as_str().to_string();
    let has_folder_editor = |subject: String| {
        authz
            .tuples
            .lock()
            .unwrap()
            .iter()
            .any(|t| t.user == subject && t.relation == "editor" && t.object == folder_obj)
    };
    assert!(
        has_folder_editor(c.subject().to_string()),
        "owner にフォルダ editor がバックフィルされる"
    );
    assert!(
        has_folder_editor(bob_subject),
        "作成前に共有された editor にもバックフィルされる"
    );
}

/// list_threads の keyset ページングが更新日降順で重複なく進む。
#[tokio::test]
async fn list_threads_keyset_pagination() {
    let Some(pool) = setup().await else { return };
    let tenant = format!("t-{}", uuid::Uuid::new_v4());
    let store = store(&pool, Arc::new(AllowAll::new())).await;
    let c = ctx(&tenant);

    // 5 スレッド作成（updated_at がほぼ同時でも id タイブレークで安定順）。
    let mut created = vec![];
    for i in 0..5 {
        let t = store
            .create_thread(&c, &format!("スレッド{i}"), false, None, None)
            .await
            .unwrap();
        created.push(t.id);
    }

    // 1 ページ目（limit=2）。
    let page1 = store.list_threads(&c, None, None, None, 2).await.unwrap();
    assert_eq!(page1.len(), 2, "1 ページ目は 2 件");

    // カーソルで 2 ページ目。
    let last = page1.last().unwrap();
    let page2 = store
        .list_threads(&c, Some(last.updated_at), Some(last.id), None, 2)
        .await
        .unwrap();
    assert_eq!(page2.len(), 2, "2 ページ目は 2 件");

    // ページ間で重複が無い。
    for a in &page1 {
        assert!(
            !page2.iter().any(|b| b.id == a.id),
            "ページ間でスレッドが重複しないこと"
        );
    }
}

/// event_stream(run_id, 0) が追記済みイベントを seq 順にリプレイし、Done で終了する。
#[tokio::test]
async fn event_stream_replays_appended_events() {
    let Some(pool) = setup().await else { return };
    let tenant = format!("t-{}", uuid::Uuid::new_v4());
    let store = store(&pool, Arc::new(AllowAll::new())).await;
    let c = ctx(&tenant);

    let thread = store
        .create_thread(&c, "配信", false, None, None)
        .await
        .unwrap();
    let res = store
        .post_message(&c, thread.id, "hi", &[], Some(false), false, None)
        .await
        .unwrap();
    let run_id = res.run_id;
    let claimed = store.claim_run(run_id, "w1", 30).await.unwrap().unwrap();
    let fencing = claimed.fencing_token;

    for t in ["a", "b"] {
        store
            .append_stream_event(run_id, fencing, &StreamEventKind::Token { text: t.into() })
            .await
            .unwrap();
    }
    // 端末イベントでストリームを終了させる。
    store
        .append_stream_event(
            run_id,
            fencing,
            &StreamEventKind::Done {
                message_id: res.assistant_message_id,
            },
        )
        .await
        .unwrap();

    // from_seq=0 で全リプレイ。Done を受け取ったら終了。
    let mut rx = store.event_stream(run_id, 0);
    let mut tokens = String::new();
    let mut done = false;
    for _ in 0..50 {
        let next = tokio::time::timeout(Duration::from_secs(10), rx.next())
            .await
            .expect("イベント待ちタイムアウト");
        let Some(ev) = next else { break };
        match ev.event {
            StreamEventKind::Token { text } => tokens.push_str(&text),
            StreamEventKind::Done { .. } => {
                done = true;
                break;
            }
            _ => {}
        }
    }
    assert!(done, "Done イベントで終了すること");
    assert_eq!(tokens, "ab", "追記した順にトークンがリプレイされること");
}

/// エージェントモードのスレッドで生成ワーカーを回し、agent-core の run_agent 経路を通す。
///
/// search=None なのでツールは無いが、エージェントループ自体は実行される。
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn agent_mode_worker_runs_to_done() {
    let Some(pool) = setup().await else { return };
    let tenant = format!("t-{}", uuid::Uuid::new_v4());
    let store = store(&pool, Arc::new(AllowAll::new())).await;
    let gateway = stub_gateway(pool.clone());
    let worker = ChatWorker::new(
        pool.clone(),
        store.clone(),
        chat::WorkerDeps {
            gateway,
            search: None,
            sandbox: None,
            artifacts: None,
            web_search: None,
            storage: None,
            ui_validator: None,
            skill_artifacts: None,
            workflow_store: None,
            workflow_catalog: None,
            collab: None,
            tabular: None,
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

    let c = ctx(&tenant);
    // agent_mode=true のスレッド → post_message は既定でエージェントモード。
    let thread = store
        .create_thread(&c, "agent", true, None, None)
        .await
        .unwrap();
    let res = store
        .post_message(&c, thread.id, "hello agent", &[], None, false, None)
        .await
        .unwrap();

    let mut rx = store.event_stream(res.run_id, 0);
    let mut done = false;
    for _ in 0..500 {
        let next = tokio::time::timeout(Duration::from_secs(20), rx.next())
            .await
            .expect("イベント待ちタイムアウト（エージェントワーカーが生成しない）");
        let Some(ev) = next else { break };
        match ev.event {
            StreamEventKind::Done { .. } => {
                done = true;
                break;
            }
            StreamEventKind::Error { message } => panic!("エージェント生成失敗: {message}"),
            _ => {}
        }
    }
    assert!(done, "エージェントモード生成が Done まで完了すること");
}

/// Task 5.6: DbApprover が承認待ちでブロックし、API 決定（submit_approval）で解けることを検証する。
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn db_approver_blocks_until_decision() {
    use std::sync::atomic::AtomicBool;
    use std::time::Duration as StdDuration;

    use agent_core::Approver;

    let Some(pool) = setup().await else { return };
    let tenant = format!("t-{}", uuid::Uuid::new_v4());
    let store = store(&pool, Arc::new(AllowAll::new())).await;
    let c = ctx(&tenant);

    let thread = store
        .create_thread(&c, "承認", true, None, None)
        .await
        .unwrap();
    let res = store
        .post_message(&c, thread.id, "do danger", &[], None, false, None)
        .await
        .unwrap();
    let run_id = res.run_id;
    let claimed = store.claim_run(run_id, "w1", 30).await.unwrap().unwrap();
    let fencing = claimed.fencing_token;

    // 承認者を別タスクで走らせる（短いポーリング間隔・上限）。
    let cancel = Arc::new(AtomicBool::new(false));
    let approver = DbApprover::new(store.clone(), run_id, fencing, cancel.clone())
        .with_timing(StdDuration::from_millis(50), StdDuration::from_secs(10));
    let handle = tokio::spawn(async move {
        approver
            .decide("tc-1", "shell", &serde_json::json!({"cmd": "rm x"}))
            .await
    });

    // 承認待ちへ遷移するのを待つ。
    let mut waited = false;
    for _ in 0..40 {
        tokio::time::sleep(StdDuration::from_millis(50)).await;
        if store.run_status(run_id).await.unwrap() == Some(RunStatus::WaitingApproval) {
            waited = true;
            break;
        }
    }
    assert!(waited, "承認待ち（waiting_approval）へ遷移する");

    // **承認待ち中もハートビートがリースを延長できる**（誤キャンセル防止・Task 5.6 の要）。
    // status='running' 限定だとここで None を返し、ワーカーが誤って停止扱いにしてしまう。
    assert!(
        store
            .heartbeat(run_id, fencing, 30)
            .await
            .unwrap()
            .is_some(),
        "waiting_approval 中もハートビートが成功する（リース失効による誤キャンセルを防ぐ）"
    );

    // API 相当: 承認を投入する。
    let accepted = store
        .submit_approval(&c, thread.id, run_id, "tc-1", "shell", true, None)
        .await
        .unwrap();
    assert!(accepted, "初回決定が採用される");

    // 承認者が Approved を返し、走行状態へ戻る。
    let decision = tokio::time::timeout(StdDuration::from_secs(5), handle)
        .await
        .expect("承認がタイムアウトしない")
        .unwrap();
    assert_eq!(decision, agent_core::ApprovalDecision::Approved);
    assert_eq!(
        store.run_status(run_id).await.unwrap(),
        Some(RunStatus::Running)
    );

    // 二重決定は no-op（先勝ち）。
    let again = store
        .submit_approval(&c, thread.id, run_id, "tc-1", "shell", false, None)
        .await
        .unwrap();
    assert!(!again, "既決の再決定は採用されない");
}

/// Task 5.6: 承認待ち中のキャンセルで DbApprover が Cancelled を返す。
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn db_approver_cancelled_by_flag() {
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::time::Duration as StdDuration;

    use agent_core::Approver;

    let Some(pool) = setup().await else { return };
    let tenant = format!("t-{}", uuid::Uuid::new_v4());
    let store = store(&pool, Arc::new(AllowAll::new())).await;
    let c = ctx(&tenant);

    let thread = store
        .create_thread(&c, "承認cancel", true, None, None)
        .await
        .unwrap();
    let res = store
        .post_message(&c, thread.id, "do danger", &[], None, false, None)
        .await
        .unwrap();
    let run_id = res.run_id;
    let claimed = store.claim_run(run_id, "w1", 30).await.unwrap().unwrap();

    let cancel = Arc::new(AtomicBool::new(false));
    let approver = DbApprover::new(store.clone(), run_id, claimed.fencing_token, cancel.clone())
        .with_timing(StdDuration::from_millis(50), StdDuration::from_secs(10));
    let handle = tokio::spawn(async move {
        approver
            .decide("tc-1", "shell", &serde_json::json!({}))
            .await
    });

    tokio::time::sleep(StdDuration::from_millis(150)).await;
    cancel.store(true, Ordering::Relaxed); // ユーザーが停止

    let decision = tokio::time::timeout(StdDuration::from_secs(5), handle)
        .await
        .expect("キャンセルがタイムアウトしない")
        .unwrap();
    assert_eq!(decision, agent_core::ApprovalDecision::Cancelled);
}
