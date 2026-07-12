//! egress の SSRF 防御（Task 9.12・PIT-23）。
//!
//! `egress_allowlist` はホスト名で許可するが、許可ホストが **内部/メタデータ IP に解決**
//! される場合（攻撃者制御 DNS・DNS リバインディング）を弾く。allowlist 通過後、送信前に
//! 名前解決して全解決先が公開 IP であることを確認する。
//!
//! 残存リスク（アルファ）: 検証と実接続の間に DNS 応答が変わる TOCTOU は残る（完全遮断は
//! egress プロキシ経由が必要・ポストアルファ）。それでも「許可ホスト→localhost/169.254.169.254」
//! のような典型的 SSRF は本チェックで遮断できる。

use std::net::IpAddr;

/// `host:port` を解決し、全ての解決先が公開 IP なら `Ok`、内部/非解決なら拒否理由を返す。
pub(crate) async fn ensure_public_host(host: &str, port: u16) -> Result<(), &'static str> {
    let addrs = tokio::net::lookup_host((host, port))
        .await
        .map_err(|_| "ホストを解決できませんでした")?;
    let mut resolved = false;
    for addr in addrs {
        resolved = true;
        if !is_public_ip(addr.ip()) {
            return Err("内部/非グローバル IP へ解決されました");
        }
    }
    if resolved {
        Ok(())
    } else {
        Err("ホストを解決できませんでした")
    }
}

/// グローバルにルーティングされ得る IP か（内部/予約は false）。
fn is_public_ip(ip: IpAddr) -> bool {
    match ip {
        IpAddr::V4(v4) => {
            let o = v4.octets();
            !(v4.is_loopback()
                || v4.is_private()
                || v4.is_link_local() // 169.254/16（メタデータ 169.254.169.254 を含む）
                || v4.is_unspecified()
                || v4.is_broadcast()
                || v4.is_documentation()
                || o[0] == 0
                || (o[0] == 100 && (o[1] & 0xc0) == 0x40)) // CGNAT 100.64/10
        }
        IpAddr::V6(v6) => {
            if let Some(mapped) = v6.to_ipv4_mapped() {
                return is_public_ip(IpAddr::V4(mapped));
            }
            let s = v6.segments();
            !(v6.is_loopback()
                || v6.is_unspecified()
                || v6.is_multicast()
                || (s[0] & 0xfe00) == 0xfc00 // ULA fc00::/7
                || (s[0] & 0xffc0) == 0xfe80) // link-local fe80::/10
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::net::{Ipv4Addr, Ipv6Addr};

    #[test]
    fn classifies_public_and_internal_ips() {
        assert!(is_public_ip(IpAddr::V4(Ipv4Addr::new(8, 8, 8, 8))));
        assert!(is_public_ip(IpAddr::V4(Ipv4Addr::new(1, 1, 1, 1))));
        // 内部/予約はすべて非公開扱い。
        assert!(!is_public_ip(IpAddr::V4(Ipv4Addr::LOCALHOST)));
        assert!(!is_public_ip(IpAddr::V4(Ipv4Addr::new(10, 0, 0, 1))));
        assert!(!is_public_ip(IpAddr::V4(Ipv4Addr::new(192, 168, 1, 1))));
        assert!(!is_public_ip(IpAddr::V4(Ipv4Addr::new(172, 16, 0, 1))));
        // クラウドメタデータ（SSRF 頻出）。
        assert!(!is_public_ip(IpAddr::V4(Ipv4Addr::new(169, 254, 169, 254))));
        assert!(!is_public_ip(IpAddr::V4(Ipv4Addr::new(100, 64, 0, 1)))); // CGNAT
        assert!(!is_public_ip(IpAddr::V6(Ipv6Addr::LOCALHOST)));
        assert!(!is_public_ip(IpAddr::V6("fc00::1".parse().unwrap()))); // ULA
        assert!(is_public_ip(IpAddr::V6(
            "2606:4700:4700::1111".parse().unwrap()
        )));
    }

    #[tokio::test]
    async fn ensure_public_host_rejects_loopback_literal() {
        // リテラル IP は DNS 不要で解決される。
        assert!(ensure_public_host("127.0.0.1", 443).await.is_err());
        assert!(ensure_public_host("169.254.169.254", 80).await.is_err());
        assert!(ensure_public_host("1.1.1.1", 443).await.is_ok());
    }
}
