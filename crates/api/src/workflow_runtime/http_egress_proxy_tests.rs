//! 外への送信の中継（`egress.proxy_url`）を通した送り口のテスト（GAP-72）。
//!
//! 偽の中継（[`fake_proxy`]）は CONNECT の要求を記録し、頼まれたアドレスへはつながずにループバックの相手
//! （[`recording_origin`]）へつなぐ（外の世界の代わり）。中継を通しても、名前解決の後のアドレスの確かめ・内部の
//! 送り先・名前の扱いが変わらないことを見る。

use std::sync::Mutex;

use super::*;

/// 偽の中継の振る舞い。
#[derive(Clone, Copy)]
enum Behavior {
    /// 200 を返して `origin` へつなぐ。
    Tunnel(SocketAddr),
    /// この応答の行を返して閉じる。
    Respond(&'static str),
    /// CONNECT を受けたまま答えない（接続は開いたまま）。
    Silent,
}

/// 受けた要求の見出し（中継は CONNECT、相手は HTTP の要求・TLS ならその始まりのバイト）。
type Seen = Arc<Mutex<Vec<String>>>;

async fn read_head(sock: &mut tokio::net::TcpStream) -> String {
    let mut head = Vec::new();
    let mut byte = [0u8; 1];
    while !head.ends_with(b"\r\n\r\n") && sock.read(&mut byte).await.unwrap_or(0) == 1 {
        head.push(byte[0]);
    }
    String::from_utf8_lossy(&head).into_owned()
}

async fn fake_proxy(behavior: Behavior) -> (SocketAddr, Seen) {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let seen: Seen = Arc::default();
    let log = Arc::clone(&seen);
    tokio::spawn(async move {
        while let Ok((mut sock, _)) = listener.accept().await {
            let head = read_head(&mut sock).await;
            log.lock().unwrap().push(head);
            match behavior {
                Behavior::Tunnel(origin) => {
                    let mut upstream = tokio::net::TcpStream::connect(origin).await.unwrap();
                    sock.write_all(b"HTTP/1.1 200 Connection established\r\n\r\n")
                        .await
                        .unwrap();
                    let _ = tokio::io::copy_bidirectional(&mut sock, &mut upstream).await;
                }
                Behavior::Respond(line) => {
                    let _ = sock
                        .write_all(format!("{line}\r\ncontent-length: 0\r\n\r\n").as_bytes())
                        .await;
                }
                Behavior::Silent => {
                    tokio::spawn(async move {
                        tokio::time::sleep(Duration::from_secs(60)).await;
                        drop(sock);
                    });
                }
            }
        }
    });
    (addr, seen)
}

/// 受けた最初のバイト列（HTTP の見出しか TLS の ClientHello）を記録し、`hello` を返すか（TLS なら）閉じる。
async fn recording_origin() -> (SocketAddr, Seen) {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let seen: Seen = Arc::default();
    let log = Arc::clone(&seen);
    tokio::spawn(async move {
        while let Ok((mut sock, _)) = listener.accept().await {
            let mut buf = vec![0u8; 4096];
            let n = sock.read(&mut buf).await.unwrap_or(0);
            let first = String::from_utf8_lossy(&buf[..n]).into_owned();
            let tls = buf.first() == Some(&0x16);
            log.lock().unwrap().push(first);
            if !tls {
                let _ = sock.write_all(ok_response("hello").as_bytes()).await;
            }
            let _ = sock.shutdown().await;
        }
    });
    (addr, seen)
}

fn proxy_at(addr: SocketAddr, credentials: Option<(&str, &str)>) -> Option<EgressProxy> {
    Some(EgressProxy::new(&format!("http://{addr}"), credentials).unwrap())
}

const PUBLIC: &str = "93.184.216.34";

/// 公開の送り先へは、確かめた IP アドレスへの CONNECT を中継に頼み（名前は渡さない）、中継の認証は CONNECT にだけ
/// 付ける。相手には URL の名前の Host と、作成者のヘッダーが届く。名前を引くのは 1 回だけ。
#[tokio::test]
async fn public_destinations_go_through_the_proxy_to_the_checked_address() {
    let (origin, at_origin) = recording_origin().await;
    let (proxy, at_proxy) = fake_proxy(Behavior::Tunnel(origin)).await;
    let resolver = MapResolver::new(&[(
        "api.example.test",
        &[format!("{PUBLIC}:80").parse().unwrap()],
    )]);
    let egress = HttpEgress::default()
        .with_resolver(Arc::clone(&resolver) as _)
        .with_proxy(proxy_at(proxy, Some(("shiki", "s3cret"))));
    let mut req = get("http://api.example.test/v1/items");
    req.headers = vec![("X-Request-Id".into(), "r-1".into())];
    let resp = egress.send(req).await.unwrap();
    assert_eq!(
        (resp.status, resp.body.as_slice()),
        (200, b"hello".as_slice())
    );
    assert_eq!(resolver.lookups.load(Ordering::SeqCst), 1);

    let connect = at_proxy.lock().unwrap()[0].clone();
    assert!(
        connect.starts_with(&format!("CONNECT {PUBLIC}:80 HTTP/1.1\r\n")),
        "{connect}"
    );
    assert!(connect.contains("Proxy-Authorization: Basic "), "{connect}");
    assert!(
        !connect.contains("api.example.test"),
        "名前を中継に渡さない"
    );
    let request = at_origin.lock().unwrap()[0].clone();
    assert!(
        request.starts_with("GET /v1/items HTTP/1.1\r\n"),
        "{request}"
    );
    assert!(request.contains("host: api.example.test\r\n"), "{request}");
    assert!(request.contains("x-request-id: r-1\r\n"), "{request}");
    assert!(!request.contains("proxy-authorization"), "{request}");
}

/// https は、トンネルの中で URL の名前の TLS を結ぶ（中継には IP への CONNECT・相手には名前の SNI）。
#[tokio::test]
async fn tls_runs_inside_the_tunnel_with_the_name_of_the_url() {
    let (origin, at_origin) = recording_origin().await;
    let (proxy, at_proxy) = fake_proxy(Behavior::Tunnel(origin)).await;
    let resolver = MapResolver::new(&[(
        "api.example.test",
        &[format!("{PUBLIC}:443").parse().unwrap()],
    )]);
    let egress = HttpEgress::default()
        .with_resolver(resolver)
        .with_proxy(proxy_at(proxy, None));
    // 相手は TLS を話さずに閉じるので、送信は失敗する（届いた ClientHello だけを見る）。
    let err = egress
        .send(get("https://api.example.test/"))
        .await
        .unwrap_err();
    assert_eq!(err.code, "unavailable", "{err:?}");
    let connect = at_proxy.lock().unwrap()[0].clone();
    assert!(
        connect.starts_with(&format!("CONNECT {PUBLIC}:443 HTTP/1.1\r\n")),
        "{connect}"
    );
    let hello = at_origin.lock().unwrap()[0].clone();
    assert!(hello.contains("api.example.test"), "SNI は URL の名前");
}

/// 管理者が内部の送り先として許したホスト（内部の IP）は、中継を通さずに直接つなぐ。
#[tokio::test]
async fn internal_hosts_are_reached_directly_not_through_the_proxy() {
    let (port, hits) = serve(ok_response("intranet")).await;
    let (origin, _) = recording_origin().await;
    let (proxy, at_proxy) = fake_proxy(Behavior::Tunnel(origin)).await;
    let resolver = MapResolver::new(&[("intranet.invalid", &[loopback(port)])]);
    let egress = internal(&["intranet.invalid"])
        .with_resolver(resolver)
        .with_proxy(proxy_at(proxy, None));
    let resp = egress
        .send(get(&format!("http://intranet.invalid:{port}/")))
        .await
        .unwrap();
    assert_eq!(resp.body, b"intranet");
    assert_eq!(hits.load(Ordering::SeqCst), 1);
    assert!(at_proxy.lock().unwrap().is_empty(), "中継を通さない");
}

/// 内部の IP に解決される名前は、中継を使う設定でも中継へ頼まずに断る（中継の向こうで内部へ届かせない）。
#[tokio::test]
async fn internal_answers_are_refused_before_asking_the_proxy() {
    let (origin, _) = recording_origin().await;
    let (proxy, at_proxy) = fake_proxy(Behavior::Tunnel(origin)).await;
    let internal_ip: SocketAddr = "10.0.0.5:80".parse().unwrap();
    let meta: SocketAddr = "169.254.169.254:80".parse().unwrap();
    let resolver = MapResolver::new(&[("evil.test", &[internal_ip]), ("meta.test", &[meta])]);
    let egress = HttpEgress::default()
        .with_resolver(resolver)
        .with_proxy(proxy_at(proxy, None));
    let err = egress.send(get("http://evil.test/")).await.unwrap_err();
    assert_eq!(err.code, DESTINATION_INTERNAL);
    let err = egress.send(get("http://meta.test/")).await.unwrap_err();
    assert_eq!(err.code, DESTINATION_LINK_LOCAL);
    assert!(at_proxy.lock().unwrap().is_empty());
}

/// 中継が認証や方針で断ったら、やり直さない（送り先には何も送っていないと言う）。中継の場所と資格情報は文に出さない。
#[tokio::test]
async fn refusals_by_the_proxy_are_not_retried_and_say_nothing_was_sent() {
    for (line, cause) in [
        (
            "HTTP/1.1 407 Proxy Authentication Required",
            "外への通信の中継（プロキシ）が、このサーバーの認証を受け付けませんでした。",
        ),
        (
            "HTTP/1.1 403 Forbidden",
            "外への通信の中継（プロキシ）が、この送り先への接続を断りました。",
        ),
    ] {
        let (proxy, _) = fake_proxy(Behavior::Respond(line)).await;
        let resolver = MapResolver::new(&[(
            "api.example.test",
            &[format!("{PUBLIC}:443").parse().unwrap()],
        )]);
        let egress = HttpEgress::default()
            .with_resolver(resolver)
            .with_proxy(proxy_at(proxy, Some(("shiki", "s3cret"))));
        let err = egress
            .send(get("https://api.example.test/"))
            .await
            .unwrap_err();
        assert_eq!(err.code, EGRESS_PROXY_REFUSED);
        assert!(!err.retryable);
        assert_eq!(
            err.message,
            format!("{cause}送り先には何も送っていません。管理者に、中継の設定を確かめるよう頼んでください。")
        );
        assert!(!err.message.contains("s3cret") && !err.message.contains(&proxy.to_string()));
    }
}

/// 中継へつなげない・中継が送り先へつなげないときは、やり直せる失敗（送り先には何も送っていない）。
#[tokio::test]
async fn unreachable_proxies_and_upstream_failures_are_retryable() {
    let closed = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let gone = closed.local_addr().unwrap();
    drop(closed);
    let (bad_gateway, _) = fake_proxy(Behavior::Respond("HTTP/1.1 502 Bad Gateway")).await;
    for (proxy, cause) in [
        (gone, "外への通信の中継（プロキシ）に接続できませんでした。"),
        (
            bad_gateway,
            "外への通信の中継（プロキシ）から、送り先に接続できませんでした。",
        ),
    ] {
        let resolver = MapResolver::new(&[(
            "api.example.test",
            &[format!("{PUBLIC}:443").parse().unwrap()],
        )]);
        let egress = HttpEgress::default()
            .with_resolver(resolver)
            .with_proxy(proxy_at(proxy, None));
        let err = egress
            .send(get("https://api.example.test/"))
            .await
            .unwrap_err();
        assert_eq!(err.code, "unavailable");
        assert!(err.retryable);
        assert_eq!(
            err.message,
            format!(
                "{cause}送り先には何も送っていないので、少し待ってから、もう一度実行してください。続くときは、\
                 管理者に知らせてください。"
            )
        );
    }
}

/// 中継が CONNECT に答えないときも、ブロックの時間の上限より長く待たない（トンネルを開く時間も上限に数える）。
/// 送り先には何も送っていないので、やり直せる失敗。
#[tokio::test]
async fn a_silent_proxy_does_not_outlast_the_time_limit() {
    let (proxy, at_proxy) = fake_proxy(Behavior::Silent).await;
    let resolver = MapResolver::new(&[(
        "api.example.test",
        &[
            format!("{PUBLIC}:443").parse().unwrap(),
            "93.184.216.35:443".parse().unwrap(),
        ],
    )]);
    let egress = HttpEgress::default()
        .with_resolver(resolver)
        .with_proxy(proxy_at(proxy, None));
    let mut req = get("https://api.example.test/");
    req.timeout_ms = Some(300);
    let started = std::time::Instant::now();
    let err = egress.send(req).await.unwrap_err();
    let took = started.elapsed();
    assert!(took < Duration::from_secs(3), "{took:?}");
    assert_eq!(err.code, "unavailable");
    assert!(err.retryable);
    assert!(
        err.message.contains("送り先には何も送っていない"),
        "{}",
        err.message
    );
    assert_eq!(
        at_proxy.lock().unwrap().len(),
        1,
        "CONNECT を頼んだのは 1 回だけ"
    );
}
