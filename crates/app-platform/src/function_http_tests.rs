//! ミニアプリの関数の外部への送り口の単体テスト（BUG-196・PIT-48 の受け入れ条件）。
//!
//! 名前解決は差し替え（[`MapResolver`]）で決め、相手はループバックの小さなサーバー（[`serve`]）にする。

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::collections::HashMap;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;

use tokio::io::{AsyncReadExt, AsyncWriteExt};

use super::*;

/// 決めた名前だけを決めたアドレスに解決する（引いた回数を数える）。`missing.` で始まる名前はアドレスが無い
/// （`NO_SUCH_HOST`）、ほかの知らない名前は引けない（`LOOKUP_FAILED`・DNS に届かない・一時的な失敗）。
struct MapResolver {
    map: HashMap<String, Vec<SocketAddr>>,
    lookups: AtomicUsize,
}

impl MapResolver {
    fn new(entries: &[(&str, &[&str])]) -> Arc<Self> {
        Arc::new(MapResolver {
            map: entries
                .iter()
                .map(|(h, ips)| {
                    (
                        (*h).to_string(),
                        ips.iter()
                            .map(|ip| SocketAddr::new(ip.parse().unwrap(), 0))
                            .collect(),
                    )
                })
                .collect(),
            lookups: AtomicUsize::new(0),
        })
    }
}

#[async_trait::async_trait]
impl HostResolver for MapResolver {
    async fn lookup(&self, host: &str, port: u16) -> Result<Vec<SocketAddr>, &'static str> {
        self.lookups.fetch_add(1, Ordering::SeqCst);
        self.map
            .get(host)
            .map(|addrs| {
                addrs
                    .iter()
                    .map(|a| SocketAddr::new(a.ip(), port))
                    .collect()
            })
            .ok_or(if host.starts_with("missing.") {
                net_guard::NO_SUCH_HOST
            } else {
                net_guard::LOOKUP_FAILED
            })
    }
}

/// 答えない DNS（名前解決の上限を確かめる）。
struct HangingResolver;

#[async_trait::async_trait]
impl HostResolver for HangingResolver {
    async fn lookup(&self, _: &str, _: u16) -> Result<Vec<SocketAddr>, &'static str> {
        std::future::pending().await
    }
}

/// 決めた応答を返すループバックのサーバー（受けた頼みの数と、最後の頼みの頭を残す）。
async fn serve(response: Vec<u8>) -> (SocketAddr, Arc<AtomicUsize>, Arc<std::sync::Mutex<String>>) {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let hits = Arc::new(AtomicUsize::new(0));
    let seen = Arc::new(std::sync::Mutex::new(String::new()));
    let (counter, last) = (Arc::clone(&hits), Arc::clone(&seen));
    tokio::spawn(async move {
        while let Ok((mut sock, _)) = listener.accept().await {
            counter.fetch_add(1, Ordering::SeqCst);
            let response = response.clone();
            let last = Arc::clone(&last);
            tokio::spawn(async move {
                let mut buf = vec![0u8; 8192];
                let n = sock.read(&mut buf).await.unwrap_or(0);
                *last.lock().unwrap() = String::from_utf8_lossy(&buf[..n]).into_owned();
                let _ = sock.write_all(&response).await;
                let _ = sock.shutdown().await;
            });
        }
    });
    (addr, hits, seen)
}

fn response(head: &str, body: &[u8]) -> Vec<u8> {
    let mut out = format!(
        "{head}\r\ncontent-length: {}\r\nconnection: close\r\n\r\n",
        body.len()
    )
    .into_bytes();
    out.extend_from_slice(body);
    out
}

fn code_of(resp: &HostResponse) -> &str {
    match resp {
        HostResponse::Err { code, .. } => code,
        HostResponse::Ok(_) => "ok",
    }
}

fn message_of(resp: &HostResponse) -> &str {
    match resp {
        HostResponse::Err { message, .. } => message,
        HostResponse::Ok(_) => "",
    }
}

