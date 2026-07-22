//! CoolWSD headless クライアントの結合テスト（偽 CoolWSD・issue #352）。
//!
//! tokio TcpListener + tungstenite でスクリプト応答する偽サーバを立て、
//! ハンドシェイク〜検索〜照合〜paste〜save〜close のシーケンスと異常系
//! （load 失敗・途中 close・応答なしタイムアウト・searchnotfound）を検証する。
//! ワイヤ形式そのものの単体テストは `live/protocol.rs` 側にある。

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::future::Future;
use std::time::Duration;

use futures::{SinkExt, StreamExt};
use office::live::{CoolWsClient, CoolWsConfig, LiveError, SaveAck, SearchOutcome};
use tokio::net::TcpStream;
use tokio::task::JoinHandle;
use tokio_tungstenite::tungstenite::Message;
use tokio_tungstenite::WebSocketStream;

type ServerWs = WebSocketStream<TcpStream>;

/// 偽 CoolWSD を 1 接続分立てる。テスト末尾で JoinHandle を await して
/// サーバ側 assert のパニックをテスト失敗に伝播させること。
async fn spawn_server<F, Fut>(script: F) -> (String, JoinHandle<()>)
where
    F: FnOnce(ServerWs) -> Fut + Send + 'static,
    Fut: Future<Output = ()> + Send,
{
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let handle = tokio::spawn(async move {
        let (tcp, _) = listener.accept().await.unwrap();
        let ws = tokio_tungstenite::accept_async(tcp).await.unwrap();
        script(ws).await;
    });
    (format!("ws://{addr}"), handle)
}

async fn recv(ws: &mut ServerWs) -> Message {
    ws.next().await.unwrap().unwrap()
}

async fn recv_text(ws: &mut ServerWs) -> String {
    match recv(ws).await {
        Message::Text(t) => t.to_string(),
        other => panic!("テキストフレームを期待: {other:?}"),
    }
}

async fn send(ws: &mut ServerWs, line: &str) {
    ws.send(Message::Text(line.to_string().into()))
        .await
        .unwrap();
}

