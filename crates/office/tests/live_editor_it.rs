//! `LiveEditor` の結合テスト（偽 CoolWSD＋実 Postgres・issue #352）。
//!
//! 検証（PIT-45 受け入れ条件）:
//! - 同一ファイルへの並行 `apply` は advisory lock で**直列化**される（AI↔AI）
//! - 別ファイルへの並行 `apply` は並行に進む（直列化はファイル単位）
//! - 存在秘匿（editor なし→Denied）・非対応種別（Unsupported）
//!
//! `STORAGE_TEST_DATABASE_URL` 未設定ならスキップ（wopi_it と同じ流儀）。

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::print_stderr,
    clippy::pedantic
)]

use std::sync::Arc;
use std::time::Duration;

use authz::{AuthContext, AuthzClient, Relation};
use futures::{SinkExt, StreamExt};
use office::live::{CoolWsConfig, LiveEditError, LiveEditor, LiveOp};
use office::OfficeTokenKey;
use sqlx::{postgres::PgPoolOptions, PgPool};
use storage::{Node, StorageService};
use tokio::sync::mpsc;
use tokio_tungstenite::tungstenite::Message;
use uuid::Uuid;

mod common;
use common::{ctx_for, MemStore, RoleAuthz};

const DOCX_TYPE: &str = "application/vnd.openxmlformats-officedocument.wordprocessingml.document";

struct Env {
    storage: Arc<StorageService>,
    authz: Arc<RoleAuthz>,
    pool: PgPool,
}

async fn setup() -> Option<Env> {
    let Ok(db_url) = std::env::var("STORAGE_TEST_DATABASE_URL") else {
        eprintln!("STORAGE_TEST_DATABASE_URL 未設定のためスキップ");
        return None;
    };
    let pool: PgPool = PgPoolOptions::new()
        .max_connections(8)
        .connect(&db_url)
        .await
        .expect("connect");
    sqlx::migrate!("../../migrations")
        .run(&pool)
        .await
        .expect("migrate");
    let authz = Arc::new(RoleAuthz::new());
    let authz_dyn: Arc<dyn AuthzClient> = authz.clone();
    let storage = Arc::new(StorageService::new(
        pool.clone(),
        Arc::new(MemStore::default()),
        authz_dyn,
        Duration::from_secs(120),
        Duration::from_secs(900),
        64 * 1024 * 1024,
    ));
    Some(Env {
        storage,
        authz,
        pool,
    })
}

async fn create_file(env: &Env, owner: &AuthContext, name: &str, content_type: &str) -> Node {
    env.authz.grant(&owner.subject(), Relation::Member);
    env.authz.grant(&owner.subject(), Relation::Editor);
    env.authz.grant(&owner.subject(), Relation::Viewer);
    let bytes = format!("body:{}", Uuid::new_v4());
    env.storage
        .write_file_internal(owner, None, name, bytes.as_bytes(), content_type, None)
        .await
        .expect("create file")
}

fn editor_for(env: &Env, ws_base: &str) -> LiveEditor {
    LiveEditor::new(
        Arc::clone(&env.storage),
        env.authz.clone(),
        env.pool.clone(),
        OfficeTokenKey::random(),
        "http://shiki-server:8080",
        CoolWsConfig::new(ws_base),
    )
}

/// セッションのライフサイクルイベント（接続順序の検証用）。
#[derive(Debug, PartialEq, Eq, Clone, Copy)]
enum Ev {
    Start(usize),
    End(usize),
}

/// 偽 CoolWSD: 接続ごとに handshake→ExecuteSearch へ `searchnotfound:` を返す
/// （適用 0 件＝save 無しで最速で終わる）。`delay` は search 応答前の待ち
/// （直列化の観測窓を作る）。イベントを `events` へ流す。
async fn spawn_fake_coolwsd(
    connections: usize,
    delay: Duration,
    events: mpsc::UnboundedSender<Ev>,
) -> String {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        for i in 0..connections {
            let (tcp, _) = listener.accept().await.unwrap();
            let events = events.clone();
            // 並行に応対する（直列化はサーバ側でなく LiveEditor 側の責務）。
            tokio::spawn(async move {
                let mut ws = tokio_tungstenite::accept_async(tcp).await.unwrap();
                events.send(Ev::Start(i)).unwrap();
                // coolclient / load を読み捨てて loaded: を返す。
                let _ = ws.next().await;
                let _ = ws.next().await;
                ws.send(Message::Text(
                    "loaded: viewid=1 views=1 isfirst=true".into(),
                ))
                .await
                .unwrap();
                // ExecuteSearch → （delay 後に）searchnotfound。
                let _ = ws.next().await;
                tokio::time::sleep(delay).await;
                ws.send(Message::Text("searchnotfound: x".into()))
                    .await
                    .unwrap();
                // クライアントの close を待つ。
                while let Some(Ok(msg)) = ws.next().await {
                    if matches!(msg, Message::Close(_)) {
                        break;
                    }
                }
                events.send(Ev::End(i)).unwrap();
            });
        }
    });
    format!("ws://{addr}")
}