/// 内部の IP・公開と内部の混在・リンクローカルとメタデータに解決される名前は断る（名前は 1 回だけ引く）。
/// ゲストに返す文に解決したアドレスを載せない。
#[tokio::test]
async fn refuses_names_that_resolve_to_internal_addresses() {
    let resolver = MapResolver::new(&[
        ("loop.example", &["127.0.0.1"]),
        ("mixed.example", &["93.184.216.34", "10.0.0.5"]),
        ("meta.example", &["169.254.169.254"]),
        ("mapped.example", &["::ffff:10.0.0.5"]),
        ("ula.example", &["fd00::1"]),
    ]);
    for host in [
        "loop.example",
        "mixed.example",
        "meta.example",
        "mapped.example",
        "ula.example",
    ] {
        let url = url::Url::parse(&format!("https://{host}/x")).unwrap();
        let err = resolve_checked(&*resolver, &url, 443).await.unwrap_err();
        assert_eq!(code_of(&err), "egress_denied", "{host}");
        let message = message_of(&err);
        assert!(message.contains(host), "{message}");
        for ip in ["127.0.0.1", "10.0.0.5", "169.254", "fd00"] {
            assert!(!message.contains(ip), "{message}");
        }
    }
    assert_eq!(
        resolver.lookups.load(Ordering::SeqCst),
        5,
        "名前は 1 回ずつ引く"
    );
    let meta = url::Url::parse("https://meta.example/").unwrap();
    assert!(
        message_of(&resolve_checked(&*resolver, &meta, 443).await.unwrap_err())
            .contains("特別なアドレス")
    );

    // IP の書き方（10 進の IPv4・IPv4 を写した IPv6）も、URL を読んだ時点で IP にそろえて断る。
    for raw in [
        "http://2130706433/",
        "http://0x7f.0.0.1/",
        "http://[::ffff:127.0.0.1]/",
        "http://[::1]/",
        "http://169.254.169.254/latest/meta-data/",
    ] {
        let url = url::Url::parse(raw).unwrap();
        let err = resolve_checked(&*resolver, &url, 80).await.unwrap_err();
        assert_eq!(code_of(&err), "egress_denied", "{raw}");
    }
    assert_eq!(
        resolver.lookups.load(Ordering::SeqCst),
        6,
        "IP のホストは名前を引かない"
    );
}

/// 公開の IP だけに解決される名前は通し、確かめたアドレスを返す。名前が無いと分かった名前はやり直さずに断り、
/// DNS に届かない・一時的に引けない名前はやり直せる失敗にする（ワークフローの送り口と同じ・BUG-202）。
#[tokio::test]
async fn passes_public_names_and_tells_missing_names_from_lookup_failures() {
    let resolver = MapResolver::new(&[("api.example", &["93.184.216.34", "2606:4700::1"])]);
    let url = url::Url::parse("https://api.example/v1").unwrap();
    let (domain, addrs) = resolve_checked(&*resolver, &url, 443).await.unwrap();
    assert_eq!(domain.as_deref(), Some("api.example"));
    assert_eq!(addrs.len(), 2);
    assert!(addrs.iter().all(|a| a.port() == 443));

    // 名前が無い（NXDOMAIN）: URL の誤りなので、やり直さない。
    let missing = url::Url::parse("https://missing.example/").unwrap();
    let err = resolve_checked(&*resolver, &missing, 443)
        .await
        .unwrap_err();
    assert!(matches!(
        &err,
        HostResponse::Err { code, retryable: false, .. } if code == "egress_denied"
    ));
    assert!(
        message_of(&err).contains("見つかりませんでした"),
        "{}",
        message_of(&err)
    );

    // DNS に届かない・一時的に引けない（EAI_AGAIN など）: やり直せる。URL の誤りだとは言い切らない。
    let unknown = url::Url::parse("https://nowhere.example/").unwrap();
    let err = resolve_checked(&*resolver, &unknown, 443)
        .await
        .unwrap_err();
    assert!(matches!(
        &err,
        HostResponse::Err { code, retryable: true, .. } if code == "egress_failed"
    ));
    let message = message_of(&err);
    assert!(message.contains("場所を調べられませんでした"), "{message}");
    assert!(message.contains("もう一度試してください"), "{message}");

    // 名前を引けてもアドレスが 1 つも無い答えも、引けなかったのと同じにやり直せる。
    let empty = MapResolver::new(&[("empty.example", &[])]);
    let url = url::Url::parse("https://empty.example/").unwrap();
    let err = resolve_checked(&*empty, &url, 443).await.unwrap_err();
    assert_eq!(code_of(&err), "egress_failed");
}

/// 答えない DNS は、上限で打ち切ってやり直せる失敗にする。
#[tokio::test]
async fn resolution_has_a_time_limit() {
    let url = url::Url::parse("https://slow.example/").unwrap();
    let err = resolve_checked(&HangingResolver, &url, 443)
        .await
        .unwrap_err();
    assert!(matches!(
        &err,
        HostResponse::Err { code, retryable: true, .. } if code == "egress_failed"
    ));
}

