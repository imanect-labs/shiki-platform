//! 生成中の SSE 再送（`viewer_event_stream`）が、溜まって届いた引用の権限をまとめて確かめる結合テスト（#1152）。
//!
//! 実行中の会話へ戻ると run の最初からのイベントが一度に届く。引用を 1 件ずつ OpenFGA に問うと、
//! その往復のたびに後続が止まり、画面が最後の操作までリプレイのように少しずつ進んだ。
//! 1 往復に遅延のある authz モックで、往復回数・所要時間・見直しの結果（読めない引用は番号だけ）を確かめる。
//!
//! 共有を外された閲覧者の開いたままの配信が、問い直しで 1 件の `Error` を流して閉じることも確かめる
//! （PIT-84・D-214）。run の実行主体の配信は問い直さないこと、共有したままの閲覧者は流れ続けることを対照にする。
//!
//! `STORAGE_TEST_DATABASE_URL` が設定されている時のみ実行（実 OpenFGA の試験は `OPENFGA_TEST_URL` も）。

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::print_stderr
)]

use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use async_trait::async_trait;
use authz::client::{OpenFgaClient, OpenFgaConfig};
use authz::{
    AuthContext, AuthzClient, AuthzError, CheckTuple, Consistency, FgaObject, ObjectType,
    Principal, ReadTupleKey, Relation, Subject,
};
use chat::{ChatStore, StreamEventKind, ThreadRole};
use futures::stream::StreamExt;
use sqlx::{postgres::PgPoolOptions, PgPool};
use storage::model::ShareTarget;

/// OpenFGA の 1 往復ぶんの遅延（ローカルの OpenFGA v1.8 ＋ Postgres で HigherConsistency の check が
/// 中央値 約 30 ms だった）。
const ROUND_TRIP: Duration = Duration::from_millis(30);

/// 往復を数え、拒否リストのオブジェクト以外を許可する authz モック。
#[derive(Default)]
struct SlowAuthz {
    checks: AtomicUsize,
    /// 会話（thread）への check の回数（開くときの認可と、配信の問い直し）。
    thread_checks: AtomicUsize,
    /// 会話への check を失敗させる（OpenFGA が読めないときの再現）。
    thread_check_fails: AtomicBool,
    batches: AtomicUsize,
    /// 往復中の batch_check の数と、その最大（往復を並行させているかを実時間に頼らず見る）。
    batches_in_flight: AtomicUsize,
    max_batches_in_flight: AtomicUsize,
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
        if o.as_str().starts_with("thread:") {
            self.thread_checks.fetch_add(1, Ordering::SeqCst);
            if self.thread_check_fails.load(Ordering::SeqCst) {
                return Err(AuthzError::Unexpected {
                    status: 503,
                    body: "一時的な障害".into(),
                });
            }
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
        let in_flight = self.batches_in_flight.fetch_add(1, Ordering::SeqCst) + 1;
        self.max_batches_in_flight
            .fetch_max(in_flight, Ordering::SeqCst);
        tokio::time::sleep(ROUND_TRIP).await;
        self.batches_in_flight.fetch_sub(1, Ordering::SeqCst);
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

/// run の実行主体の配信は、開くときの 1 回しか会話の権限を問わない（問い直しは実行主体でない閲覧者だけ）。
fn assert_not_rechecked(authz: &SlowAuthz) {
    assert_eq!(
        authz.thread_checks.load(Ordering::SeqCst),
        1,
        "実行主体の配信は問い直さない"
    );
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
        stop_reason: None,
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
    authz.thread_checks.store(0, Ordering::SeqCst);
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
    assert_not_rechecked(&authz);
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

    authz.thread_checks.store(0, Ordering::SeqCst);
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
    assert_not_rechecked(&authz);
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
    authz.thread_checks.store(0, Ordering::SeqCst);
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
    assert_not_rechecked(&authz);
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
                stop_reason: None,
            },
        )
        .await
        .unwrap();

    let started = Instant::now();
    authz.thread_checks.store(0, Ordering::SeqCst);
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
    let max_in_flight = authz.max_batches_in_flight.load(Ordering::SeqCst);
    eprintln!(
        "文書 300 種の送り直し: {elapsed:?}（batch_check {} 回・同時に最大 {max_in_flight} 往復）",
        authz.batches.load(Ordering::SeqCst)
    );
    assert_eq!(n, 301);
    // 直列なら往復は同時に 1 つまで。実時間で見ると、機械が混んだときに DB の読みの遅れで揺れる。
    assert!(
        max_in_flight >= 2,
        "往復を直列に重ねない: 同時に最大 {max_in_flight} 往復"
    );
    assert_not_rechecked(&authz);
}

