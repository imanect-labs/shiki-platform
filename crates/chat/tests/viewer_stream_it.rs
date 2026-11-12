//! 生成中の SSE 再送（`viewer_event_stream`）が、溜まって届いた引用の権限をまとめて確かめる結合テスト（#1152）。
//!
//! 実行中の会話へ戻ると run の最初からのイベントが一度に届く。引用を 1 件ずつ OpenFGA に問うと、
//! その往復のたびに後続が止まり、画面が最後の操作までリプレイのように少しずつ進んだ。
//! 1 往復に遅延のある authz モックで、往復回数・所要時間・見直しの結果（読めない引用は番号だけ）を確かめる。
//!
//! `STORAGE_TEST_DATABASE_URL` が設定されている時のみ実行。

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::print_stderr
)]

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use async_trait::async_trait;
use authz::{
    AuthContext, AuthzClient, AuthzError, CheckTuple, Consistency, FgaObject, ObjectType,
    Principal, ReadTupleKey, Relation, Subject,
};
use chat::{ChatStore, StreamEventKind};
use futures::stream::StreamExt;
use sqlx::{postgres::PgPoolOptions, PgPool};

/// OpenFGA の 1 往復ぶんの遅延（ローカルの OpenFGA v1.8 ＋ Postgres で HigherConsistency の check が
/// 中央値 約 30 ms だった）。
const ROUND_TRIP: Duration = Duration::from_millis(30);

/// 往復を数え、拒否リストのオブジェクト以外を許可する authz モック。
#[derive(Default)]
struct SlowAuthz {
    checks: AtomicUsize,
    batches: AtomicUsize,
    /// 問われたファイルの延べ数（batch_check の問数の合計）。
    batch_items: AtomicUsize,
    deny: Mutex<Vec<String>>,
    /// 問われた整合性（剥奪の即時反映のため HigherConsistency であること）。
    consistencies: Mutex<Vec<Consistency>>,
    /// 次の batch_check を失敗させる回数（OpenFGA の一時的な障害の再現）。
    fail_next: AtomicUsize,
}

impl SlowAuthz {
    fn allowed(&self, o: &FgaObject) -> bool {
        !self.deny.lock().unwrap().iter().any(|d| d == o.as_str())
    }
}