/// `coolclient`→`load` を受けて `loaded:` まで返す（正常系ハンドシェイク）。
async fn accept_handshake(ws: &mut ServerWs) -> String {
    let hello = recv_text(ws).await;
    assert!(hello.starts_with("coolclient 0.1 "), "hello: {hello}");
    let load = recv_text(ws).await;
    assert!(load.starts_with("load url="), "load: {load}");
    // access_token は load 行に含めない（WS パス側が運ぶ）。
    assert!(!load.contains("access_token"), "load: {load}");
    send(ws, "coolserver 25.04.9.5 abcdef 0.1").await;
    send(ws, r#"lokitversion {"ProductName":"FakeOffice"}"#).await;
    send(ws, "loaded: viewid=9 views=2 isfirst=false").await;
    load
}

fn fast_cfg(ws_base: &str) -> CoolWsConfig {
    let mut cfg = CoolWsConfig::new(ws_base);
    cfg.connect_timeout = Duration::from_secs(2);
    cfg.load_timeout = Duration::from_secs(2);
    cfg.op_timeout = Duration::from_millis(400);
    cfg.save_timeout = Duration::from_millis(600);
    cfg.total_deadline = Duration::from_secs(10);
    cfg
}

const WOPI_SRC: &str = "http://shiki-server:8080/wopi/files/00000000-0000-0000-0000-000000000001";

/// 正常系: 検索→選択照合→paste（Writer）→GoToCell→paste（Calc）→save→close。
#[tokio::test]
async fn happy_path_search_paste_save() {
    let (url, server) = spawn_server(|mut ws| async move {
        accept_handshake(&mut ws).await;
        // 検索: ノイズ（viewinfo）を挟んでも選択コールバックで ack される。
        let search = recv_text(&mut ws).await;
        assert!(search.starts_with("uno .uno:ExecuteSearch {"), "{search}");
        assert!(search.contains("旧文言"), "{search}");
        send(&mut ws, r#"viewinfo: [{"id":9,"username":"Shiki AI"}]"#).await;
        send(&mut ws, "textselection: 1284,1418 1104 275").await;
        // 選択照合。
        assert_eq!(
            recv_text(&mut ws).await,
            "gettextselection mimetype=text/plain;charset=utf-8"
        );
        send(&mut ws, "textselectioncontent: 旧文言").await;
        // paste（バイナリフレーム・mimetype ヘッダ＋HTML）。
        match recv(&mut ws).await {
            Message::Binary(bytes) => {
                let text = String::from_utf8(bytes.to_vec()).unwrap();
                assert!(
                    text.starts_with("paste mimetype=text/html;charset=utf-8\n<p>新文言</p>"),
                    "{text}"
                );
            }
            other => panic!("バイナリフレームを期待: {other:?}"),
        }
        send(&mut ws, "pasteresult: success").await;
        // Calc: GoToCell → paste。
        let goto = recv_text(&mut ws).await;
        assert!(goto.starts_with("uno .uno:GoToCell {"), "{goto}");
        assert!(goto.contains("Sheet2.B3"), "{goto}");
        send(&mut ws, "celladdress: B3").await;
        match recv(&mut ws).await {
            Message::Binary(bytes) => {
                assert!(bytes.starts_with(b"paste mimetype=text/html;charset=utf-8\n<table>"));
            }
            other => panic!("バイナリフレームを期待: {other:?}"),
        }
        send(&mut ws, "pasteresult: success").await;
        // save → core ack。
        assert_eq!(
            recv_text(&mut ws).await,
            "save dontTerminateEdit=1 dontSaveIfUnmodified=1"
        );
        send(
            &mut ws,
            r#"unocommandresult: {"commandName":".uno:Save","success":true}"#,
        )
        .await;
        // クライアント側の close を待つ。
        while let Some(Ok(msg)) = ws.next().await {
            if matches!(msg, Message::Close(_)) {
                break;
            }
        }
    })
    .await;

    let mut client = CoolWsClient::connect(&fast_cfg(&url), WOPI_SRC, "tok", "ja")
        .await
        .unwrap();
    assert_eq!(client.loaded().view_id, "9");
    assert_eq!(client.loaded().views, 2);

    assert_eq!(
        client.execute_search("旧文言").await.unwrap(),
        SearchOutcome::Selected
    );
    assert_eq!(client.selection_text().await.unwrap(), "旧文言");
    assert!(client
        .paste("text/html;charset=utf-8", "<p>新文言</p>".as_bytes())
        .await
        .unwrap());
    client.go_to_cell("Sheet2.B3").await.unwrap();
    assert!(client
        .paste(
            "text/html;charset=utf-8",
            b"<table><tr><td>1</td></tr></table>"
        )
        .await
        .unwrap());
    assert_eq!(client.save().await.unwrap(), SaveAck::CoreSaved);
    client.close().await;
    server.await.unwrap();
}

/// load 失敗（`error: cmd=load`）は Load に分類され、接続は成立しない。
#[tokio::test]
async fn load_failure_is_classified() {
    let (url, server) = spawn_server(|mut ws| async move {
        let _hello = recv_text(&mut ws).await;
        let _load = recv_text(&mut ws).await;
        send(&mut ws, "error: cmd=load kind=faileddocloading").await;
    })
    .await;
    let Err(err) = CoolWsClient::connect(&fast_cfg(&url), WOPI_SRC, "tok", "ja").await else {
        panic!("load 失敗を期待")
    };
    assert!(
        matches!(&err, LiveError::Load(msg) if msg.contains("faileddocloading")),
        "{err:?}"
    );
    server.await.unwrap();
}

/// `searchnotfound:` は NotFound として返る（エラーではない＝skipped 報告の入力）。
#[tokio::test]
async fn search_not_found() {
    let (url, server) = spawn_server(|mut ws| async move {
        accept_handshake(&mut ws).await;
        let _search = recv_text(&mut ws).await;
        send(&mut ws, "searchnotfound: 見つからない語").await;
    })
    .await;
    let mut client = CoolWsClient::connect(&fast_cfg(&url), WOPI_SRC, "tok", "ja")
        .await
        .unwrap();
    assert_eq!(
        client.execute_search("見つからない語").await.unwrap(),
        SearchOutcome::NotFound
    );
    server.await.unwrap();
}

/// 検索 ack が来ない場合は Unconfirmed（照合フェーズが最終判定する・エラーにしない）。
#[tokio::test]
async fn search_without_ack_is_unconfirmed() {
    let (url, server) = spawn_server(|mut ws| async move {
        accept_handshake(&mut ws).await;
        let _search = recv_text(&mut ws).await;
        // 何も返さない（クライアント切断まで待つ）。
        while ws.next().await.is_some() {}
    })
    .await;
    let mut client = CoolWsClient::connect(&fast_cfg(&url), WOPI_SRC, "tok", "ja")
        .await
        .unwrap();
    assert_eq!(
        client.execute_search("x").await.unwrap(),
        SearchOutcome::Unconfirmed
    );
    client.close().await;
    server.await.unwrap();
}

/// ops 途中の `close:` は Closed として即失敗する（部分適用の正直な報告へ）。
#[tokio::test]
async fn mid_session_close_fails_fast() {
    let (url, server) = spawn_server(|mut ws| async move {
        accept_handshake(&mut ws).await;
        let _get = recv_text(&mut ws).await;
        send(&mut ws, "close: recycling").await;
    })
    .await;
    let mut client = CoolWsClient::connect(&fast_cfg(&url), WOPI_SRC, "tok", "ja")
        .await
        .unwrap();
    let err = client.selection_text().await.unwrap_err();
    assert!(
        matches!(&err, LiveError::Closed(r) if r == "recycling"),
        "{err:?}"
    );
    server.await.unwrap();
}

/// paste の応答が無い場合はフェーズ名付きタイムアウト。
#[tokio::test]
async fn paste_timeout() {
    let (url, server) = spawn_server(|mut ws| async move {
        accept_handshake(&mut ws).await;
        let _paste = recv(&mut ws).await;
        // pasteresult を返さない。
        while ws.next().await.is_some() {}
    })
    .await;
    let mut client = CoolWsClient::connect(&fast_cfg(&url), WOPI_SRC, "tok", "ja")
        .await
        .unwrap();
    let err = client
        .paste("text/html;charset=utf-8", b"<p>x</p>")
        .await
        .unwrap_err();
    assert!(matches!(err, LiveError::Timeout("paste")), "{err:?}");
    client.close().await;
    server.await.unwrap();
}

/// 保存中の `error: cmd=storage` は SaveFailed に写る。
#[tokio::test]
async fn save_storage_error() {
    let (url, server) = spawn_server(|mut ws| async move {
        accept_handshake(&mut ws).await;
        let _save = recv_text(&mut ws).await;
        send(&mut ws, "error: cmd=storage kind=savefailed").await;
    })
    .await;
    let mut client = CoolWsClient::connect(&fast_cfg(&url), WOPI_SRC, "tok", "ja")
        .await
        .unwrap();
    let err = client.save().await.unwrap_err();
    assert!(
        matches!(&err, LiveError::SaveFailed(m) if m.contains("savefailed")),
        "{err:?}"
    );
    server.await.unwrap();
}

/// 保存 ack が来ない場合は Unverified（編集は適用済み・永続化未確認の報告へ）。
#[tokio::test]
async fn save_without_ack_is_unverified() {
    let (url, server) = spawn_server(|mut ws| async move {
        accept_handshake(&mut ws).await;
        let _save = recv_text(&mut ws).await;
        while ws.next().await.is_some() {}
    })
    .await;
    let mut client = CoolWsClient::connect(&fast_cfg(&url), WOPI_SRC, "tok", "ja")
        .await
        .unwrap();
    assert_eq!(client.save().await.unwrap(), SaveAck::Unverified);
    client.close().await;
    server.await.unwrap();
}