// ── 共有を外された閲覧者の配信の打ち切り（PIT-84・D-214） ──────────────────────────────

/// 「間隔の中では問い直さない」を見るときの問い直しの間隔（試験の間に過ぎない長さ）。
const LONG_RECHECK: Duration = Duration::from_hours(1);
/// 打ち切りのときに流れる固定の文（`store/message_view.rs` の `VIEWER_REVOKED_MESSAGE`）。
const REVOKED_MESSAGE: &str = "表示を続けられなくなりました。ページを読み込み直してください。";

/// alice の会話と、ワーカーが着手した生成（run）。
struct LiveRun {
    thread_id: uuid::Uuid,
    run_id: uuid::Uuid,
    fencing: i64,
    message_id: uuid::Uuid,
}

async fn live_run(store: &ChatStore, owner: &AuthContext) -> LiveRun {
    let thread = store
        .create_thread(owner, "共有した会話", false, None, None)
        .await
        .unwrap();
    let res = store
        .post_message(
            owner,
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
    LiveRun {
        thread_id: thread.id,
        run_id: res.run_id,
        fencing: claimed.fencing_token,
        message_id: res.assistant_message_id,
    }
}

async fn append(store: &ChatStore, run: &LiveRun, ev: &StreamEventKind) {
    store
        .append_stream_event(run.run_id, run.fencing, ev)
        .await
        .unwrap();
}

fn token(text: &str) -> StreamEventKind {
    StreamEventKind::Token { text: text.into() }
}

fn done(run: &LiveRun) -> StreamEventKind {
    StreamEventKind::Done {
        message_id: run.message_id,
        stop_reason: None,
    }
}

/// 打ち切りの `Error` が 1 件だけ届き、その後にストリームが閉じる（後続の分は届かない）。
async fn assert_cut(s: &mut futures::stream::BoxStream<'static, chat::StreamEvent>) {
    let ev = tokio::time::timeout(Duration::from_secs(10), s.next())
        .await
        .unwrap()
        .expect("打ち切りの文が届くこと");
    match ev.event {
        StreamEventKind::Error { message } => assert_eq!(message, REVOKED_MESSAGE),
        other => panic!("打ち切りの文の前に届いた: {other:?}"),
    }
    assert_eq!(ev.seq, 0, "run の seq と重ならない");
    assert!(
        tokio::time::timeout(Duration::from_secs(10), s.next())
            .await
            .unwrap()
            .is_none(),
        "打ち切りの文の後は閉じる"
    );
}

/// 会話の閲覧をどう失わせるか。
#[derive(Clone, Copy, PartialEq, Eq)]
enum Revoke {
    /// 共有を外した（check が断る）。
    Deny,
    /// OpenFGA が読めない（check が誤りを返す・fail-closed で打ち切る）。
    Unreadable,
}

async fn revoked_viewer_stream_is_cut(revoke: Revoke) {
    let Some(pool) = setup().await else { return };
    let tenant = format!("t-{}", uuid::Uuid::new_v4());
    let authz = Arc::new(SlowAuthz::default());
    // 届いた分ごとに問い直す（間隔が過ぎたのを、実時間を待たずに作る）。
    let store = ChatStore::connect(pool.clone(), Arc::clone(&authz) as _, None)
        .await
        .unwrap()
        .with_viewer_recheck(Duration::ZERO);
    let owner = ctx(&tenant, "alice");
    let dave = ctx(&tenant, "dave");
    let run = live_run(&store, &owner).await;
    append(&store, &run, &token("共有中の本文")).await;

    authz.thread_checks.store(0, Ordering::SeqCst);
    let trace = format!("trace-{}", uuid::Uuid::new_v4());
    let mut s = store
        .viewer_event_stream(&dave, run.thread_id, run.run_id, 0, Some(&trace))
        .await
        .unwrap();
    // 共有している間は、問い直しても流れる（開くときと、届いた分の前の 1 回）。
    assert_eq!(next_event(&mut s).await, token("共有中の本文"));
    assert_eq!(authz.thread_checks.load(Ordering::SeqCst), 2);

    match revoke {
        Revoke::Deny => authz.deny.lock().unwrap().push(
            dave.ns()
                .thread(&run.thread_id.to_string())
                .as_str()
                .to_string(),
        ),
        Revoke::Unreadable => authz.thread_check_fails.store(true, Ordering::SeqCst),
    }
    for ev in [
        token("外した後の本文 SECRET-AFTER-UNSHARE"),
        StreamEventKind::Thinking {
            text: "外した後の思考 SECRET-AFTER-UNSHARE".into(),
        },
        done(&run),
    ] {
        append(&store, &run, &ev).await;
    }
    assert_cut(&mut s).await;
    assert_eq!(
        authz.thread_checks.load(Ordering::SeqCst),
        3,
        "外した後に届いた分の前に 1 回だけ問い直して閉じる"
    );

    // 断られた問い直しは、開くときと同じく deny の監査を残す（読めなかったときは判定が無いので残さない）。
    let denies: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM audit_log WHERE tenant_id = $1 AND actor = 'dave' \
         AND action = 'thread.stream' AND decision = 'deny' AND trace_id = $2",
    )
    .bind(&tenant)
    .bind(&trace)
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!(denies, i64::from(revoke == Revoke::Deny));
}

