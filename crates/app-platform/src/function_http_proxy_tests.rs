//! ミニアプリの関数の外部への送信を、外への送信の中継（`egress.proxy_url`）に通すテスト（GAP-72）。
//!
//! 偽の中継（[`fake_proxy`]）は CONNECT の要求の 1 行目を記録し、頼まれたアドレスへはつながずにループバックの相手
//! （[`serve`]）へつなぐ（外の世界の代わり）。

use std::sync::Mutex;

use super::*;

/// 偽の中継（`line` が `None` なら 200 を返して `origin` へつなぐ。`Some` ならその応答の行を返して閉じる）。
async fn fake_proxy(
    origin: SocketAddr,
    line: Option<&'static str>,
) -> (SocketAddr, Arc<Mutex<Vec<String>>>) {
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
            let first = String::from_utf8_lossy(&head)
                .lines()
                .next()
                .unwrap_or_default()
                .to_string();
            log.lock().unwrap().push(first);
            if let Some(line) = line {
                let _ = sock
                    .write_all(format!("{line}\r\ncontent-length: 0\r\n\r\n").as_bytes())
                    .await;
                continue;
            }
            let mut upstream = tokio::net::TcpStream::connect(origin).await.unwrap();
            sock.write_all(b"HTTP/1.1 200 Connection established\r\n\r\n")
                .await
                .unwrap();
            let _ = tokio::io::copy_bidirectional(&mut sock, &mut upstream).await;
        }
    });
    (addr, seen)
}

fn proxy_at(addr: SocketAddr) -> EgressProxy {
    EgressProxy::new(&format!("http://{addr}"), Some(("shiki", "s3cret"))).unwrap()
}

/// 公開の送り先へは、確かめた IP アドレスへの CONNECT を中継に頼み（名前は渡さない）、Host は URL の名前のまま。
#[tokio::test]
async fn public_destinations_go_through_the_proxy_to_the_checked_address() {
    let (origin, hits, seen) = serve(response("HTTP/1.1 200 OK", b"via proxy")).await;
    let (proxy, connects) = fake_proxy(origin, None).await;
    let proxy = proxy_at(proxy);
    let checked: SocketAddr = "93.184.216.34:443".parse().unwrap();
    let tunnel = open_tunnel(Some(&proxy), &[checked], "api.example.test")
        .await
        .unwrap();
    assert!(tunnel.is_some());
    // 相手は平文で答えるので http の URL で送る（中継のトンネルは http も同じ形で通す）。
    let url = url::Url::parse("http://api.example.test/x").unwrap();
    let out = send_pinned(
        url,
        reqwest::Method::GET,
        None,
        Some("api.example.test"),
        &[checked],
        tunnel.as_ref(),
        MAX_BODY,
    )
    .await;
    let HostResponse::Ok(v) = out else {
        panic!("届くこと: {}", message_of(&out));
    };
    assert_eq!(v["body"], "via proxy");
    assert_eq!(hits.load(Ordering::SeqCst), 1);
    assert_eq!(
        *connects.lock().unwrap(),
        ["CONNECT 93.184.216.34:443 HTTP/1.1"]
    );
    let request = seen.lock().unwrap().clone();
    assert!(request.contains("host: api.example.test"), "{request}");
    assert!(!request.contains("proxy-authorization"), "{request}");
}

/// 内部の IP を含むアドレス・中継を設定していないときは、トンネルを開かない（直接つなぐ）。
#[tokio::test]
async fn internal_addresses_and_no_proxy_do_not_open_a_tunnel() {
    let (origin, _, _) = serve(response("HTTP/1.1 200 OK", b"x")).await;
    let (proxy, connects) = fake_proxy(origin, None).await;
    let proxy = proxy_at(proxy);
    let internal: SocketAddr = "10.0.0.5:443".parse().unwrap();
    assert!(open_tunnel(Some(&proxy), &[internal], "erp.corp.example")
        .await
        .unwrap()
        .is_none());
    let public: SocketAddr = "93.184.216.34:443".parse().unwrap();
    assert!(open_tunnel(None, &[public], "api.example.test")
        .await
        .unwrap()
        .is_none());
    assert!(connects.lock().unwrap().is_empty());
}

/// 中継が認証や方針で断ったらやり直さない `egress_denied`、つなげないならやり直せる `egress_failed`。どちらも送り先には
/// 何も送っていないと言い、中継の場所と資格情報を出さない。
#[tokio::test]
async fn proxy_failures_say_nothing_was_sent() {
    let (origin, _, _) = serve(response("HTTP/1.1 200 OK", b"x")).await;
    let (denied, _) = fake_proxy(origin, Some("HTTP/1.1 407 Proxy Authentication Required")).await;
    let public: SocketAddr = "93.184.216.34:443".parse().unwrap();
    let out = open_tunnel(Some(&proxy_at(denied)), &[public], "api.example.test")
        .await
        .unwrap_err();
    assert!(matches!(
        &out,
        HostResponse::Err { code, retryable: false, .. } if code == "egress_denied"
    ));
    assert_eq!(
        message_of(&out),
        "外への通信の中継（プロキシ）が、このサーバーの認証を受け付けませんでした。送り先には何も送っていません。\
         管理者に、中継の設定を確かめるよう頼んでください。"
    );

    let closed = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let gone = closed.local_addr().unwrap();
    drop(closed);
    let out = open_tunnel(Some(&proxy_at(gone)), &[public], "api.example.test")
        .await
        .unwrap_err();
    assert!(matches!(
        &out,
        HostResponse::Err { code, retryable: true, .. } if code == "egress_failed"
    ));
    let message = message_of(&out);
    assert_eq!(
        message,
        "外への通信の中継（プロキシ）に接続できませんでした。送り先には何も送っていないので、少し待ってから、\
         もう一度試してください。"
    );
    assert!(!message.contains("s3cret") && !message.contains(&gone.to_string()));
}
