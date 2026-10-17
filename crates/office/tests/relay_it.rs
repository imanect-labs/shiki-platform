//! ブラウザ ↔ Collabora 中継の結合テスト（PIT-68）。
//!
//! 偽の上流（axum: WS エコー＋HTTP）を立て、中継越しに
//! - 通常の編集命令は上流へ届き、上流の応答がブラウザへ返る
//! - `executescript` / `proxyreturn` は上流へ届かず、ブラウザ側は 1008 で切られる
//! - 許可リスト外の HTTP（convert-to・管理画面）は上流へ届かない
//!   ことを確かめる。実機 coolwsd に対する確認は `relay_live_collabora`（環境変数ゲート）。

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::print_stderr
)]

use std::sync::{Arc, Mutex};
use std::time::Duration;

use axum::extract::ws::{Message as AxMessage, WebSocketUpgrade};
use axum::extract::State;
use axum::routing::{any, get};
use axum::Router;
use futures::{SinkExt, StreamExt};
use office::relay::{build_relay_router, RelayConfig};
use tokio_tungstenite::tungstenite::client::IntoClientRequest;
use tokio_tungstenite::tungstenite::protocol::frame::coding::CloseCode;
use tokio_tungstenite::tungstenite::Message;

const DOC: &str = "http%3A%2F%2Fshiki-server%3A8080%2Fwopi%2Ffiles%2Fabc";

/// 上流が受け取ったもの（WS フレームと HTTP パス）。
type Seen = Arc<Mutex<Vec<String>>>;

async fn upstream_ws(State(seen): State<Seen>, ws: WebSocketUpgrade) -> axum::response::Response {
    ws.on_upgrade(move |mut socket| async move {
        while let Some(Ok(msg)) = socket.next().await {
            match msg {
                // バイナリも記録する（上流へ届いていないことを直接確かめるため）。
                AxMessage::Binary(b) => {
                    seen.lock()
                        .unwrap()
                        .push(format!("ws-bin:{}", String::from_utf8_lossy(&b)));
                }
                AxMessage::Text(t) => {
                    seen.lock().unwrap().push(format!("ws:{}", t.as_str()));
                    if socket
                        .send(AxMessage::Text(format!("echo:{}", t.as_str()).into()))
                        .await
                        .is_err()
                    {
                        break;
                    }
                }
                _ => {}
            }
        }
    })
}

async fn upstream_http(State(seen): State<Seen>, uri: axum::http::Uri) -> &'static str {
    seen.lock().unwrap().push(format!("http:{}", uri.path()));
    "upstream"
}

async fn serve(router: Router) -> String {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move { axum::serve(listener, router).await.unwrap() });
    format!("127.0.0.1:{}", addr.port())
}

/// 偽の上流と中継を立て、中継のアドレスと上流の観測記録を返す。
async fn setup() -> (String, Seen) {
    let seen: Seen = Arc::default();
    let upstream = Router::new()
        .route("/cool/{doc}/ws", get(upstream_ws))
        .fallback(any(upstream_http))
        .with_state(Arc::clone(&seen));
    let upstream_addr = serve(upstream).await;
    let relay = build_relay_router(
        RelayConfig {
            upstream_base: format!("http://{upstream_addr}"),
            max_body_bytes: 1 << 20,
        },
        reqwest::Client::builder()
            .redirect(reqwest::redirect::Policy::none())
            .build()
            .unwrap(),
    );
    (serve(relay).await, seen)
}

async fn connect(
    relay: &str,
) -> tokio_tungstenite::WebSocketStream<tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>> {
    let mut req = format!("ws://{relay}/cool/{DOC}/ws?WOPISrc=x&compat=/ws")
        .into_client_request()
        .unwrap();
    req.headers_mut()
        .insert("origin", "http://localhost:9980".parse().unwrap());
    tokio_tungstenite::connect_async(req).await.unwrap().0
}

async fn next_text(
    ws: &mut tokio_tungstenite::WebSocketStream<
        tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>,
    >,
) -> Message {
    tokio::time::timeout(Duration::from_secs(5), ws.next())
        .await
        .expect("応答待ちがタイムアウト")
        .expect("ストリーム終端")
        .expect("WS エラー")
}

#[tokio::test]
async fn ordinary_commands_pass_both_ways() {
    let (relay, seen) = setup().await;
    let mut ws = connect(&relay).await;
    ws.send(Message::Text("uno .uno:Bold".into()))
        .await
        .unwrap();
    assert_eq!(
        next_text(&mut ws).await,
        Message::Text("echo:uno .uno:Bold".into())
    );
    assert_eq!(seen.lock().unwrap().as_slice(), ["ws:uno .uno:Bold"]);
}

#[tokio::test]
async fn script_execution_is_dropped_and_connection_closed() {
    for command in [
        "executescript 1 (function(){return 1}).apply(null, []);",
        "  ExecuteScript 2 x",
        "proxyreturn 3 null",
    ] {
        let (relay, seen) = setup().await;
        let mut ws = connect(&relay).await;
        ws.send(Message::Text(command.into())).await.unwrap();
        match next_text(&mut ws).await {
            Message::Close(Some(frame)) => assert_eq!(frame.code, CloseCode::Policy),
            other => panic!("Close(1008) を期待したが {other:?}"),
        }
        // 上流にはスクリプト命令が 1 つも届いていない。
        tokio::time::sleep(Duration::from_millis(100)).await;
        assert!(
            seen.lock()
                .unwrap()
                .iter()
                .all(|s| !s.to_ascii_lowercase().contains("script") && !s.contains("proxyreturn")),
            "上流に届いた: {:?}",
            seen.lock().unwrap()
        );
    }
}

