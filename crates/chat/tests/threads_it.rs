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
    /// 例外的に拒否するオブジェクト（`FgaObject::as_str()`）。閲覧権限の無いファイルの再現用。
    deny: std::sync::Mutex<Vec<String>>,
    /// 特定の主体にだけ拒否する（主体, オブジェクト）。**誰の権限で判定したか**を検証する用。
    deny_for: std::sync::Mutex<Vec<(String, String)>>,
}

impl AllowAll {
    fn new() -> Self {
        Self {
            tuples: std::sync::Mutex::new(vec![]),
            deny: std::sync::Mutex::new(vec![]),
            deny_for: std::sync::Mutex::new(vec![]),
        }
    }
}

#[async_trait]
impl AuthzClient for AllowAll {
    async fn check(
        &self,
        s: &Subject,
        _r: Relation,
        o: &FgaObject,
        _c: Consistency,
    ) -> Result<bool, AuthzError> {
        let subject = s.to_string();
        let denied_for = self
            .deny_for
            .lock()
            .unwrap()
            .iter()
            .any(|(ds, d)| *ds == subject && d == o.as_str());
        Ok(!denied_for && !self.deny.lock().unwrap().iter().any(|d| d == o.as_str()))
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
    let listed = store
        .list_threads(&c, None, None, None, None, 50)
        .await
        .unwrap();
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
        .list_threads(&c, None, None, Some(note_a), None, 50)
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
        .list_threads(&c, None, None, Some(note_b), None, 50)
        .await
        .unwrap();
    assert!(b_only.is_empty());

    // フィルタ無し（全件）は通常スレッドも含む。
    let all = store
        .list_threads(&c, None, None, None, None, 50)
        .await
        .unwrap();
    assert!(all.iter().any(|t| t.id == plain.id));

    // 後付け: 通常スレッドを note_b 由来にする（下書き確定→ノート実体化の紐付け）。
    store
        .set_thread_origin_note(&c, plain.id, note_b, "B", None)
        .await
        .unwrap();
    let b_after = store
        .list_threads(&c, None, None, Some(note_b), None, 50)
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
        .post_message(
            &c,
            thread.id,
            "1つ目の質問",
            &[],
            None,
            Some(false),
            false,
            &[],
            None,
        )
        .await
        .unwrap();
    store
        .post_message(
            &c,
            thread.id,
            "2つ目の質問",
            &[],
            None,
            Some(false),
            false,
            &[],
            None,
        )
        .await
        .unwrap();

    let msgs = store.get_messages(&c, thread.id, None).await.unwrap();
    // 各 post が user+assistant を作るため 4 メッセージ。同一 post 内の user/assistant は
    // `created_at` が同値（`now()`＝TX 開始時刻）になるため、**返信が発話より前に並ばない**
    // ことを厳密に固定する（tie-break が id＝ランダム UUID だと約半数で逆転し、UI 上で
    // アシスタントの返答がユーザ発話より上に描画される）。
    assert_eq!(msgs.len(), 4, "user+assistant を 2 往復");
    let roles: Vec<Role> = msgs.iter().map(|m| m.role).collect();
    assert_eq!(
        roles,
        vec![Role::User, Role::Assistant, Role::User, Role::Assistant],
        "user → assistant の因果順で並ぶ（返信が先に来ない）"
    );
    // 返信は自分の親（直前の user 発話）の後ろに来る。
    for (i, m) in msgs.iter().enumerate() {
        if let Some(parent) = m.parent_id {
            let parent_pos = msgs
                .iter()
                .position(|p| p.id == parent)
                .expect("親メッセージが同一スレッドに存在する");
            assert!(parent_pos < i, "親は返信より前に並ぶ（{parent_pos} < {i}）");
        }
    }

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
    let page1 = store
        .list_threads(&c, None, None, None, None, 2)
        .await
        .unwrap();
    assert_eq!(page1.len(), 2, "1 ページ目は 2 件");

    // カーソルで 2 ページ目。
    let last = page1.last().unwrap();
    let page2 = store
        .list_threads(&c, Some(last.updated_at), Some(last.id), None, None, 2)
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

/// list_threads の title_query がタイトルの部分一致（大小無視）で絞り、ワイルドカードを文字として扱う
/// （検索パレットが先頭 30 件以外も探せる・#591）。カーソルとも併用できる。
#[tokio::test]
async fn list_threads_filters_by_title_query() {
    let Some(pool) = setup().await else { return };
    let tenant = format!("t-{}", uuid::Uuid::new_v4());
    let store = store(&pool, Arc::new(AllowAll::new())).await;
    let c = ctx(&tenant);

    for title in [
        "予算の相談 Alpha",
        "予算レビュー alpha",
        "議事録",
        "100% 達成",
        "1000 達成",
    ] {
        store
            .create_thread(&c, title, false, None, None)
            .await
            .unwrap();
    }
    let titles = |ts: &[chat::Thread]| {
        let mut v: Vec<String> = ts.iter().map(|t| t.title.clone()).collect();
        v.sort();
        v
    };

    let hit = store
        .list_threads(&c, None, None, None, Some("ALPHA"), 50)
        .await
        .unwrap();
    assert_eq!(
        titles(&hit),
        vec!["予算の相談 Alpha", "予算レビュー alpha"],
        "大小を無視して部分一致する"
    );

    let pct = store
        .list_threads(&c, None, None, None, Some("0%"), 50)
        .await
        .unwrap();
    assert_eq!(
        titles(&pct),
        vec!["100% 達成"],
        "% はワイルドカードにならない"
    );

    let blank = store
        .list_threads(&c, None, None, None, Some("  "), 50)
        .await
        .unwrap();
    assert_eq!(blank.len(), 5, "空白のみは絞り込まない");

    // 検索語とカーソルの併用で、残りのヒットだけが続く。
    let p1 = store
        .list_threads(&c, None, None, None, Some("予算"), 1)
        .await
        .unwrap();
    let last = p1.last().unwrap();
    let p2 = store
        .list_threads(
            &c,
            Some(last.updated_at),
            Some(last.id),
            None,
            Some("予算"),
            1,
        )
        .await
        .unwrap();
    assert_eq!(p2.len(), 1);
    assert_ne!(p1[0].id, p2[0].id, "ページ間で重複しない");
    assert!(p2[0].title.contains("予算"));
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
    let mut rx = store.event_stream(&c.tenant_id, run_id, 0);
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
        .post_message(
            &c,
            thread.id,
            "hello agent",
            &[],
            None,
            None,
            false,
            &[],
            None,
        )
        .await
        .unwrap();

    let mut rx = store.event_stream(&c.tenant_id, res.run_id, 0);
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
        .post_message(
            &c,
            thread.id,
            "do danger",
            &[],
            None,
            None,
            false,
            &[],
            None,
        )
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
        if store.run_status(&c.tenant_id, run_id).await.unwrap() == Some(RunStatus::WaitingApproval)
        {
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
        store.run_status(&c.tenant_id, run_id).await.unwrap(),
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
        .post_message(
            &c,
            thread.id,
            "do danger",
            &[],
            None,
            None,
            false,
            &[],
            None,
        )
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

/// ツール結果の観測本文は**実行主体本人にだけ**返す（#386 のレビュー指摘・P1）。
///
/// 引用（citation）は node_id で個別に再認可できるが、tool_result は node に紐づかず
/// 再認可できない。共有スレッドの閲覧者（元ファイルの閲覧権限なし）に doc_search の
/// スニペットや document.read の本文が渡らないよう、本人以外には本文を落とす。
#[tokio::test]
async fn tool_result_bodies_are_hidden_from_other_viewers() {
    let Some(pool) = setup().await else { return };
    let tenant = format!("t-{}", uuid::Uuid::new_v4());
    let store = store(&pool, Arc::new(AllowAll::new())).await;
    let owner = ctx(&tenant);

    let thread = store
        .create_thread(&owner, "共有スレッド", false, None, None)
        .await
        .unwrap();
    let posted = store
        .post_message(
            &owner,
            thread.id,
            "社内文書を調べて",
            &[],
            None,
            None,
            false,
            &[],
            None,
        )
        .await
        .unwrap();

    // ワーカーが確定させる content を模して、ツール呼び出しと結果を直接書き込む。
    let content = vec![
        ContentBlock::ToolCall {
            id: "t1".into(),
            name: "doc_search".into(),
            input: serde_json::json!({ "query": "就業規則" }),
            step: Some(0),
        },
        ContentBlock::ToolResult {
            tool_call_id: "t1".into(),
            content: "社外秘: 給与テーブルの抜粋".into(),
            ok: true,
        },
    ];
    sqlx::query("UPDATE message SET content = $2 WHERE id = $1")
        .bind(posted.assistant_message_id)
        .bind(sqlx::types::Json(&content))
        .execute(&pool)
        .await
        .unwrap();

    // 本人（run の actor）は本文を見られる。
    let mine = store.get_messages(&owner, thread.id, None).await.unwrap();
    assert!(
        mine.iter().any(|m| m.content.iter().any(|b| matches!(
            b,
            ContentBlock::ToolResult { content, .. } if content.contains("社外秘")
        ))),
        "実行主体本人には観測本文が見える"
    );

    // 別ユーザー（共有された閲覧者）には本文を返さない。ブロック自体と成否は残す。
    let mut other = ctx(&tenant);
    other.principal.id = "bob".into();
    let theirs = store.get_messages(&other, thread.id, None).await.unwrap();
    let results: Vec<&ContentBlock> = theirs
        .iter()
        .flat_map(|m| m.content.iter())
        .filter(|b| matches!(b, ContentBlock::ToolResult { .. }))
        .collect();
    assert_eq!(results.len(), 1, "ブロック自体は残す（成否は見せる）");
    assert!(
        matches!(results[0], ContentBlock::ToolResult { content, .. } if content.is_empty()),
        "他人には観測本文を返さない: {results:?}"
    );
}

/// Web の出典（`node_id` を持たない・#891）。
fn web_cite(cite_id: u32) -> chat::Citation {
    let page =
        agent_core::Citation::web("https://example.com/a", "例", "公開ページの抜粋").unwrap();
    let mut c = chat::Citation::from(&page);
    c.cite_id = cite_id;
    c
}

/// 閲覧者が読めない文書の引用は、番号だけの「閲覧できない出典」に置き換わる（#508）。
///
/// 落とすと本文の `[n]` が欠番として描けない（読めないのが最大番号だと、残りの引用からは
/// 欠番だと分からない）。ファイル・本文・位置情報は一切残さない。
#[tokio::test]
async fn unreadable_citations_become_number_only_stubs() {
    let Some(pool) = setup().await else { return };
    let tenant = format!("t-{}", uuid::Uuid::new_v4());
    let authz = Arc::new(AllowAll::new());
    let store = store(&pool, Arc::clone(&authz) as _).await;
    let owner = ctx(&tenant);
    let thread = store
        .create_thread(&owner, "共有スレッド", false, None, None)
        .await
        .unwrap();
    let posted = store
        .post_message(
            &owner,
            thread.id,
            "調べて",
            &[],
            None,
            None,
            false,
            &[],
            None,
        )
        .await
        .unwrap();

    let cite = |cite_id: u32, node: &str| {
        let mut c = chat::Citation::withheld(cite_id);
        c.withheld = false;
        c.node_id = node.into();
        c.chunk_id = format!("chunk-{cite_id}");
        c.snippet = format!("{node} の本文");
        c.version = Some(1);
        ContentBlock::Citation(c)
    };
    let readable = uuid::Uuid::new_v4().to_string();
    let secret = uuid::Uuid::new_v4().to_string();
    let content = vec![
        ContentBlock::Text {
            text: "条件です[1]。期限です[2]。報道です[3]。".into(),
        },
        cite(1, &readable),
        cite(2, &secret),
        // Web の出典（公開 web のページ・#891）。ファイルの権限で見直さず、そのまま見せる。
        ContentBlock::Citation(web_cite(3)),
    ];
    sqlx::query("UPDATE message SET content = $2 WHERE id = $1")
        .bind(posted.assistant_message_id)
        .bind(sqlx::types::Json(&content))
        .execute(&pool)
        .await
        .unwrap();

    let mut viewer = ctx(&tenant);
    viewer.principal.id = "bob".into();
    authz
        .deny
        .lock()
        .unwrap()
        .push(viewer.ns().file(&secret).as_str().to_string());
    // Web の出典をファイル（node_id は空）として判定してしまうと、ここで拒否されて番号だけになる。
    authz
        .deny
        .lock()
        .unwrap()
        .push(viewer.ns().file("").as_str().to_string());
    let theirs = store.get_messages(&viewer, thread.id, None).await.unwrap();
    let cites: Vec<&chat::Citation> = theirs
        .iter()
        .flat_map(|m| m.content.iter())
        .filter_map(|b| match b {
            ContentBlock::Citation(c) => Some(c),
            _ => None,
        })
        .collect();
    assert_eq!(cites.len(), 3, "番号は詰めない");
    assert!(!cites[0].withheld && cites[0].node_id == readable);
    let stub = cites[1];
    assert!(stub.withheld && stub.cite_id == 2);
    assert!(stub.node_id.is_empty() && stub.snippet.is_empty() && stub.version.is_none());
    let web = cites[2];
    assert!(
        !web.withheld && web.cite_id == 3,
        "Web の出典はそのまま: {web:?}"
    );
    assert_eq!(web.web.as_ref().unwrap().url, "https://example.com/a");
    assert_eq!(web.snippet, "公開ページの抜粋");
}

/// 生成中の SSE 再送も、保存済みメッセージと同じく閲覧者の権限で見直す（#513）。
///
/// 引用は読めないファイルのものを番号だけの出典に、ツール結果は実行主体以外には本文を落とす。
/// 実行主体本人には何も落とさない（読めるファイルの引用・自分のツール結果はそのまま）。
#[tokio::test]
async fn viewer_event_stream_redacts_for_other_viewers() {
    let Some(pool) = setup().await else { return };
    let tenant = format!("t-{}", uuid::Uuid::new_v4());
    let authz = Arc::new(AllowAll::new());
    let store = store(&pool, Arc::clone(&authz) as _).await;
    let owner = ctx(&tenant);
    let thread = store
        .create_thread(&owner, "共有スレッド", false, None, None)
        .await
        .unwrap();
    let res = store
        .post_message(
            &owner,
            thread.id,
            "調べて",
            &[],
            None,
            Some(false),
            false,
            &[],
            None,
        )
        .await
        .unwrap();
    let claimed = store
        .claim_run(res.run_id, "w1", 30)
        .await
        .unwrap()
        .unwrap();

    let readable = uuid::Uuid::new_v4().to_string();
    let secret = uuid::Uuid::new_v4().to_string();
    let cite = |cite_id: u32, node: &str| {
        let mut c = chat::Citation::withheld(cite_id);
        c.withheld = false;
        c.node_id = node.into();
        c.snippet = format!("{node} の本文");
        StreamEventKind::Citation(c)
    };
    for ev in [
        StreamEventKind::ToolResult {
            tool_call_id: "t1".into(),
            ok: true,
            content: "社外秘: 給与テーブルの抜粋".into(),
        },
        cite(1, &readable),
        cite(2, &secret),
        // 同じファイルの 2 つ目の引用（判定の使い回しでも結果が変わらないこと）。
        cite(3, &secret),
        // Web の出典（#891）。ファイルの権限で見直さず、そのまま流す。
        StreamEventKind::Citation(web_cite(4)),
        StreamEventKind::Done {
            message_id: res.assistant_message_id,
        },
    ] {
        store
            .append_stream_event(res.run_id, claimed.fencing_token, &ev)
            .await
            .unwrap();
    }

    let mut viewer = ctx(&tenant);
    viewer.principal.id = "bob".into();
    // **閲覧者（bob）にだけ**拒否する。実装が誤って実行主体（alice）の権限で判定すると、
    // bob にも抜粋が流れてこのテストが落ちる。
    // Web の出典をファイル（node_id は空）として判定してしまうと、ここで拒否されて番号だけになる。
    for node in [secret.as_str(), ""] {
        authz.deny_for.lock().unwrap().push((
            viewer.subject().to_string(),
            viewer.ns().file(node).as_str().to_string(),
        ));
    }

    let collect = |ctx: AuthContext| {
        let store = store.clone();
        async move {
            let mut s = store
                .viewer_event_stream(&ctx, thread.id, res.run_id, 0, None)
                .await
                .unwrap();
            let mut out = vec![];
            while let Ok(Some(ev)) = tokio::time::timeout(Duration::from_secs(10), s.next()).await {
                let done = matches!(ev.event, StreamEventKind::Done { .. });
                out.push(ev.event);
                if done {
                    break;
                }
            }
            out
        }
    };

    // 閲覧者（bob）: 読めない引用は番号だけ・ツール結果は本文なし。
    let theirs = collect(viewer.clone()).await;
    assert!(
        matches!(theirs.last(), Some(StreamEventKind::Done { .. })),
        "Done まで届く"
    );
    assert!(
        theirs.iter().any(|e| matches!(
            e,
            StreamEventKind::ToolResult { content, ok: true, .. } if content.is_empty()
        )),
        "他人にはツール結果の本文を流さない（成否は残す）: {theirs:?}"
    );
    let cites: Vec<&chat::Citation> = theirs
        .iter()
        .filter_map(|e| match e {
            StreamEventKind::Citation(c) => Some(c),
            _ => None,
        })
        .collect();
    assert_eq!(cites.len(), 4, "番号は詰めない");
    assert!(!cites[0].withheld && cites[0].node_id == readable);
    for stub in &cites[1..3] {
        assert!(stub.withheld && stub.node_id.is_empty() && stub.snippet.is_empty());
    }
    assert!(
        !cites[3].withheld && cites[3].web.is_some() && cites[3].snippet == "公開ページの抜粋",
        "Web の出典はそのまま: {:?}",
        cites[3]
    );

    // 実行主体（alice）: 自分のツール結果は見える。読める引用はそのまま（bob への拒否は効かない）。
    let mine = collect(owner.clone()).await;
    assert!(mine.iter().any(|e| matches!(
        e,
        StreamEventKind::ToolResult { content, .. } if content.contains("社外秘")
    )));
    assert!(mine
        .iter()
        .filter_map(|e| match e {
            StreamEventKind::Citation(c) => Some(c),
            _ => None,
        })
        .all(|c| !c.withheld));
}

/// SSE の見直しは run がそのスレッドのものかを確かめ、番号の無い旧形式の読めない引用は落とす（#513）。
#[tokio::test]
async fn viewer_event_stream_checks_thread_and_drops_legacy_citations() {
    let Some(pool) = setup().await else { return };
    let tenant = format!("t-{}", uuid::Uuid::new_v4());
    let authz = Arc::new(AllowAll::new());
    let store = store(&pool, Arc::clone(&authz) as _).await;
    let owner = ctx(&tenant);
    let thread = store
        .create_thread(&owner, "共有スレッド", false, None, None)
        .await
        .unwrap();
    let other_thread = store
        .create_thread(&owner, "別スレッド", false, None, None)
        .await
        .unwrap();
    let res = store
        .post_message(
            &owner,
            thread.id,
            "調べて",
            &[],
            None,
            Some(false),
            false,
            &[],
            None,
        )
        .await
        .unwrap();
    let claimed = store
        .claim_run(res.run_id, "w1", 30)
        .await
        .unwrap()
        .unwrap();
    let secret = uuid::Uuid::new_v4().to_string();
    let mut legacy = chat::Citation::withheld(0);
    legacy.withheld = false;
    legacy.node_id = secret.clone();
    legacy.snippet = "旧形式の抜粋".into();
    for ev in [
        StreamEventKind::Citation(legacy),
        StreamEventKind::Done {
            message_id: res.assistant_message_id,
        },
    ] {
        store
            .append_stream_event(res.run_id, claimed.fencing_token, &ev)
            .await
            .unwrap();
    }

    // run が属さないスレッドを名乗っても読めない（同じテナントの他スレッドの run を覗かせない）。
    let err = store
        .viewer_event_stream(&owner, other_thread.id, res.run_id, 0, None)
        .await
        .err()
        .expect("別スレッドの run は読めない");
    assert!(matches!(err, chat::ChatError::NotFound), "{err:?}");

    // 番号の無い旧形式の引用は、読めなければ番号だけの出典にもできないので落とす。
    let mut viewer = ctx(&tenant);
    viewer.principal.id = "bob".into();
    authz
        .deny
        .lock()
        .unwrap()
        .push(viewer.ns().file(&secret).as_str().to_string());
    let mut s = store
        .viewer_event_stream(&viewer, thread.id, res.run_id, 0, None)
        .await
        .unwrap();
    let mut events = vec![];
    while let Ok(Some(ev)) = tokio::time::timeout(Duration::from_secs(10), s.next()).await {
        let done = matches!(ev.event, StreamEventKind::Done { .. });
        events.push(ev.event);
        if done {
            break;
        }
    }
    assert!(
        !events
            .iter()
            .any(|e| matches!(e, StreamEventKind::Citation(_))),
        "読めない旧形式の引用は流さない: {events:?}"
    );
    assert!(matches!(events.last(), Some(StreamEventKind::Done { .. })));
}

/// 検証 C-P1-3: 承認待ちのツール呼び出しは run に永続化され、承認 API はそれと照合する（別の id への決定は
/// 400 で記録しない・以前は黙って記録され run は進まなかった）。承認待ちの間は生成の枠を返す（未回答の承認が
/// 他の run を止めない）。決定後は枠を取り直してから続ける。
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn pending_approval_is_persisted_and_the_wait_releases_the_slot() {
    use std::sync::atomic::AtomicBool;
    use std::time::Duration as StdDuration;

    use agent_core::Approver;
    use chat::worker::slot::{ParkLimits, RunSlot};

    let Some(pool) = setup().await else { return };
    let tenant = format!("t-{}", uuid::Uuid::new_v4());
    let store = store(&pool, Arc::new(AllowAll::new())).await;
    let c = ctx(&tenant);
    let thread = store
        .create_thread(&c, "承認の枠", true, None, None)
        .await
        .unwrap();
    let res = store
        .post_message(
            &c,
            thread.id,
            "do danger",
            &[],
            None,
            None,
            false,
            &[],
            None,
        )
        .await
        .unwrap();
    let run_id = res.run_id;
    let claimed = store.claim_run(run_id, "w1", 30).await.unwrap().unwrap();

    // 枠 1 本のワーカーを模す。この run が枠を持って承認待ちに入る。
    let sem = Arc::new(tokio::sync::Semaphore::new(1));
    let permit = Arc::clone(&sem).acquire_owned().await.unwrap();
    let slot = RunSlot::new(Arc::clone(&sem), ParkLimits::standard(), permit);
    let cancel = Arc::new(AtomicBool::new(false));
    let approver = DbApprover::new(store.clone(), run_id, claimed.fencing_token, cancel)
        .with_timing(StdDuration::from_millis(50), StdDuration::from_secs(20))
        .with_slot(Arc::clone(&slot), &c.tenant_id, &c.principal.id);
    let handle = tokio::spawn(async move {
        approver
            .decide("tc-9", "data_record_write", &serde_json::json!({}))
            .await
    });
    let mut parked = false;
    for _ in 0..60 {
        tokio::time::sleep(StdDuration::from_millis(50)).await;
        if sem.available_permits() == 1 {
            parked = true;
            break;
        }
    }
    assert!(parked, "承認待ちの間は枠を返す（他の run が生成できる）");
    assert_eq!(
        store.run_status(&c.tenant_id, run_id).await.unwrap(),
        Some(RunStatus::WaitingApproval)
    );
    // 承認待ちのツール呼び出しが run に残る（再読込後も引ける）。
    assert_eq!(
        store.pending_approval(run_id).await.unwrap(),
        Some(("tc-9".to_string(), "data_record_write".to_string()))
    );
    // 別の id への決定は記録しない（400）。
    let r = store
        .submit_approval(
            &c,
            thread.id,
            run_id,
            "stubtool_1",
            "data_record_write",
            true,
            None,
        )
        .await;
    assert!(matches!(r, Err(chat::ChatError::Invalid(_))), "{r:?}");
    // 正しい id に別のツール名を申告した決定も記録しない（400・3 回目の検証 3-B-4）。
    let r = store
        .submit_approval(&c, thread.id, run_id, "tc-9", "shell", true, None)
        .await;
    assert!(matches!(r, Err(chat::ChatError::Invalid(_))), "{r:?}");
    assert_eq!(
        store.pending_approval(run_id).await.unwrap(),
        Some(("tc-9".to_string(), "data_record_write".to_string())),
        "承認待ちのまま"
    );
    // 他の run が枠を使っている間に承認しても、枠が空くまで続きは走らない。
    let other = Arc::clone(&sem).acquire_owned().await.unwrap();
    assert!(store
        .submit_approval(
            &c,
            thread.id,
            run_id,
            "tc-9",
            "data_record_write",
            true,
            None
        )
        .await
        .unwrap());
    tokio::time::sleep(StdDuration::from_millis(300)).await;
    assert!(!handle.is_finished(), "枠が空くまで待つ");
    drop(other);
    let decision = tokio::time::timeout(StdDuration::from_secs(5), handle)
        .await
        .expect("承認後に再開する")
        .unwrap();
    assert_eq!(decision, agent_core::ApprovalDecision::Approved);
    assert_eq!(sem.available_permits(), 0, "再開した run が枠を持つ");
    assert_eq!(store.pending_approval(run_id).await.unwrap(), None);
    assert_eq!(
        store.run_status(&c.tenant_id, run_id).await.unwrap(),
        Some(RunStatus::Running)
    );
}

/// #965: ホームの「今日のこと」に出す承認待ちの一覧。自分が actor の run だけを返す。
/// 他人の run・承認待ちでない run・editor を失った thread は出さない。
#[tokio::test]
async fn list_pending_approvals_returns_only_my_waiting_runs_with_editor_access() {
    let Some(pool) = setup().await else { return };
    let tenant = format!("t-{}", uuid::Uuid::new_v4());
    let authz = Arc::new(AllowAll::new());
    let store = store(&pool, Arc::clone(&authz)).await;
    let alice = ctx(&tenant);
    let mut bob = ctx(&tenant);
    bob.principal.id = "bob".into();

    // 承認待ちの run を作る（ワーカーが書く状態を SQL で再現する: pending_approval → waiting_approval）。
    async fn waiting_run(
        store: &ChatStore,
        pool: &PgPool,
        c: &AuthContext,
        title: &str,
        tool_call_id: &str,
    ) -> (uuid::Uuid, uuid::Uuid) {
        let thread = store
            .create_thread(c, title, true, None, None)
            .await
            .unwrap();
        let res = store
            .post_message(c, thread.id, "do it", &[], None, None, false, &[], None)
            .await
            .unwrap();
        sqlx::query(
            "UPDATE generation_run SET status = 'waiting_approval', \
                 pending_approval = jsonb_build_object('tool_call_id', $2::text, \
                     'tool_name', 'drive.move', 'requested_at', now()) \
             WHERE run_id = $1",
        )
        .bind(res.run_id)
        .bind(tool_call_id)
        .execute(pool)
        .await
        .unwrap();
        (thread.id, res.run_id)
    }

    let (mine, mine_run) = waiting_run(&store, &pool, &alice, "移動の確認", "tc-a").await;
    let (_, _) = waiting_run(&store, &pool, &bob, "bob の承認待ち", "tc-b").await;
    // 承認待ちでない run（生成中のまま）は出さない。
    let idle = store
        .create_thread(&alice, "ふつうの会話", false, None, None)
        .await
        .unwrap();
    store
        .post_message(&alice, idle.id, "hi", &[], None, None, false, &[], None)
        .await
        .unwrap();

    let got = store.list_pending_approvals(&alice, 10).await.unwrap();
    assert_eq!(got.len(), 1, "{got:?}");
    assert_eq!(got[0].thread_id, mine);
    assert_eq!(got[0].run_id, mine_run);
    assert_eq!(got[0].thread_title, "移動の確認");
    assert_eq!(got[0].tool_call_id, "tc-a");
    assert_eq!(got[0].tool_name, "drive.move");
    assert_eq!(got[0].origin_note_id, None);

    // 別 org の同名ユーザーには出さない（tenant ＋ org ＋ actor で絞る）。
    let mut other_org = ctx(&tenant);
    other_org.org = "other".into();
    assert!(store
        .list_pending_approvals(&other_org, 10)
        .await
        .unwrap()
        .is_empty());

    // thread の editor を失ったら出さない（post-filter・共有の剥奪は即時に効く）。
    authz.deny_for.lock().unwrap().push((
        alice.subject().to_string(),
        alice.ns().thread(&mine.to_string()).as_str().to_string(),
    ));
    assert!(store
        .list_pending_approvals(&alice, 10)
        .await
        .unwrap()
        .is_empty());
}

/// R3-5: 承認待ちの保留の上限（ここではユーザーあたり 1 件）を超える承認要求は待たずに却下する。
/// 枠は返さず・承認待ちへ遷移せず・承認待ちの記録も残さず、理由を `failure_recovery`
/// （action = `approval_overloaded`）でストリームに出す。
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn approval_over_the_parking_cap_is_rejected_without_waiting() {
    use std::sync::atomic::AtomicBool;
    use std::time::Duration as StdDuration;

    use agent_core::Approver;
    use chat::worker::slot::{ParkLimits, RunSlot};

    let Some(pool) = setup().await else { return };
    let tenant = format!("t-{}", uuid::Uuid::new_v4());
    let store = store(&pool, Arc::new(AllowAll::new())).await;
    let c = ctx(&tenant);
    let thread = store
        .create_thread(&c, "承認の上限", true, None, None)
        .await
        .unwrap();
    let res = store
        .post_message(
            &c,
            thread.id,
            "do danger",
            &[],
            None,
            None,
            false,
            &[],
            None,
        )
        .await
        .unwrap();
    let run_id = res.run_id;
    let claimed = store.claim_run(run_id, "w1", 30).await.unwrap().unwrap();
    // WorkerSink が承認要求の SSE を出す前に記録する承認待ち（上限で却下したら消す）。
    store
        .set_pending_approval(run_id, claimed.fencing_token, "tc-1", "data_record_write")
        .await
        .unwrap();

    // 同じユーザーの別の run が既に 1 件保留している（ユーザーあたりの上限 1）。
    let sem = Arc::new(tokio::sync::Semaphore::new(2));
    let limits = ParkLimits::new(10, 1);
    let other = RunSlot::new(
        Arc::clone(&sem),
        Arc::clone(&limits),
        Arc::clone(&sem).acquire_owned().await.unwrap(),
    );
    other.park(&c.tenant_id, &c.principal.id).await.unwrap();
    let slot = RunSlot::new(
        Arc::clone(&sem),
        Arc::clone(&limits),
        Arc::clone(&sem).acquire_owned().await.unwrap(),
    );
    let before = sem.available_permits();
    let approver = DbApprover::new(
        store.clone(),
        run_id,
        claimed.fencing_token,
        Arc::new(AtomicBool::new(false)),
    )
    .with_timing(StdDuration::from_millis(50), StdDuration::from_secs(20))
    .with_slot(Arc::clone(&slot), &c.tenant_id, &c.principal.id);
    let decision = tokio::time::timeout(
        StdDuration::from_secs(5),
        approver.decide("tc-1", "data_record_write", &serde_json::json!({})),
    )
    .await
    .expect("待たずに返る");
    assert_eq!(decision, agent_core::ApprovalDecision::Rejected);
    assert_eq!(
        sem.available_permits(),
        before,
        "却下した run は枠を返さない"
    );
    assert_ne!(
        store.run_status(&c.tenant_id, run_id).await.unwrap(),
        Some(RunStatus::WaitingApproval),
        "承認待ちへ遷移しない"
    );
    assert_eq!(store.pending_approval(run_id).await.unwrap(), None);
    let detail: Option<String> = sqlx::query_scalar(
        "SELECT payload->>'detail' FROM generation_event \
         WHERE run_id = $1 AND payload->>'action' = 'approval_overloaded'",
    )
    .bind(run_id)
    .fetch_optional(&pool)
    .await
    .unwrap();
    assert!(
        detail
            .as_deref()
            .is_some_and(|d| d.contains("承認待ちが多すぎる")),
        "{detail:?}"
    );
    // 別のユーザーは同じプロセスでも保留できる（ユーザーごとに数える）。
    let third = RunSlot::new(
        Arc::clone(&sem),
        Arc::clone(&limits),
        Arc::clone(&sem).acquire_owned().await.unwrap(),
    );
    third.park(&c.tenant_id, "someone-else").await.unwrap();
}