/// 共有を外された閲覧者の開いたままの配信は、問い直しで `Error` を 1 件流して閉じ、以後の本文・思考・完了は届かない。
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn revoked_viewer_stream_stops_after_recheck() {
    revoked_viewer_stream_is_cut(Revoke::Deny).await;
}

/// 問い直しで会話の権限を確かめられないときも、流さずに閉じる（fail-closed）。
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn unreadable_recheck_stops_viewer_stream() {
    revoked_viewer_stream_is_cut(Revoke::Unreadable).await;
}

/// run の実行主体の配信は問い直さない（間隔が無くても・会話の check が断る状態にしても、本人の生成は最後まで流れる）。
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn actor_stream_is_not_rechecked() {
    let Some(pool) = setup().await else { return };
    let tenant = format!("t-{}", uuid::Uuid::new_v4());
    let authz = Arc::new(SlowAuthz::default());
    let store = ChatStore::connect(pool.clone(), Arc::clone(&authz) as _, None)
        .await
        .unwrap()
        .with_viewer_recheck(Duration::ZERO);
    let owner = ctx(&tenant, "alice");
    let run = live_run(&store, &owner).await;

    authz.thread_checks.store(0, Ordering::SeqCst);
    let mut s = store
        .viewer_event_stream(&owner, run.thread_id, run.run_id, 0, None)
        .await
        .unwrap();
    authz.deny.lock().unwrap().push(
        owner
            .ns()
            .thread(&run.thread_id.to_string())
            .as_str()
            .to_string(),
    );
    append(&store, &run, &token("本人の本文")).await;
    append(&store, &run, &done(&run)).await;
    assert_eq!(next_event(&mut s).await, token("本人の本文"));
    assert_eq!(next_event(&mut s).await, done(&run));
    assert_not_rechecked(&authz);
}

