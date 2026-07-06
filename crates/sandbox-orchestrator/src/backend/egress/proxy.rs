//! ホスト名フィルタリング TCP プロキシ。netns 内で bind したリスナ（ホストランタイムが service）で
//! ゲスト接続を受け、ClientHello SNI / HTTP Host を覗いて allowlist 判定し、許可時のみ実ホストへ中継する。
//!
//! 判定は全件 `target="sandbox_audit"` で監査する（遮断/許可イベントを残す・Task 4.6 受け入れ条件）。

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
#[must_use]
pub fn classify(port: u16, peeked: &[u8], egress: &Egress) -> (Option<String>, Decision) {
    let host = if port == 80 {
        sni::parse_http_host(peeked)
    } else {
        sni::parse_tls_sni(peeked)
    };
    let decision = match &host {
        Some(h) => rules::evaluate(egress, h, port),
        None => Decision::DenyNoHostname,
    };
    (host, decision)
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

/// ClientHello を覗ける程度まで（タイムアウト付きで）先読みする。
async fn peek_initial(client: &TcpStream) -> Vec<u8> {
    let mut buf = vec![0u8; 4096];
    // ClientHello / HTTP リクエスト行は最初のセグメントで来るのが通常。数回だけ peek を試す。
    for _ in 0..5 {
        match tokio::time::timeout(Duration::from_secs(3), client.peek(&mut buf)).await {
            Ok(Ok(0)) => return Vec::new(),
            Ok(Ok(n)) if n >= 64 => {
                buf.truncate(n);
                return buf;
            }
            Ok(Ok(_)) => {
                tokio::time::sleep(Duration::from_millis(20)).await;
            }
            _ => return Vec::new(),
        }
    }
    Vec::new()
}

async fn handle_conn(mut client: TcpStream, port: u16, egress: &Egress, audit: &EgressAudit) {
    let peeked = peek_initial(&client).await;
    let (host, decision) = classify(port, &peeked, egress);
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

    // 実ホストへ接続（ホスト netns のシステムリゾルバで解決）。
    let Ok(Ok(mut upstream)) = tokio::time::timeout(
        Duration::from_secs(10),
        TcpStream::connect((host.as_str(), port)),
    )
    .await
    else {
        tracing::debug!(target: "sandbox_audit", host=%host, %port, "egress upstream connect failed");
        return;
    };
    // peek で消費していないため、client の先頭バイトはそのまま upstream へ流れる。
    let _ = tokio::io::copy_bidirectional(&mut client, &mut upstream).await;
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

    /// プロキシ全体のループバック結合（netns 不要）: 許可 SNI は上流へ中継、拒否 SNI は切断。
    #[tokio::test]
    async fn proxy_relays_allowed_and_blocks_denied() {
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