/// 確かめたアドレスへ接続を固定する（解決できない `.invalid` の名前でも届く＝接続のときに名前を引き直さない）。
/// Host の頭は URL の名前のまま。
#[tokio::test]
async fn connects_only_to_the_checked_address() {
    let (addr, hits, seen) = serve(response("HTTP/1.1 200 OK", "pinned ok".as_bytes())).await;
    let url = url::Url::parse(&format!("http://pinned.example.invalid:{}/x", addr.port())).unwrap();
    let out = send_pinned(
        url,
        reqwest::Method::GET,
        None,
        Some("pinned.example.invalid"),
        &[addr],
        None,
        MAX_BODY,
    )
    .await;
    let HostResponse::Ok(v) = out else {
        panic!("届くこと: {}", message_of(&out));
    };
    assert_eq!(v["status"], 200);
    assert_eq!(v["body"], "pinned ok");
    assert_eq!(v["truncated"], false);
    assert_eq!(hits.load(Ordering::SeqCst), 1);
    assert!(seen.lock().unwrap().contains("pinned.example.invalid"));
}

/// 3xx は追わず、応答のまま返す（Location の先へは送らない）。
#[tokio::test]
async fn does_not_follow_redirects() {
    let (target, target_hits, _) = serve(response("HTTP/1.1 200 OK", b"secret")).await;
    let (addr, hits, _) = serve(response(
        &format!(
            "HTTP/1.1 302 Found\r\nlocation: http://127.0.0.1:{}/",
            target.port()
        ),
        b"",
    ))
    .await;
    let url =
        url::Url::parse(&format!("http://redirect.example.invalid:{}/", addr.port())).unwrap();
    let out = send_pinned(
        url,
        reqwest::Method::GET,
        None,
        Some("redirect.example.invalid"),
        &[addr],
        None,
        MAX_BODY,
    )
    .await;
    let HostResponse::Ok(v) = out else {
        panic!("応答のまま返す: {}", message_of(&out));
    };
    assert_eq!(v["status"], 302);
    assert_eq!(hits.load(Ordering::SeqCst), 1);
    assert_eq!(target_hits.load(Ordering::SeqCst), 0, "追わない");
}

/// 本文はバイトの上限で打ち切り、途中で切れた文字は外す。
#[tokio::test]
async fn reads_the_body_only_up_to_the_byte_limit() {
    let body = "あいうえお".repeat(10); // 150 バイト
    let (addr, _, _) = serve(response("HTTP/1.1 200 OK", body.as_bytes())).await;
    let url = url::Url::parse(&format!("http://big.example.invalid:{}/", addr.port())).unwrap();
    let out = send_pinned(
        url,
        reqwest::Method::GET,
        None,
        Some("big.example.invalid"),
        &[addr],
        None,
        10,
    )
    .await;
    let HostResponse::Ok(v) = out else {
        panic!("{}", message_of(&out));
    };
    assert_eq!(v["truncated"], true);
    // 10 バイトのうち、まるごと入る 3 文字（9 バイト）だけを返す。
    assert_eq!(v["body"], "あいう");
}

/// 接続できないときは、下位の誤りの原文を返さない（やり直せる失敗）。
#[tokio::test]
async fn connection_failures_do_not_leak_library_text() {
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let addr = listener.local_addr().unwrap();
    drop(listener);
    let url = url::Url::parse(&format!(
        "http://down.example.invalid:{}/?key=secret",
        addr.port()
    ))
    .unwrap();
    let out = send_pinned(
        url,
        reqwest::Method::POST,
        Some(&serde_json::json!({ "a": 1 })),
        Some("down.example.invalid"),
        &[addr],
        None,
        MAX_BODY,
    )
    .await;
    assert!(matches!(
        &out,
        HostResponse::Err { code, retryable: true, .. } if code == "egress_failed"
    ));
    let message = message_of(&out);
    assert!(message.contains("down.example.invalid"), "{message}");
    assert!(
        !message.contains("secret") && !message.contains("error"),
        "{message}"
    );
}

#[test]
fn trims_only_a_cut_character() {
    let mut whole = "aあ".as_bytes().to_vec();
    trim_partial_utf8(&mut whole);
    assert_eq!(whole, "aあ".as_bytes());
    let mut cut = "aあ".as_bytes()[..3].to_vec();
    trim_partial_utf8(&mut cut);
    assert_eq!(cut, b"a");
    let mut emoji = "😀".as_bytes()[..2].to_vec();
    trim_partial_utf8(&mut emoji);
    assert!(emoji.is_empty());
    let mut empty = Vec::new();
    trim_partial_utf8(&mut empty);
    assert!(empty.is_empty());
}

#[path = "function_http_proxy_tests.rs"]
mod proxy;