#[async_trait]
impl AuthzClient for SlowAuthz {
    async fn check(
        &self,
        _s: &Subject,
        _r: Relation,
        o: &FgaObject,
        c: Consistency,
    ) -> Result<bool, AuthzError> {
        // スレッドの viewer 認可（require_thread）も check を使うので、ファイル以外は数えない。
        if o.as_str().contains("file:") {
            self.checks.fetch_add(1, Ordering::SeqCst);
            self.consistencies.lock().unwrap().push(c);
            tokio::time::sleep(ROUND_TRIP).await;
        }
        Ok(self.allowed(o))
    }
    async fn batch_check(
        &self,
        items: &[CheckTuple],
        c: Consistency,
    ) -> Result<Vec<bool>, AuthzError> {
        self.batches.fetch_add(1, Ordering::SeqCst);
        self.batch_items.fetch_add(items.len(), Ordering::SeqCst);
        self.consistencies.lock().unwrap().push(c);
        tokio::time::sleep(ROUND_TRIP).await;
        if self
            .fail_next
            .fetch_update(Ordering::SeqCst, Ordering::SeqCst, |n| n.checked_sub(1))
            .is_ok()
        {
            return Err(AuthzError::Unexpected {
                status: 503,
                body: "一時的な障害".into(),
            });
        }
        Ok(items.iter().map(|i| self.allowed(&i.object)).collect())
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
    let Ok(db_url) = shiki_testenv::var("STORAGE_TEST_DATABASE_URL") else {
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

fn ctx(tenant: &str, user: &str) -> AuthContext {
    AuthContext::new(
        Principal {
            kind: authz::PrincipalKind::User,
            id: user.into(),
            email: None,
            groups: vec![],
            roles: vec![],
            tenant_id: Some(tenant.into()),
        },
        "acme".into(),
        authz::TenantId::parse(tenant).unwrap(),
    )
}

async fn next_event(
    s: &mut futures::stream::BoxStream<'static, chat::StreamEvent>,
) -> StreamEventKind {
    tokio::time::timeout(Duration::from_secs(10), s.next())
        .await
        .unwrap()
        .unwrap()
        .event
}

fn cite(cite_id: u32, node: &str) -> StreamEventKind {
    let mut c = chat::Citation::withheld(cite_id);
    c.withheld = false;
    c.node_id = node.into();
    c.snippet = format!("{node} の本文");
    StreamEventKind::Citation(c)
}

/// 再訪（溜まったイベントの送り直し）では、含まれる引用の権限を 1 件ずつではなくまとめて確かめる。
/// 見直しの結果（読めない文書の引用は番号だけ・順序）は 1 件ずつのときと変わらない。
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn replayed_citations_are_checked_in_batches() {
    let Some(pool) = setup().await else { return };
    let tenant = format!("t-{}", uuid::Uuid::new_v4());
    let authz = Arc::new(SlowAuthz::default());
    let store = ChatStore::connect(pool.clone(), Arc::clone(&authz) as _, None)
        .await
        .unwrap();
    let owner = ctx(&tenant, "alice");
    let thread = store
        .create_thread(&owner, "検索の多い会話", true, None, None)
        .await
        .unwrap();
    let res = store
        .post_message(
            &owner,
            thread.id,
            "調べて",
            &[],
            None,
            Some(true),
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

    // 検索 10 回 × 13 件の引用（文書は 40 種類・同じ文書の再引用を含む）と、その合間の本文。
    let nodes: Vec<String> = (0..40).map(|_| uuid::Uuid::new_v4().to_string()).collect();
    let secret = nodes[7].clone();
    let mut events = vec![];
    let mut cite_id = 0;
    for step in 0..10 {
        events.push(StreamEventKind::Thinking {
            text: format!("手順 {step}"),
        });
        for k in 0..13 {
            cite_id += 1;
            events.push(cite(cite_id, &nodes[(step * 13 + k) % nodes.len()]));
        }
        events.push(StreamEventKind::Token {
            text: format!("段落 {step}。"),
        });
    }
    events.push(StreamEventKind::Done {
        message_id: res.assistant_message_id,
    });
    for ev in &events {
        store
            .append_stream_event(res.run_id, claimed.fencing_token, ev)
            .await
            .unwrap();
    }
    authz
        .deny
        .lock()
        .unwrap()
        .push(owner.ns().file(&secret).as_str().to_string());

    // 戻った（seq 0 から送り直し）。
    let started = Instant::now();
    let mut s = store
        .viewer_event_stream(&owner, thread.id, res.run_id, 0, None)
        .await
        .unwrap();
    let mut out = vec![];
    while let Ok(Some(ev)) = tokio::time::timeout(Duration::from_secs(30), s.next()).await {
        let done = matches!(ev.event, StreamEventKind::Done { .. });
        out.push(ev.event);
        if done {
            break;
        }
    }
    let elapsed = started.elapsed();
    let checks = authz.checks.load(Ordering::SeqCst);
    let batches = authz.batches.load(Ordering::SeqCst);
    eprintln!(
        "引用 {cite_id} 件を含む {} 件の送り直し: {elapsed:?}（check {checks} 回・batch_check {batches} 回・延べ {} 問）",
        events.len(),
        authz.batch_items.load(Ordering::SeqCst),
    );

    // 1 件ずつの往復（引用ごとに 30 ms・計 130 回）ではなく、数回にまとまる。
    assert_eq!(checks, 0, "引用を 1 件ずつ問わない");
    assert!(
        (1..=5).contains(&batches),
        "溜まった分はまとめて問う: {batches} 回"
    );
    assert!(
        elapsed < ROUND_TRIP * 20,
        "往復を待ち続けない（1 件ずつなら {:?} 以上）: {elapsed:?}",
        ROUND_TRIP * cite_id
    );
    // 剥奪の即時反映のため、まとめても HigherConsistency で問う。
    assert!(authz
        .consistencies
        .lock()
        .unwrap()
        .iter()
        .all(|c| matches!(c, Consistency::HigherConsistency)));

    // 見直しの結果は 1 件ずつのときと同じ: 全件・同じ順に届き、読めない文書の引用だけが番号だけになる。
    assert_eq!(out.len(), events.len());
    for (got, sent) in out.iter().zip(&events) {
        match (got, sent) {
            (StreamEventKind::Citation(g), StreamEventKind::Citation(s)) => {
                assert_eq!(g.cite_id, s.cite_id, "順序を保つ");
                if s.node_id == secret {
                    assert!(g.withheld && g.node_id.is_empty() && g.snippet.is_empty());
                } else {
                    assert!(!g.withheld && g.node_id == s.node_id);
                }
            }
            (g, s) => assert_eq!(g, s),
        }
    }
}

/// ライブで 1 件ずつ届く引用は、届くたびに確かめ直す（途中で閲覧権限が外れたら、次の引用から番号だけ）。
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn live_citations_are_rechecked_and_revocation_applies() {
    let Some(pool) = setup().await else { return };
    let tenant = format!("t-{}", uuid::Uuid::new_v4());
    let authz = Arc::new(SlowAuthz::default());
    let store = ChatStore::connect(pool.clone(), Arc::clone(&authz) as _, None)
        .await
        .unwrap();
    let owner = ctx(&tenant, "alice");
    let thread = store
        .create_thread(&owner, "生成中の会話", true, None, None)
        .await
        .unwrap();
    let res = store
        .post_message(
            &owner,
            thread.id,
            "調べて",
            &[],
            None,
            Some(true),
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
    let node = uuid::Uuid::new_v4().to_string();

    let mut s = store
        .viewer_event_stream(&owner, thread.id, res.run_id, 0, None)
        .await
        .unwrap();
    // 1 件目: 読める。
    store
        .append_stream_event(res.run_id, claimed.fencing_token, &cite(1, &node))
        .await
        .unwrap();
    match next_event(&mut s).await {
        StreamEventKind::Citation(c) => assert!(!c.withheld && c.node_id == node),
        other => panic!("引用が届くこと: {other:?}"),
    }
    // 閲覧権限が外れた後の 2 件目: 前の許可を使い回さず、番号だけになる。
    authz
        .deny
        .lock()
        .unwrap()
        .push(owner.ns().file(&node).as_str().to_string());
    store
        .append_stream_event(res.run_id, claimed.fencing_token, &cite(2, &node))
        .await
        .unwrap();
    match next_event(&mut s).await {
        StreamEventKind::Citation(c) => assert!(c.withheld && c.node_id.is_empty()),
        other => panic!("引用が届くこと: {other:?}"),
    }
    assert_eq!(
        authz.batches.load(Ordering::SeqCst),
        2,
        "届くたびに確かめる"
    );
}

/// 判定が一時的に失敗した引用はその回だけ伏せ、「読めない」とは覚えない（次の引用は確かめ直して出す）。
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn transient_check_failure_withholds_once_without_caching_denial() {
    let Some(pool) = setup().await else { return };
    let tenant = format!("t-{}", uuid::Uuid::new_v4());
    let authz = Arc::new(SlowAuthz::default());
    let store = ChatStore::connect(pool.clone(), Arc::clone(&authz) as _, None)
        .await
        .unwrap();
    let owner = ctx(&tenant, "alice");
    let thread = store
        .create_thread(&owner, "障害中の会話", true, None, None)
        .await
        .unwrap();
    let res = store
        .post_message(
            &owner,
            thread.id,
            "調べて",
            &[],
            None,
            Some(true),
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
    let node = uuid::Uuid::new_v4().to_string();
    let mut s = store
        .viewer_event_stream(&owner, thread.id, res.run_id, 0, None)
        .await
        .unwrap();

    authz.fail_next.store(1, Ordering::SeqCst);
    store
        .append_stream_event(res.run_id, claimed.fencing_token, &cite(1, &node))
        .await
        .unwrap();
    match next_event(&mut s).await {
        StreamEventKind::Citation(c) => {
            assert!(c.withheld && c.node_id.is_empty(), "判定できない間は伏せる");
        }
        other => panic!("引用が届くこと: {other:?}"),
    }
    store
        .append_stream_event(res.run_id, claimed.fencing_token, &cite(2, &node))
        .await
        .unwrap();
    match next_event(&mut s).await {
        StreamEventKind::Citation(c) => {
            assert!(!c.withheld && c.node_id == node, "失敗を拒否として覚えない");
        }
        other => panic!("引用が届くこと: {other:?}"),
    }
}

/// 文書の多い送り直しでも、BatchCheck の往復を直列に重ねない（50 問ずつの往復を並行させる）。
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn many_documents_are_checked_in_parallel_round_trips() {
    let Some(pool) = setup().await else { return };
    let tenant = format!("t-{}", uuid::Uuid::new_v4());
    let authz = Arc::new(SlowAuthz::default());
    let store = ChatStore::connect(pool.clone(), Arc::clone(&authz) as _, None)
        .await
        .unwrap();
    let owner = ctx(&tenant, "alice");
    let thread = store
        .create_thread(&owner, "文書の多い会話", true, None, None)
        .await
        .unwrap();
    let res = store
        .post_message(
            &owner,
            thread.id,
            "調べて",
            &[],
            None,
            Some(true),
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
    for i in 0..300u32 {
        store
            .append_stream_event(
                res.run_id,
                claimed.fencing_token,
                &cite(i + 1, &uuid::Uuid::new_v4().to_string()),
            )
            .await
            .unwrap();
    }
    store
        .append_stream_event(
            res.run_id,
            claimed.fencing_token,
            &StreamEventKind::Done {
                message_id: res.assistant_message_id,
            },
        )
        .await
        .unwrap();

    let started = Instant::now();
    let mut s = store
        .viewer_event_stream(&owner, thread.id, res.run_id, 0, None)
        .await
        .unwrap();
    let mut n = 0;
    while let Ok(Some(ev)) = tokio::time::timeout(Duration::from_secs(30), s.next()).await {
        n += 1;
        if matches!(ev.event, StreamEventKind::Done { .. }) {
            break;
        }
    }
    let elapsed = started.elapsed();
    eprintln!(
        "文書 300 種の送り直し: {elapsed:?}（batch_check {} 回）",
        authz.batches.load(Ordering::SeqCst)
    );
    assert_eq!(n, 301);
    // 直列なら 6 往復（180 ms 以上）。並行なら 1〜2 往復ぶんで済む。
    assert!(
        elapsed < ROUND_TRIP * 4,
        "往復を直列に重ねない: {elapsed:?}"
    );
}

/// 公開ヘルプは専用イベントとブロックのまま、他の閲覧者への再送・保存後の共有にも残る。
#[tokio::test]
async fn help_citation_survives_replay_persistence_and_shared_viewer() {
    let Some(pool) = setup().await else { return };
    let tenant = format!("t-{}", uuid::Uuid::new_v4());
    let authz = Arc::new(SlowAuthz::default());
    let store = ChatStore::connect(pool, authz.clone(), None).await.unwrap();
    let owner = ctx(&tenant, "alice");
    let viewer = ctx(&tenant, "bob");
    let thread = store
        .create_thread(&owner, "ヘルプ", false, None, None)
        .await
        .unwrap();
    store
        .share_thread(
            &owner,
            thread.id,
            &storage::ShareTarget::User { id: "bob".into() },
            chat::ThreadRole::Viewer,
            None,
        )
        .await
        .unwrap();
    let posted = store
        .post_message(
            &owner,
            thread.id,
            "パスキー",
            &[],
            None,
            None,
            true,
            &[],
            None,
        )
        .await
        .unwrap();
    let run = store
        .claim_run(posted.run_id, "help-test", 30)
        .await
        .unwrap()
        .unwrap();
    let cite = agent_core::help_cite::HelpCitation {
        cite_id: 1,
        corpus_version: "v1".into(),
        chunk_id: "c1".into(),
        title: "パスキー".into(),
        heading_path: vec!["手順".into()],
        snippet: "設定を開く".into(),
        url: "https://shiki.imanect.app/help/a/#step".into(),
    };
    let event = StreamEventKind::HelpCitation(cite.clone());
    store
        .append_stream_event(posted.run_id, run.fencing_token, &event)
        .await
        .unwrap();
    let done = StreamEventKind::Done {
        message_id: posted.assistant_message_id,
    };
    store
        .finalize_run(
            posted.run_id,
            run.fencing_token,
            chat::RunStatus::Done,
            &[
                chat::ContentBlock::Text {
                    text: "手順です。[H1]".into(),
                },
                chat::ContentBlock::HelpCitation(cite.clone()),
            ],
            None,
            Some(&done),
        )
        .await
        .unwrap();
    let mut stream = store
        .viewer_event_stream(&viewer, thread.id, posted.run_id, 0, None)
        .await
        .unwrap();
    assert_eq!(next_event(&mut stream).await, event);
    let messages = store.get_messages(&viewer, thread.id, None).await.unwrap();
    assert!(messages
        .iter()
        .flat_map(|m| &m.content)
        .any(|b| matches!(b, chat::ContentBlock::HelpCitation(c) if c == &cite)));
    assert_eq!(authz.checks.load(Ordering::SeqCst), 0);
}