#[tokio::test]
async fn binary_frames_are_inspected_too() {
    let (relay, seen) = setup().await;
    let mut ws = connect(&relay).await;
    ws.send(Message::Binary(b"executescript 1 x".to_vec().into()))
        .await
        .unwrap();
    assert!(matches!(next_text(&mut ws).await, Message::Close(Some(_))));
    assert!(seen.lock().unwrap().is_empty());
}

#[tokio::test]
async fn http_allowlist_is_enforced() {
    let (relay, seen) = setup().await;
    let http = reqwest::Client::new();
    let ok = http
        .get(format!("http://{relay}/hosting/discovery"))
        .send()
        .await
        .unwrap();
    assert_eq!(ok.status(), 200);
    assert_eq!(ok.text().await.unwrap(), "upstream");
    for (method, path) in [
        (reqwest::Method::POST, "/cool/convert-to/pdf"),
        (reqwest::Method::POST, "/cool/transform-document-structure"),
        (reqwest::Method::GET, "/cool/adminws"),
        (reqwest::Method::GET, "/browser/dist/admin/admin.html"),
        (reqwest::Method::POST, &format!("/cool/{DOC}/ws/s/write/1")),
        // パーセントエンコードした `..`・管理画面で許可リストを抜けられないこと。
        (reqwest::Method::GET, "/browser/%2e%2e/cool/getMetrics"),
        (
            reqwest::Method::POST,
            "/browser/%2e%2e/cool/convert-to/pdf/x.html",
        ),
        (reqwest::Method::GET, "/browser/dist/%61dmin/admin.html"),
    ] {
        let res = http
            .request(method.clone(), format!("http://{relay}{path}"))
            .send()
            .await
            .unwrap();
        assert_eq!(res.status(), 404, "{method} {path}");
    }
    assert_eq!(seen.lock().unwrap().as_slice(), ["http:/hosting/discovery"]);
}

/// 実機 coolwsd に対する確認（`OFFICE_RELAY_TEST_COLLABORA=http://localhost:9982` と
/// `OFFICE_RELAY_TEST_WOPI_SRC=<WOPISrc>` があるときだけ走る）。
/// 中継越しに文書を開けること・executescript が届かないことを確かめる。
#[tokio::test]
async fn relay_live_collabora() {
    let (Ok(base), Ok(wopi_src)) = (
        std::env::var("OFFICE_RELAY_TEST_COLLABORA"),
        std::env::var("OFFICE_RELAY_TEST_WOPI_SRC"),
    ) else {
        eprintln!("skip: OFFICE_RELAY_TEST_COLLABORA / OFFICE_RELAY_TEST_WOPI_SRC 未設定");
        return;
    };
    let _ = tracing_subscriber::fmt().with_env_filter("warn").try_init();
    let relay = serve(build_relay_router(
        RelayConfig {
            upstream_base: base,
            max_body_bytes: 1 << 20,
        },
        reqwest::Client::builder()
            .redirect(reqwest::redirect::Policy::none())
            .build()
            .unwrap(),
    ))
    .await;
    let enc = |s: &str| {
        percent_encoding::utf8_percent_encode(s, percent_encoding::NON_ALPHANUMERIC).to_string()
    };
    let doc = enc(&format!("{wopi_src}?access_token=x"));
    let mut req = format!(
        "ws://{relay}/cool/{doc}/ws?WOPISrc={}&compat=/ws",
        enc(&wopi_src)
    )
    .into_client_request()
    .unwrap();
    req.headers_mut()
        .insert("origin", format!("http://{relay}").parse().unwrap());
    let mut ws = tokio_tungstenite::connect_async(req).await.unwrap().0;
    ws.send(Message::Text("coolclient 0.1 0 0".into()))
        .await
        .unwrap();
    ws.send(Message::Text(
        format!(
            "load url={} lang=ja deviceFormFactor=desktop",
            enc(&wopi_src)
        )
        .into(),
    ))
    .await
    .unwrap();
    let mut loaded = false;
    while let Ok(Some(Ok(msg))) = tokio::time::timeout(Duration::from_mins(1), ws.next()).await {
        if let Message::Text(t) = msg {
            if t.as_str().starts_with("loaded:") {
                loaded = true;
                break;
            }
        }
    }
    assert!(loaded, "中継越しに文書を開けなかった");
    ws.send(Message::Text(
        "executescript 9 (function(){return 'reached'}).apply(null, []);".into(),
    ))
    .await
    .unwrap();
    let mut closed_by_policy = false;
    while let Ok(Some(Ok(msg))) = tokio::time::timeout(Duration::from_secs(10), ws.next()).await {
        match msg {
            Message::Text(t) => assert!(
                !t.as_str().starts_with("executescriptresult"),
                "スクリプトが kit に届いた: {t}"
            ),
            Message::Close(Some(frame)) => {
                closed_by_policy = frame.code == CloseCode::Policy;
                break;
            }
            _ => {}
        }
    }
    assert!(closed_by_policy, "1008 で切られなかった");
}