/// 共有したままの閲覧者には流れ続ける。問い直しは間隔の中では起きず、間隔を過ぎていれば、まとめて届いた分の前に
/// 1 回だけで、1 件ごとには問わない。ツール結果の本文を落とす見直しは、問い直しの後も今までどおり掛かる。
///
/// 1 スレッドで回す（溜まった分の送り直しは、読み手が動く前に全部がチャネルに入るので、1 回にまとめて届く）。
#[tokio::test]
async fn shared_viewer_keeps_streaming_with_one_recheck_per_interval() {
    let Some(pool) = setup().await else { return };
    let tenant = format!("t-{}", uuid::Uuid::new_v4());
    let authz = Arc::new(SlowAuthz::default());
    let store = ChatStore::connect(pool.clone(), Arc::clone(&authz) as _, None)
        .await
        .unwrap();
    let owner = ctx(&tenant, "alice");
    let dave = ctx(&tenant, "dave");
    let run = live_run(&store, &owner).await;
    let events = [
        token("一段落目。"),
        token("二段落目。"),
        StreamEventKind::ToolResult {
            tool_call_id: "t1".into(),
            ok: true,
            content: "alice の権限で読んだ本文".into(),
        },
        done(&run),
    ];
    for ev in &events {
        append(&store, &run, ev).await;
    }
    let expected = vec![
        events[0].clone(),
        events[1].clone(),
        StreamEventKind::ToolResult {
            tool_call_id: "t1".into(),
            ok: true,
            content: String::new(),
        },
        events[3].clone(),
    ];
    // 間隔の中なら開くときの 1 回だけ。間隔が過ぎていれば、まとめて届いた 4 件の前にもう 1 回だけ。
    for (every, checks) in [(LONG_RECHECK, 1), (Duration::ZERO, 2)] {
        authz.thread_checks.store(0, Ordering::SeqCst);
        let viewer = store.clone().with_viewer_recheck(every);
        let mut s = viewer
            .viewer_event_stream(&dave, run.thread_id, run.run_id, 0, None)
            .await
            .unwrap();
        let mut out = vec![];
        while let Ok(Some(ev)) = tokio::time::timeout(Duration::from_secs(10), s.next()).await {
            out.push(ev.event);
        }
        assert_eq!(
            out, expected,
            "共有したままなら最後まで流れる（ツール結果の本文は落とす・{every:?}）"
        );
        assert_eq!(
            authz.thread_checks.load(Ordering::SeqCst),
            checks,
            "問い直しの回数（間隔 {every:?}）"
        );
    }
}

/// 実 OpenFGA で、`unshare_thread` が開いたままの配信を止める（共有したままの carol は流れ続ける）。
/// 外された dave は、読み込み直しても配信を開けない。
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn unshare_thread_stops_the_partners_open_stream() {
    let Some(pool) = setup().await else { return };
    let Ok(fga_url) = shiki_testenv::var("OPENFGA_TEST_URL") else {
        eprintln!("OPENFGA_TEST_URL 未設定のためスキップ");
        return;
    };
    let fga = OpenFgaClient::connect(
        reqwest::Client::new(),
        &OpenFgaConfig {
            base_url: fga_url,
            store_name: format!("shiki-chat-viewer-stream-{}", uuid::Uuid::new_v4()),
        },
        &authz::model::default_model(),
    )
    .await
    .expect("OpenFGA 接続");
    // 届いた分ごとに問い直す（間隔が過ぎたのを、実時間を待たずに作る）。
    let store = ChatStore::connect(pool.clone(), Arc::new(fga), None)
        .await
        .unwrap()
        .with_viewer_recheck(Duration::ZERO);
    let tenant = format!("t-{}", uuid::Uuid::new_v4());
    let owner = ctx(&tenant, "alice");
    let dave = ctx(&tenant, "dave");
    let carol = ctx(&tenant, "carol");
    let run = live_run(&store, &owner).await;
    let dave_target = ShareTarget::User { id: "dave".into() };
    for target in [&dave_target, &ShareTarget::User { id: "carol".into() }] {
        store
            .share_thread(&owner, run.thread_id, target, ThreadRole::Viewer, None)
            .await
            .unwrap();
    }
    let mut dave_s = store
        .viewer_event_stream(&dave, run.thread_id, run.run_id, 0, None)
        .await
        .unwrap();
    let mut carol_s = store
        .viewer_event_stream(&carol, run.thread_id, run.run_id, 0, None)
        .await
        .unwrap();
    append(&store, &run, &token("共有中の本文")).await;
    assert_eq!(next_event(&mut dave_s).await, token("共有中の本文"));
    assert_eq!(next_event(&mut carol_s).await, token("共有中の本文"));

    store
        .unshare_thread(
            &owner,
            run.thread_id,
            &dave_target,
            ThreadRole::Viewer,
            None,
        )
        .await
        .unwrap();
    append(&store, &run, &token("外した後の本文 SECRET-AFTER-UNSHARE")).await;
    append(&store, &run, &done(&run)).await;

    assert_cut(&mut dave_s).await;
    assert_eq!(
        next_event(&mut carol_s).await,
        token("外した後の本文 SECRET-AFTER-UNSHARE")
    );
    assert_eq!(next_event(&mut carol_s).await, done(&run));
    // 読み込み直すと、配信の口そのものが断る。
    assert!(matches!(
        store
            .viewer_event_stream(&dave, run.thread_id, run.run_id, 0, None)
            .await,
        Err(chat::ChatError::Forbidden)
    ));
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
        stop_reason: None,
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
