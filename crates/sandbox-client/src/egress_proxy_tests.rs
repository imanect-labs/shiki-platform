//! 外への送信の中継（[`super`]）の単体テスト（GAP-72）。
//!
//! 中継は小さな偽物（[`fake_proxy`]・CONNECT の要求を記録して、決めた応答を返す）で、外の送り先はループバックの
//! サーバー（[`origin`]）で作る。偽の中継は、頼まれたアドレスへはつながずに [`origin`] へつなぐ（外の世界の代わり）。

#![allow(clippy::unwrap_used)]

use std::net::SocketAddr;
use std::sync::{Arc, Mutex};

use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream, UnixStream};

use super::*;

/// 偽の中継の振る舞い（CONNECT の宛先ごとに決める）。
#[derive(Clone)]
enum Behavior {
    /// 200 を返し、`to` へつないで中継する（`early` は 200 の見出しと同じ書き込みで送る）。
    Tunnel { to: SocketAddr, early: &'static str },
    /// この応答の行を返して閉じる。
    Respond(&'static str),
    /// 何も返さずに閉じる。
    Close,
}

/// 受けた CONNECT の要求の見出し。
type Seen = Arc<Mutex<Vec<String>>>;

/// 偽の中継を立てる（`decide` は CONNECT の宛先から振る舞いを決める）。
async fn fake_proxy(
    decide: impl Fn(&str) -> Behavior + Send + Sync + 'static,
) -> (SocketAddr, Seen) {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let seen: Seen = Arc::default();
    let log = Arc::clone(&seen);
    let decide = Arc::new(decide);
    tokio::spawn(async move {
        while let Ok((mut sock, _)) = listener.accept().await {
            let log = Arc::clone(&log);
            let decide = Arc::clone(&decide);
            tokio::spawn(async move {
                let head = read_until_blank_line(&mut sock).await;
                let target = head
                    .lines()
                    .next()
                    .and_then(|l| l.split(' ').nth(1))
                    .unwrap_or_default()
                    .to_string();
                log.lock().unwrap().push(head);
                match decide(&target) {
                    Behavior::Tunnel { to, early } => {
                        let mut upstream = TcpStream::connect(to).await.unwrap();
                        let reply = format!("HTTP/1.1 200 Connection established\r\n\r\n{early}");
                        sock.write_all(reply.as_bytes()).await.unwrap();
                        let _ = tokio::io::copy_bidirectional(&mut sock, &mut upstream).await;
                    }
                    Behavior::Respond(line) => {
                        let _ = sock
                            .write_all(format!("{line}\r\ncontent-length: 0\r\n\r\n").as_bytes())
                            .await;
                    }
                    Behavior::Close => {}
                }
            });
        }
    });
    (addr, seen)
}

/// 見出しの終わり（空の行）まで読む。
async fn read_until_blank_line<S: AsyncReadExt + Unpin>(sock: &mut S) -> String {
    let mut buf = Vec::new();
    let mut byte = [0u8; 1];
    while !buf.ends_with(b"\r\n\r\n") {
        if sock.read(&mut byte).await.unwrap_or(0) == 0 {
            break;
        }
        buf.push(byte[0]);
    }
    String::from_utf8_lossy(&buf).into_owned()
}

/// 外の送り先の代わり（受けた要求の見出しを記録し、`ok` を返す）。
async fn origin() -> (SocketAddr, Seen) {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let seen: Seen = Arc::default();
    let log = Arc::clone(&seen);
    tokio::spawn(async move {
        while let Ok((mut sock, _)) = listener.accept().await {
            let head = read_until_blank_line(&mut sock).await;
            log.lock().unwrap().push(head);
            let _ = sock
                .write_all(b"HTTP/1.1 200 OK\r\ncontent-length: 2\r\nconnection: close\r\n\r\nok")
                .await;
            let _ = sock.shutdown().await;
        }
    });
    (addr, seen)
}

fn proxy_at(addr: SocketAddr, credentials: Option<(&str, &str)>) -> EgressProxy {
    EgressProxy::new(&format!("http://{addr}"), credentials).unwrap()
}

/// トンネルの口に HTTP の要求を書き、応答を最後まで読む。
async fn exchange(tunnel: &ProxyTunnel, request: &str) -> String {
    let mut local = UnixStream::connect(tunnel.socket_path()).await.unwrap();
    local.write_all(request.as_bytes()).await.unwrap();
    let mut out = String::new();
    local.read_to_string(&mut out).await.unwrap();
    out
}

const PUBLIC: &str = "93.184.216.34:443";

/// 中継には、確かめた IP アドレスへの CONNECT を頼み（名前を渡さない）、認証は CONNECT にだけ付ける。送り先には
/// URL の名前の Host が届き、中継の認証は届かない。
#[tokio::test]
async fn tunnels_to_the_checked_address_and_keeps_the_name_for_the_origin() {
    let (to, at_origin) = origin().await;
    let (proxy, at_proxy) = fake_proxy(move |_| Behavior::Tunnel { to, early: "" }).await;
    let tunnel = proxy_at(proxy, Some(("shiki", "s3cret")))
        .open(&[PUBLIC.parse().unwrap()])
        .await
        .unwrap();
    let reply = exchange(
        &tunnel,
        "GET /v1/items HTTP/1.1\r\nHost: api.example.test\r\nconnection: close\r\n\r\n",
    )
    .await;
    assert!(reply.ends_with("ok"), "{reply}");

    let connect = at_proxy.lock().unwrap()[0].clone();
    assert!(
        connect.starts_with("CONNECT 93.184.216.34:443 HTTP/1.1\r\nHost: 93.184.216.34:443\r\n"),
        "{connect}"
    );
    // base64("shiki:s3cret")
    assert!(
        connect.contains("Proxy-Authorization: Basic c2hpa2k6czNjcmV0\r\n"),
        "{connect}"
    );
    assert!(
        !connect.contains("api.example.test"),
        "名前を中継に渡さない"
    );

    let request = at_origin.lock().unwrap()[0].clone();
    assert!(request.contains("Host: api.example.test\r\n"), "{request}");
    assert!(!request.to_ascii_lowercase().contains("proxy-authorization"));
}

/// IPv6 の宛先は括弧で包んで頼む。認証を決めていなければ付けない。
#[tokio::test]
async fn ipv6_targets_are_bracketed_and_no_auth_is_sent_without_credentials() {
    let (to, _) = origin().await;
    let (proxy, at_proxy) = fake_proxy(move |_| Behavior::Tunnel { to, early: "" }).await;
    let tunnel = proxy_at(proxy, None)
        .open(&["[2606:2800:220:1::1]:443".parse().unwrap()])
        .await
        .unwrap();
    drop(tunnel);
    let connect = at_proxy.lock().unwrap()[0].clone();
    assert!(
        connect.starts_with("CONNECT [2606:2800:220:1::1]:443 HTTP/1.1\r\n"),
        "{connect}"
    );
    assert!(!connect.contains("Proxy-Authorization"));
}

/// 中継の答えを理由に分ける（認証と方針で断られたものはやり直さない）。断られたら、ほかのアドレスは試さない。
#[tokio::test]
async fn refusals_are_told_apart_and_not_retried_on_other_addresses() {
    let two: [SocketAddr; 2] = [
        PUBLIC.parse().unwrap(),
        "93.184.216.35:443".parse().unwrap(),
    ];
    for (behavior, want) in [
        (
            Behavior::Respond("HTTP/1.1 407 Proxy Authentication Required"),
            ProxyError::AuthRejected,
        ),
        (
            Behavior::Respond("HTTP/1.1 403 Forbidden"),
            ProxyError::Refused,
        ),
    ] {
        let (proxy, at_proxy) = fake_proxy(move |_| behavior.clone()).await;
        let err = proxy_at(proxy, None).open(&two).await.err().unwrap();
        assert_eq!(err, want);
        assert!(!err.is_retryable());
        assert_eq!(
            at_proxy.lock().unwrap().len(),
            1,
            "ほかのアドレスは試さない"
        );
    }
}

/// 中継が送り先へつなげない・応答を読めないときは、次のアドレスを（IPv4 を先に・2 つまで）試す。
#[tokio::test]
async fn upstream_failures_try_the_next_address_ipv4_first() {
    let (to, at_origin) = origin().await;
    let (proxy, at_proxy) = fake_proxy(move |target| match target {
        "93.184.216.34:443" => Behavior::Respond("HTTP/1.1 502 Bad Gateway"),
        _ => Behavior::Tunnel { to, early: "" },
    })
    .await;
    let addrs: [SocketAddr; 3] = [
        "[2606:2800:220:1::1]:443".parse().unwrap(),
        PUBLIC.parse().unwrap(),
        "93.184.216.35:443".parse().unwrap(),
    ];
    let tunnel = proxy_at(proxy, None).open(&addrs).await.unwrap();
    assert!(exchange(&tunnel, "GET / HTTP/1.1\r\nHost: a.test\r\n\r\n")
        .await
        .ends_with("ok"));
    let targets: Vec<String> = at_proxy
        .lock()
        .unwrap()
        .iter()
        .map(|h| h.split(' ').nth(1).unwrap().to_string())
        .collect();
    assert_eq!(targets, ["93.184.216.34:443", "93.184.216.35:443"]);
    assert_eq!(at_origin.lock().unwrap().len(), 1);

    // どれもつなげなければ、最後の理由（やり直せる）。
    for (behavior, want) in [
        (
            Behavior::Respond("HTTP/1.1 503 Service Unavailable"),
            ProxyError::UpstreamFailed,
        ),
        (Behavior::Close, ProxyError::BadResponse),
        (
            Behavior::Respond("SSH-2.0-OpenSSH"),
            ProxyError::BadResponse,
        ),
        (
            Behavior::Respond("HTTP/1.1 301 Moved"),
            ProxyError::BadResponse,
        ),
    ] {
        let (proxy, at_proxy) = fake_proxy(move |_| behavior.clone()).await;
        let err = proxy_at(proxy, None).open(&addrs).await.err().unwrap();
        assert_eq!(err, want);
        assert!(err.is_retryable());
        assert_eq!(at_proxy.lock().unwrap().len(), MAX_ATTEMPTS);
    }
}

/// 中継へつなげないときは、やり直せる失敗（ほかのアドレスは試さない）。
#[tokio::test]
async fn an_unreachable_proxy_is_a_retryable_failure() {
    let closed = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = closed.local_addr().unwrap();
    drop(closed);
    let err = proxy_at(addr, None)
        .open(&[PUBLIC.parse().unwrap()])
        .await
        .err()
        .unwrap();
    assert_eq!(err, ProxyError::Unreachable);
    assert!(err.is_retryable());
}

/// 見出しの後に続けて届いたバイトも、受けた側へ渡す（見出しが何回かに分かれて届いても読める）。
#[tokio::test]
async fn bytes_after_the_head_reach_the_client() {
    let (to, _) = origin().await;
    let (proxy, _) = fake_proxy(move |_| Behavior::Tunnel {
        to,
        early: "EARLY:",
    })
    .await;
    let tunnel = proxy_at(proxy, None)
        .open(&[PUBLIC.parse().unwrap()])
        .await
        .unwrap();
    let reply = exchange(&tunnel, "GET / HTTP/1.1\r\nHost: a.test\r\n\r\n").await;
    assert!(reply.starts_with("EARLY:HTTP/1.1 200 OK"), "{reply}");
}

/// トンネルの口は、このプロセスの利用者だけが入れるディレクトリに置き、最初の 1 本だけを受け、落とすと消える。
#[tokio::test]
async fn the_socket_is_private_accepts_one_connection_and_is_removed_on_drop() {
    use std::os::unix::fs::PermissionsExt as _;
    let (to, _) = origin().await;
    let (proxy, _) = fake_proxy(move |_| Behavior::Tunnel { to, early: "" }).await;
    let tunnel = proxy_at(proxy, None)
        .open(&[PUBLIC.parse().unwrap()])
        .await
        .unwrap();
    let dir = tunnel.socket_path().parent().unwrap().to_path_buf();
    assert_eq!(
        std::fs::metadata(&dir).unwrap().permissions().mode() & 0o777,
        0o700
    );
    assert!(exchange(&tunnel, "GET / HTTP/1.1\r\nHost: a.test\r\n\r\n")
        .await
        .ends_with("ok"));
    assert!(
        UnixStream::connect(tunnel.socket_path()).await.is_err(),
        "2 本目は受けない"
    );
    drop(tunnel);
    assert!(!dir.exists(), "落とすとディレクトリごと消える");
}

/// 中継の URL は http:// のホストとポートだけ。誤りの文は URL を映し返さず、表に資格情報を出さない。
#[test]
fn proxy_urls_are_checked_and_credentials_are_never_shown() {
    for (url, want) in [
        (
            "http://shiki:s3cret@proxy.corp.example:8080",
            "利用者名やパスワード",
        ),
        ("https://proxy.corp.example:8443", "http:// で始めて"),
        ("socks5://proxy.corp.example:1080", "http:// で始めて"),
        ("http://proxy.corp.example:8080/path", "ホストとポートだけ"),
        ("http://proxy.corp.example:8080/?a=1", "ホストとポートだけ"),
        ("proxy.corp.example:8080", "http:// で始めて"),
        ("not a url", "URL として読めません"),
    ] {
        let err = EgressProxy::new(url, None).unwrap_err();
        assert!(err.contains(want), "{url}: {err}");
        assert!(
            !err.contains("s3cret") && !err.contains(url),
            "{url}: {err}"
        );
    }
    assert_eq!(
        EgressProxy::new("http://Proxy.Corp.Example:8080/", None)
            .unwrap()
            .endpoint(),
        "proxy.corp.example:8080"
    );
    assert_eq!(
        EgressProxy::new("http://[fd00::1]:3128", None)
            .unwrap()
            .endpoint(),
        "[fd00::1]:3128"
    );
    assert_eq!(
        EgressProxy::new("http://10.0.0.8", None)
            .unwrap()
            .endpoint(),
        "10.0.0.8:80"
    );
    for (user, password, want) in [
        ("", "s3cret", "空です"),
        ("a:b", "s3cret", "「:」"),
        ("shiki", "s3cret\nx", "改行"),
    ] {
        let err = EgressProxy::new("http://proxy:8080", Some((user, password))).unwrap_err();
        assert!(err.contains(want), "{user}: {err}");
        assert!(!err.contains("s3cret"), "{err}");
    }
    let proxy = EgressProxy::new("http://proxy:8080", Some(("shiki", "s3cret"))).unwrap();
    let shown = format!("{proxy:?}");
    assert_eq!(
        shown,
        "EgressProxy { endpoint: \"proxy:8080\", authenticated: true }"
    );
}

/// 中継を通すのは、確かめたアドレスがすべて公開の IP のときだけ（内部の送り先は直接つなぐ）。
#[test]
fn only_public_addresses_go_through_the_proxy() {
    let public: SocketAddr = PUBLIC.parse().unwrap();
    let internal: SocketAddr = "10.0.0.5:443".parse().unwrap();
    assert!(via_proxy(&[public]));
    assert!(!via_proxy(&[internal]));
    assert!(!via_proxy(&[public, internal]));
    assert!(!via_proxy(&[]));
}
