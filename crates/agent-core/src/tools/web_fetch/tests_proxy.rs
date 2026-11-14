//! 外への送信の中継（`egress.proxy_url`）を通した取得のテスト（GAP-72）。
//!
//! 偽の中継（[`fake_proxy`]）は CONNECT の要求を記録し、頼まれたアドレスへはつながずにループバックのスタブへつなぐ
//! （外の世界の代わり）。中継を通しても、検証済みのアドレスと名前の扱いが変わらないことを見る。

#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::sync::Mutex;

use tokio::io::{AsyncReadExt, AsyncWriteExt};

use super::tests::{ctx, http_response, stub_server, tool_with};
use super::*;

/// 偽の中継（受けた CONNECT の要求の 1 行目を記録し、`origin` へつなぐ）。
async fn fake_proxy(origin: SocketAddr) -> (SocketAddr, Arc<Mutex<Vec<String>>>) {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let seen: Arc<Mutex<Vec<String>>> = Arc::default();
    let log = Arc::clone(&seen);
    tokio::spawn(async move {
        while let Ok((mut sock, _)) = listener.accept().await {
            let mut head = Vec::new();
            let mut byte = [0u8; 1];
            while !head.ends_with(b"\r\n\r\n") && sock.read(&mut byte).await.unwrap_or(0) == 1 {
                head.push(byte[0]);
            }
            let head = String::from_utf8_lossy(&head).into_owned();
            log.lock()
                .unwrap()
                .push(head.lines().next().unwrap_or_default().to_string());
            let mut upstream = tokio::net::TcpStream::connect(origin).await.unwrap();
            sock.write_all(b"HTTP/1.1 200 Connection established\r\n\r\n")
                .await
                .unwrap();
            let _ = tokio::io::copy_bidirectional(&mut sock, &mut upstream).await;
        }
    });
    (addr, seen)
}

/// 中継を使う設定では、検証済みの公開 IP への CONNECT を中継に頼み（名前は渡さない）、Host は URL の名前のまま。
/// 名前を引くのは 1 回だけ（`.invalid` は OS の DNS で引けない）。
#[tokio::test]
async fn fetches_through_the_proxy_with_the_checked_address() {
    let (origin, seen) = stub_server(http_response(
        "HTTP/1.1 200 OK\r\nContent-Type: text/html; charset=utf-8",
        "<html>中継の向こう</html>",
    ))
    .await;
    let (proxy, connects) = fake_proxy(origin).await;
    let (tool, calls) = tool_with(vec!["93.184.216.34:80".parse().unwrap()]);
    let tool = tool.with_proxy(Some(
        EgressProxy::new(&format!("http://{proxy}"), None).unwrap(),
    ));
    let out = tool
        .call(
            &ctx(),
            serde_json::json!({"url": "http://news.example.invalid/a"}),
            None,
        )
        .await
        .unwrap();
    assert!(!out.is_error, "{}", out.content);
    assert!(out.content.contains("中継の向こう"), "{}", out.content);
    assert_eq!(
        *connects.lock().unwrap(),
        ["CONNECT 93.184.216.34:80 HTTP/1.1"]
    );
    assert_eq!(calls.load(std::sync::atomic::Ordering::SeqCst), 1);
    assert!(
        seen.lock().await.contains("news.example.invalid"),
        "Host は URL の名前のまま"
    );
}

/// 内部の IP に解決される名前は、中継を使う設定でも中継へ頼まずに断る（中継の向こうで内部へ届かせない）。
#[tokio::test]
async fn internal_answers_are_refused_before_asking_the_proxy() {
    let (origin, _) = stub_server(http_response("HTTP/1.1 200 OK", "x")).await;
    let (proxy, connects) = fake_proxy(origin).await;
    let (tool, _) = tool_with(vec!["10.0.0.5:443".parse().unwrap()]);
    let tool = tool.with_proxy(Some(
        EgressProxy::new(&format!("http://{proxy}"), None).unwrap(),
    ));
    let err = tool
        .call(
            &ctx(),
            serde_json::json!({"url": "https://example.com/"}),
            None,
        )
        .await
        .unwrap_err();
    assert!(matches!(err, ToolError::Invalid(_)), "{err:?}");
    assert!(connects.lock().unwrap().is_empty());
}

/// 中継へつなげないときは、中継の場所や資格情報を出さずに、取得の失敗として返す。
#[tokio::test]
async fn an_unreachable_proxy_is_reported_without_details() {
    let closed = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = closed.local_addr().unwrap();
    drop(closed);
    let (tool, _) = tool_with(vec!["93.184.216.34:443".parse().unwrap()]);
    let proxy = EgressProxy::new(&format!("http://{addr}"), Some(("shiki", "s3cret"))).unwrap();
    let out = tool
        .with_proxy(Some(proxy))
        .call(
            &ctx(),
            serde_json::json!({"url": "https://example.com/"}),
            None,
        )
        .await
        .unwrap();
    assert!(out.is_error);
    assert_eq!(
        out.content,
        "取得に失敗しました: 外への通信の中継（プロキシ）に接続できませんでした。"
    );
}