/// イベントを n 件（タイムアウト付きで）回収する。クライアント側の close 完了と
/// サーバ側の End 送出は非同期なので、try_recv でなく待って集める。
async fn drain_events(rx: &mut mpsc::UnboundedReceiver<Ev>, n: usize) -> Vec<Ev> {
    let mut events = Vec::new();
    while events.len() < n {
        match tokio::time::timeout(Duration::from_secs(5), rx.recv()).await {
            Ok(Some(ev)) => events.push(ev),
            _ => break,
        }
    }
    events
}

fn ops() -> Vec<LiveOp> {
    vec![LiveOp::ReplaceText {
        find: "存在しない文字列".into(),
        html: "<p>x</p>".into(),
    }]
}

/// 同一ファイルへの並行 apply は直列化される（2 本目の接続は 1 本目の終了後）。
#[tokio::test]
async fn same_file_ai_edits_are_serialized() {
    let Some(env) = setup().await else { return };
    let alice = ctx_for("alice", "default");
    let node = create_file(
        &env,
        &alice,
        &format!("a-{}.docx", Uuid::new_v4()),
        DOCX_TYPE,
    )
    .await;
    let (tx, mut rx) = mpsc::unbounded_channel();
    let ws_base = spawn_fake_coolwsd(2, Duration::from_millis(800), tx).await;
    let editor = Arc::new(editor_for(&env, &ws_base));

    let ops1 = ops();
    let ops2 = ops();
    let (r1, r2) = tokio::join!(
        editor.apply(&alice, node.id, &ops1),
        editor.apply(&alice, node.id, &ops2),
    );
    let report1 = r1.expect("apply 1");
    let report2 = r2.expect("apply 2");
    assert!(!report1.results[0].applied);
    assert!(!report2.results[0].applied);

    // イベント順: Start→End→Start→End（接続が重ならない＝直列化）。
    let events = drain_events(&mut rx, 4).await;
    assert_eq!(events.len(), 4, "{events:?}");
    assert!(matches!(events[0], Ev::Start(_)), "{events:?}");
    assert!(matches!(events[1], Ev::End(_)), "{events:?}");
    assert!(matches!(events[2], Ev::Start(_)), "{events:?}");
    assert!(matches!(events[3], Ev::End(_)), "{events:?}");
}

/// 別ファイルへの並行 apply は並行に進む（直列化はファイル単位）。
#[tokio::test]
async fn different_files_run_concurrently() {
    let Some(env) = setup().await else { return };
    let alice = ctx_for("alice", "default");
    let node_a = create_file(
        &env,
        &alice,
        &format!("b-{}.docx", Uuid::new_v4()),
        DOCX_TYPE,
    )
    .await;
    let node_b = create_file(
        &env,
        &alice,
        &format!("c-{}.docx", Uuid::new_v4()),
        DOCX_TYPE,
    )
    .await;
    let (tx, mut rx) = mpsc::unbounded_channel();
    // search 応答を 1.2 秒遅らせ、並行なら Start が 2 回先行する観測窓を作る。
    let ws_base = spawn_fake_coolwsd(2, Duration::from_millis(1200), tx).await;
    let editor = Arc::new(editor_for(&env, &ws_base));

    let ops1 = ops();
    let ops2 = ops();
    let (r1, r2) = tokio::join!(
        editor.apply(&alice, node_a.id, &ops1),
        editor.apply(&alice, node_b.id, &ops2),
    );
    r1.expect("apply a");
    r2.expect("apply b");

    // 2 接続が重なっている（Start, Start が End より先）。
    let events = drain_events(&mut rx, 4).await;
    assert_eq!(events.len(), 4, "{events:?}");
    assert!(
        matches!((events[0], events[1]), (Ev::Start(_), Ev::Start(_))),
        "並行実行を期待: {events:?}"
    );
}

/// editor が無い実行主体は存在秘匿（Denied）。非対応種別は Unsupported。
#[tokio::test]
async fn denies_and_unsupported() {
    let Some(env) = setup().await else { return };
    let alice = ctx_for("alice", "default");
    let node = create_file(
        &env,
        &alice,
        &format!("d-{}.docx", Uuid::new_v4()),
        DOCX_TYPE,
    )
    .await;
    // CoolWSD へは到達しない（authz/種別ゲートで手前落ち）。
    let editor = editor_for(&env, "ws://127.0.0.1:1");

    // editor 権限のない bob は Denied（存在秘匿・viewer があっても書けない）。
    let bob = ctx_for("bob", "default");
    env.authz.grant(&bob.subject(), Relation::Viewer);
    let bob_ops = ops();
    let err = editor
        .apply(&bob, node.id, &bob_ops)
        .await
        .expect_err("denied");
    assert!(matches!(err, LiveEditError::Denied), "{err:?}");

    // 非対応 content_type（md）は Unsupported。
    let md = create_file(
        &env,
        &alice,
        &format!("e-{}.md", Uuid::new_v4()),
        "text/markdown",
    )
    .await;
    let md_ops = ops();
    let err = editor
        .apply(&alice, md.id, &md_ops)
        .await
        .expect_err("unsupported");
    assert!(matches!(err, LiveEditError::Unsupported), "{err:?}");
}
