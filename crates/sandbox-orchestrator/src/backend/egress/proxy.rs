//! ホスト名フィルタリング TCP プロキシ。netns 内で bind したリスナ（ホストランタイムが service）で
//! ゲスト接続を受け、ClientHello SNI / HTTP Host を覗いて allowlist 判定し、許可時のみ実ホストへ中継する。
//!
//! 判定は全件 `target="sandbox_audit"` で監査する（遮断/許可イベントを残す・Task 4.6 受け入れ条件）。

use std::net::IpAddr;
use std::sync::Arc;
use std::time::Duration;

use sandbox_client::Egress;
use tokio::net::{TcpListener, TcpStream};

use super::rules::{self, Decision};
use super::sni;

/// 監査コンテキスト（テナント・サンドボックス識別）。
#[derive(Debug, Clone)]
pub struct EgressAudit {
    pub tenant_id: String,
    pub sandbox_id: String,
}

/// 覗いたバイト列からホスト名と判定を導く（純関数・テスト可）。
///
/// TLS ClientHello の SNI を先に試し、無ければ HTTP Host を試す（ポートに依らず・カスタムポートの
/// 平文 HTTP でもホスト名を取れる）。ポートは allowlist 判定にのみ使う。
#[must_use]
pub fn classify(port: u16, peeked: &[u8], egress: &Egress) -> (Option<String>, Decision) {
    let host = sni::parse_tls_sni(peeked).or_else(|| sni::parse_http_host(peeked));
    let decision = match &host {
        Some(h) => rules::evaluate(egress, h, port),
        None => Decision::DenyNoHostname,
    };
    (host, decision)
}

/// SSRF 防御: プロキシが中継してはいけない解決先 IP か（内部/予約レンジ）。
///
/// allowlist はホスト名で判定するが、`web_fetch` の動的許可などホスト名が実質攻撃者制御の経路では、
/// そのホストがクラウドメタデータ（169.254.169.254）・loopback・私設/リンクローカル/ULA 等へ解決され得る。
/// 解決後 IP をここで弾く。テスト用に `SANDBOX_EGRESS_ALLOW_PRIVATE=1` で私設許可（本番は設定しない）。
#[must_use]
pub(super) fn is_forbidden_upstream(ip: IpAddr) -> bool {
    match ip {
        IpAddr::V4(v4) => {
            v4.is_loopback()
                || v4.is_private()
                || v4.is_link_local()
                || v4.is_broadcast()
                || v4.is_documentation()
                || v4.is_unspecified()
                || v4.octets()[0] == 0
                // CGNAT 100.64.0.0/10 / benchmarking 198.18.0.0/15 も内部扱い。
                || (v4.octets()[0] == 100 && (64..128).contains(&v4.octets()[1]))
                || (v4.octets()[0] == 198 && (v4.octets()[1] & 0xfe) == 18)
        }
        IpAddr::V6(v6) => {
            v6.is_loopback()
                || v6.is_unspecified()
                // リンクローカル fe80::/10・ULA fc00::/7。
                || (v6.segments()[0] & 0xffc0) == 0xfe80
                || (v6.segments()[0] & 0xfe00) == 0xfc00
                // IPv4-mapped は v4 側の判定へ委ねる。
                || v6.to_ipv4_mapped().is_some_and(is_forbidden_v4_mapped)
        }
    }
}

fn is_forbidden_v4_mapped(v4: std::net::Ipv4Addr) -> bool {
    is_forbidden_upstream(IpAddr::V4(v4))
}

fn allow_private_upstream() -> bool {
    std::env::var("SANDBOX_EGRESS_ALLOW_PRIVATE").as_deref() == Ok("1")
}

/// 1 ポート分の accept ループ。リスナが閉じるまで各接続を処理する。
pub(super) async fn serve_port(
    listener: TcpListener,
    port: u16,
    egress: Arc<Egress>,
    audit: EgressAudit,
) {
    loop {
        let (client, _peer) = match listener.accept().await {
            Ok(pair) => pair,
            Err(e) => {
                tracing::debug!(target: "sandbox_audit", %port, error=%e, "egress accept ended");
                return;
            }
        };
        let egress = Arc::clone(&egress);
        let audit = audit.clone();
        tokio::spawn(async move {
            handle_conn(client, port, &egress, &audit).await;
        });
    }
}

