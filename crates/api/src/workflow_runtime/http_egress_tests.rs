//! 外部への送り口の単体テスト（BUG-181・PIT-48 の受け入れ条件・D-135）。
//!
//! 名前解決は差し替え（[`MapResolver`]）で決め、相手はループバックの小さなサーバー（[`serve`]）にする。

use std::collections::HashMap;
use std::net::SocketAddr;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;

use tokio::io::{AsyncReadExt, AsyncWriteExt};

use super::*;

/// 決めた名前だけを決めたアドレスに解決する（引いた回数を数える）。
struct MapResolver {
    map: HashMap<String, Vec<SocketAddr>>,
    lookups: AtomicUsize,
}

impl MapResolver {
    fn new(entries: &[(&str, &[SocketAddr])]) -> Arc<Self> {
        Arc::new(MapResolver {
            map: entries
                .iter()
                .map(|(h, a)| ((*h).to_string(), a.to_vec()))
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
            .ok_or("ホストを解決できませんでした")
    }
}

/// 決めた応答を返すループバックのサーバー（受けた頼みの数を数える）。
async fn serve(response: String) -> (u16, Arc<AtomicUsize>) {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    let hits = Arc::new(AtomicUsize::new(0));
    let counter = Arc::clone(&hits);
    tokio::spawn(async move {
        while let Ok((mut sock, _)) = listener.accept().await {
            counter.fetch_add(1, Ordering::SeqCst);
            let response = response.clone();
            tokio::spawn(async move {
                let mut buf = vec![0u8; 8192];
                let _ = sock.read(&mut buf).await;
                let _ = sock.write_all(response.as_bytes()).await;
                let _ = sock.shutdown().await;
            });
        }
    });
    (port, hits)
}

fn ok_response(body: &str) -> String {
    format!(
        "HTTP/1.1 200 OK\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{body}",
        body.len()
    )
}

fn get(url: &str) -> HttpSendReq {
    HttpSendReq {
        method: "GET".into(),
        url: url.into(),
        headers: vec![],
        body: None,
        follow_redirects: false,
        timeout_ms: Some(5_000),
    }
}

fn loopback(port: u16) -> SocketAddr {
    SocketAddr::from(([127, 0, 0, 1], port))
}

fn allow(hosts: &[&str]) -> HttpEgress {
    HttpEgress::new(&hosts.iter().map(|h| (*h).to_string()).collect::<Vec<_>>())
}

/// 内部の IP に解決される名前は、送らずに断る（作成者向けの文・やり直さない）。
#[tokio::test]
async fn names_resolving_to_internal_ips_are_refused() {
    let (port, hits) = serve(ok_response("secret")).await;
    let resolver = MapResolver::new(&[("evil.test", &[loopback(port)])]);
    let egress = HttpEgress::default().with_resolver(resolver);
    let err = egress
        .send(get(&format!("http://evil.test:{port}/")))
        .await
        .unwrap_err();
    assert_eq!(err.code, DESTINATION_INTERNAL);
    assert!(!err.retryable);
    assert_eq!(
        err.message,
        "送り先 evil.test は内部のネットワークにつながるため、送るのをやめました。社内のサービスに送るには、\
         管理者にこの送り先の許可を頼んでください。"
    );
    assert_eq!(hits.load(Ordering::SeqCst), 0, "内部へ接続していない");
}

/// 公開と内部の混在は、公開の側だけを選り分けて繋がずに、応答ごと断る。
#[tokio::test]
async fn mixed_public_and_internal_answers_are_refused() {
    let (port, hits) = serve(ok_response("x")).await;
    let public: SocketAddr = "93.184.216.34:80".parse().unwrap();
    let resolver = MapResolver::new(&[("mixed.test", &[public, loopback(port)])]);
    let egress = HttpEgress::default().with_resolver(resolver);
    let err = egress
        .send(get(&format!("http://mixed.test:{port}/")))
        .await
        .unwrap_err();
    assert_eq!(err.code, DESTINATION_INTERNAL);
    assert_eq!(hits.load(Ordering::SeqCst), 0);
}

/// 管理者がそのホストをそのまま許すと、内部の IP にも送れる。名前は 1 回だけ引き、確かめたアドレスに接続する
/// （`.invalid` は OS の DNS では引けない名前なので、届いたことが引き直していない証明になる）。
#[tokio::test]
async fn explicitly_allowed_hosts_connect_to_the_checked_address_without_a_second_lookup() {
    let (port, hits) = serve(ok_response("hello")).await;
    let resolver = MapResolver::new(&[("intranet.invalid", &[loopback(port)])]);
    let egress = allow(&["Intranet.invalid"]).with_resolver(Arc::clone(&resolver) as _);
    let resp = egress
        .send(get(&format!("http://intranet.invalid:{port}/x")))
        .await
        .unwrap();
    assert_eq!(resp.status, 200);
    assert_eq!(resp.body, b"hello");
    assert_eq!(hits.load(Ordering::SeqCst), 1);
    assert_eq!(
        resolver.lookups.load(Ordering::SeqCst),
        1,
        "名前は 1 回だけ引く"
    );
}

/// ワイルドカードの許可は、内部の IP への送信を許さない（そのホストをそのまま書いたときだけ）。
#[tokio::test]
async fn wildcard_allowlist_entries_do_not_allow_internal_ips() {
    let (port, hits) = serve(ok_response("x")).await;
    let resolver = MapResolver::new(&[("a.corp.invalid", &[loopback(port)])]);
    let egress = allow(&["*.corp.invalid", "corp.invalid"]).with_resolver(resolver);
    let err = egress
        .send(get(&format!("http://a.corp.invalid:{port}/")))
        .await
        .unwrap_err();
    assert_eq!(err.code, DESTINATION_INTERNAL);
    assert_eq!(hits.load(Ordering::SeqCst), 0);
}

/// リンクローカルとクラウドのメタデータは、管理者が許していても断る（名前・IP の書き方によらない）。
#[tokio::test]
async fn link_local_and_metadata_are_refused_even_when_allowed() {
    let meta: SocketAddr = "169.254.169.254:80".parse().unwrap();
    let resolver = MapResolver::new(&[("metadata.invalid", &[meta])]);
    let egress = allow(&[
        "metadata.invalid",
        "169.254.169.254",
        "[::ffff:a9fe:a9fe]",
        "[fd00:ec2::254]",
    ])
    .with_resolver(resolver);
    for url in [
        "http://metadata.invalid/latest/meta-data/",
        "http://169.254.169.254/latest/meta-data/",
        "http://2852039166/",          // 169.254.169.254 の 10 進
        "http://0251.0376.0251.0376/", // 8 進
        "http://0xa9.0xfe.0xa9.0xfe/", // 16 進
        "http://[::ffff:169.254.169.254]/",
        "http://[fd00:ec2::254]/",
        "http://[fe80::1]/",
    ] {
        let err = egress.send(get(url)).await.unwrap_err();
        assert_eq!(err.code, DESTINATION_LINK_LOCAL, "{url}");
        assert!(!err.retryable, "{url}");
        assert!(
            err.message
                .contains("サーバーの設定を読み出せる特別なアドレスにつながるため、送れません。"),
            "{url}: {}",
            err.message
        );
    }
}

/// 10 進・8 進・16 進の IPv4 と、IPv4 を写した IPv6 は、そろえた IP の分類どおりに断る。
#[tokio::test]
async fn alternative_ip_notations_are_classified_like_the_ip() {
    let (port, hits) = serve(ok_response("x")).await;
    let egress = HttpEgress::default();
    for url in [
        format!("http://2130706433:{port}/"), // 127.0.0.1 の 10 進
        format!("http://0177.0.0.1:{port}/"), // 8 進
        format!("http://0x7f.1:{port}/"),     // 16 進と省略
        format!("http://127.1:{port}/"),      // 省略
        format!("http://[::ffff:127.0.0.1]:{port}/"), // IPv4 を写した IPv6
        format!("http://[::ffff:7f00:1]:{port}/"),
        format!("http://[::1]:{port}/"),
        format!("http://10.0.0.1:{port}/"),
    ] {
        let err = egress.send(get(&url)).await.unwrap_err();
        assert_eq!(err.code, DESTINATION_INTERNAL, "{url}");
    }
    assert_eq!(hits.load(Ordering::SeqCst), 0, "どれも接続していない");
}

/// 3xx は追わない（相手の応答のまま返し、移った先には送らない）。
#[tokio::test]
async fn redirects_are_not_followed() {
    let (target, target_hits) = serve(ok_response("moved")).await;
    let (port, hits) = serve(format!(
        "HTTP/1.1 302 Found\r\nlocation: http://127.0.0.1:{target}/\r\ncontent-length: 0\r\nconnection: close\r\n\r\n"
    ))
    .await;
    let egress = allow(&["127.0.0.1"]);
    let resp = egress
        .send(get(&format!("http://127.0.0.1:{port}/")))
        .await
        .unwrap();
    assert_eq!(resp.status, 302);
    assert_eq!(hits.load(Ordering::SeqCst), 1);
    assert_eq!(target_hits.load(Ordering::SeqCst), 0, "移った先へ送らない");
}

/// 本文は上限まで読み、超えた分は読まない。
#[tokio::test]
async fn bodies_are_cut_at_the_limit() {
    let (port, _) = serve(ok_response(&"a".repeat(100))).await;
    let egress = allow(&["127.0.0.1"]).with_max_body(10);
    let resp = egress
        .send(get(&format!("http://127.0.0.1:{port}/")))
        .await
        .unwrap();
    assert_eq!(resp.body, b"aaaaaaaaaa");
}

/// 解決できない名前は、やり直せる失敗にする（送り先の場所を調べられない）。
#[tokio::test]
async fn unresolvable_names_are_a_retryable_failure() {
    let egress = HttpEgress::default().with_resolver(MapResolver::new(&[]));
    let err = egress
        .send(get("https://nowhere.invalid/"))
        .await
        .unwrap_err();
    assert_eq!(err.code, "unavailable");
    assert!(err.retryable);
    assert_eq!(
        err.message,
        "送り先のサービス（nowhere.invalid）の場所を調べられませんでした。URL のドメインを確かめてください。"
    );
}

/// 確かめた名前のほかは引かない（固定の名前解決）。
#[tokio::test]
async fn pinned_resolver_answers_only_the_checked_name() {
    use reqwest::dns::Resolve as _;
    let pinned = PinnedResolver {
        host: "api.example.com".into(),
        addrs: vec![loopback(1)],
    };
    let ok = pinned
        .resolve("API.example.com".parse().unwrap())
        .await
        .unwrap()
        .collect::<Vec<_>>();
    assert_eq!(ok, vec![loopback(1)]);
    assert!(pinned
        .resolve("other.example.com".parse().unwrap())
        .await
        .is_err());
}