/// ホスト名が取れるまで（タイムアウト付きで）先読みし、覗いたバイト列と判定を返す。
///
/// 固定バイト数の下限は設けない（短い正当な HTTP リクエスト＝~30 バイトも遮断しない）。
/// ホスト名を取れたら即返し、取れなければ短時間リトライして最後に判定する。
async fn peek_and_classify(
    client: &TcpStream,
    port: u16,
    egress: &Egress,
) -> (Option<String>, Decision) {
    let mut buf = vec![0u8; 8192];
    let deadline = tokio::time::Instant::now() + Duration::from_secs(3);
    loop {
        match tokio::time::timeout(Duration::from_millis(200), client.peek(&mut buf)).await {
            // 相手が閉じた / peek エラー → ホスト名不明で遮断。
            Ok(Ok(0) | Err(_)) => return (None, Decision::DenyNoHostname),
            Ok(Ok(n)) => {
                let (host, decision) = classify(port, &buf[..n], egress);
                if host.is_some() {
                    return (host, decision);
                }
            }
            Err(_) => {}
        }
        if tokio::time::Instant::now() >= deadline {
            // 最終試行（現在のバッファで判定）。
            return classify(port, &buf, egress);
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
}

async fn handle_conn(mut client: TcpStream, port: u16, egress: &Egress, audit: &EgressAudit) {
    let (host, decision) = peek_and_classify(&client, port, egress).await;
    let host_str = host.as_deref().unwrap_or("<none>");
    tracing::info!(
        target: "sandbox_audit",
        tenant = %audit.tenant_id,
        sandbox_id = %audit.sandbox_id,
        host = %host_str,
        %port,
        decision = decision.reason(),
        "egress decision"
    );
    if !decision.is_allow() {
        // 遮断: 接続を閉じる（client は drop で FIN）。
        return;
    }
    let Some(host) = host else { return };

    // 実ホストへ接続する前に、解決先 IP を SSRF フィルタで検査する（内部/予約レンジを弾く）。
    let Some(mut upstream) = connect_filtered(&host, port).await else {
        return;
    };
    // peek で消費していないため、client の先頭バイトはそのまま upstream へ流れる。
    let _ = tokio::io::copy_bidirectional(&mut client, &mut upstream).await;
}

/// ホスト名を解決し、許可された（非内部）IP にのみ接続する。
async fn connect_filtered(host: &str, port: u16) -> Option<TcpStream> {
    let allow_private = allow_private_upstream();
    let resolved = tokio::time::timeout(
        Duration::from_secs(5),
        tokio::net::lookup_host((host, port)),
    )
    .await
    .ok()?
    .ok()?;
    for addr in resolved {
        if !allow_private && is_forbidden_upstream(addr.ip()) {
            tracing::warn!(
                target: "sandbox_audit",
                %host, %port, ip = %addr.ip(),
                "egress upstream rejected (internal/reserved address・SSRF 防御)"
            );
            continue;
        }
        if let Ok(Ok(s)) =
            tokio::time::timeout(Duration::from_secs(10), TcpStream::connect(addr)).await
        {
            return Some(s);
        }
    }
    tracing::debug!(target: "sandbox_audit", %host, %port, "egress upstream connect failed/blocked");
    None
}

#[cfg(test)]
mod tests {
    use super::*;
    use sandbox_client::EgressRule;
    use tokio::io::AsyncReadExt;

    fn allow(host: &str, port: u16) -> Egress {
        Egress {
            static_allow: vec![EgressRule {
                host_pattern: host.to_string(),
                port,
            }],
            ..Egress::blocked()
        }
    }

    // sni.rs のテストヘルパと同型の ClientHello を再構成する。
    fn client_hello(host: &str) -> Vec<u8> {
        let name = host.as_bytes();
        let mut sn_entry = vec![0x00];
        sn_entry.extend_from_slice(&(name.len() as u16).to_be_bytes());
        sn_entry.extend_from_slice(name);
        let mut sn_list = (sn_entry.len() as u16).to_be_bytes().to_vec();
        sn_list.extend_from_slice(&sn_entry);
        let mut ext = 0x0000u16.to_be_bytes().to_vec();
        ext.extend_from_slice(&(sn_list.len() as u16).to_be_bytes());
        ext.extend_from_slice(&sn_list);
        let mut exts = (ext.len() as u16).to_be_bytes().to_vec();
        exts.extend_from_slice(&ext);
        let mut body = vec![0x03, 0x03];
        body.extend_from_slice(&[0u8; 32]);
        body.push(0x00);
        body.extend_from_slice(&0x0002u16.to_be_bytes());
        body.extend_from_slice(&[0x13, 0x01]);
        body.push(0x01);
        body.push(0x00);
        body.extend_from_slice(&exts);
        let mut hs = vec![0x01];
        let blen = body.len();
        hs.extend_from_slice(&[(blen >> 16) as u8, (blen >> 8) as u8, blen as u8]);
        hs.extend_from_slice(&body);
        let mut rec = vec![0x16, 0x03, 0x01];
        rec.extend_from_slice(&(hs.len() as u16).to_be_bytes());
        rec.extend_from_slice(&hs);
        rec
    }

    #[test]
    fn classify_allows_matching_sni() {
        let e = allow("api.example.com", 443);
        let (host, decision) = classify(443, &client_hello("api.example.com"), &e);
        assert_eq!(host.as_deref(), Some("api.example.com"));
        assert_eq!(decision, Decision::Allow);
    }

    #[test]
    fn classify_denies_unlisted_and_no_hostname() {
        let e = allow("api.example.com", 443);
        assert_eq!(
            classify(443, &client_hello("evil.example.com"), &e).1,
            Decision::DenyNoRule
        );
        assert_eq!(classify(443, b"not tls", &e).1, Decision::DenyNoHostname);
    }

    #[test]
    fn classify_handles_http_on_custom_port() {
        // カスタムポートの平文 HTTP も Host からホスト名を取れる（SNI→HTTP のフォールバック）。
        let e = allow("api.example.com", 8080);
        let req = b"GET / HTTP/1.1\r\nHost: api.example.com\r\n\r\n";
        let (host, decision) = classify(8080, req, &e);
        assert_eq!(host.as_deref(), Some("api.example.com"));
        assert_eq!(decision, Decision::Allow);
    }

    #[test]
    fn ssrf_filter_rejects_internal_addresses() {
        use std::net::{Ipv4Addr, Ipv6Addr};
        // 内部/予約レンジは中継禁止。
        for ip in [
            "127.0.0.1",
            "10.0.0.5",
            "192.168.1.1",
            "172.16.0.1",
            "169.254.169.254", // クラウドメタデータ
            "100.64.0.1",      // CGNAT
            "0.0.0.0",
        ] {
            let a: Ipv4Addr = ip.parse().unwrap();
            assert!(is_forbidden_upstream(a.into()), "{ip} must be forbidden");
        }
        // 公開 IP は許可。
        assert!(!is_forbidden_upstream(
            "1.1.1.1".parse::<Ipv4Addr>().unwrap().into()
        ));
        assert!(!is_forbidden_upstream(
            "93.184.216.34".parse::<Ipv4Addr>().unwrap().into()
        ));
        // IPv6 loopback/ULA/リンクローカルは禁止・GUA は許可。
        assert!(is_forbidden_upstream(Ipv6Addr::LOCALHOST.into()));
        assert!(is_forbidden_upstream(
            "fe80::1".parse::<Ipv6Addr>().unwrap().into()
        ));
        assert!(is_forbidden_upstream(
            "fc00::1".parse::<Ipv6Addr>().unwrap().into()
        ));
        assert!(!is_forbidden_upstream(
            "2606:4700:4700::1111".parse::<Ipv6Addr>().unwrap().into()
        ));
        // IPv4-mapped の内部アドレスも弾く。
        assert!(is_forbidden_upstream(
            "::ffff:127.0.0.1".parse::<Ipv6Addr>().unwrap().into()
        ));
    }

    /// プロキシ全体のループバック結合（netns 不要）: 許可 SNI は上流へ中継、拒否 SNI は切断。
    #[tokio::test]
    async fn proxy_relays_allowed_and_blocks_denied() {
        // ループバック upstream を使うため SSRF フィルタをテスト内だけ緩める（本番は未設定）。
        std::env::set_var("SANDBOX_EGRESS_ALLOW_PRIVATE", "1");
        // 上流エコーサーバ。
        let upstream = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let up_addr = upstream.local_addr().unwrap();
        tokio::spawn(async move {
            while let Ok((mut s, _)) = upstream.accept().await {
                tokio::spawn(async move {
                    let mut b = [0u8; 1024];
                    if let Ok(n) = s.read(&mut b).await {
                        let _ = tokio::io::AsyncWriteExt::write_all(&mut s, &b[..n]).await;
                    }
                });
            }
        });

        // upstream のポートを allow に載せ、SNI に "127.0.0.1" を使う（解決不要のループバック）。
        // host が "127.0.0.1" だと connect(("127.0.0.1", port)) が上流に到達する。
        let port = up_addr.port();
        let e = Arc::new(allow("127.0.0.1", port));
        let proxy = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let proxy_addr = proxy.local_addr().unwrap();
        let audit = EgressAudit {
            tenant_id: "t".into(),
            sandbox_id: "s".into(),
        };
        tokio::spawn(serve_port(proxy, port, e, audit));

        // 許可: SNI=127.0.0.1 → 上流エコー。
        let mut c = TcpStream::connect(proxy_addr).await.unwrap();
        let hello = client_hello("127.0.0.1");
        tokio::io::AsyncWriteExt::write_all(&mut c, &hello)
            .await
            .unwrap();
        let mut got = vec![0u8; hello.len()];
        let n = c.read(&mut got).await.unwrap();
        assert_eq!(&got[..n], &hello[..n]);

        // 拒否: SNI=evil → 上流に届かず切断（read 0）。
        let mut d = TcpStream::connect(proxy_addr).await.unwrap();
        let bad = client_hello("evil.test");
        let _ = tokio::io::AsyncWriteExt::write_all(&mut d, &bad).await;
        let mut sink = [0u8; 16];
        let n = tokio::time::timeout(Duration::from_secs(2), d.read(&mut sink))
            .await
            .unwrap_or(Ok(0))
            .unwrap_or(0);
        assert_eq!(n, 0, "denied connection should be closed with no data");
    }
}
