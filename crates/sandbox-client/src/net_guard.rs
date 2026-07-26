//! egress 宛先の IP 分類（SSRF/DNS リバインディング防御の単一の正・issue #348・PIT-23）。
//!
//! 「許可ホスト名が内部 IP に解決される」型の SSRF を、**名前解決の結果**を見て弾く。
//! ホスト名の許可（アプリ層）とは別レイヤで、`169.254.169.254`（IMDS）・`127.0.0.1`・
//! compose 網内の private 帯などを一律で拒否する。
//!
//! 呼び出し側は [`resolve_public_addrs`] が返した**検証済みアドレスへ直接接続する**こと
//! （名前で再解決しない）。検証と接続で解決結果が変わる DNS リバインディングは、
//! アドレスを固定して初めて塞がる。

use std::net::{IpAddr, SocketAddr};

/// グローバルにルーティングされ得る IP か（内部/予約は false）。
pub fn is_public_ip(ip: IpAddr) -> bool {
    match ip {
        IpAddr::V4(v4) => {
            let o = v4.octets();
            !(v4.is_loopback()
                || v4.is_private()
                || v4.is_link_local() // 169.254/16（メタデータ 169.254.169.254 を含む）
                || v4.is_unspecified()
                || v4.is_broadcast()
                || v4.is_documentation()
                || v4.is_multicast()
                || o[0] == 0
                || (o[0] == 100 && (o[1] & 0xc0) == 0x40) // CGNAT 100.64/10
                || (o[0] == 198 && (o[1] & 0xfe) == 18) // ベンチマーク 198.18/15
                || o[0] >= 240) // 予約 240/4
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
                || (s[0] & 0xffc0) == 0xfe80 // link-local fe80::/10
                || (s[0] == 0x2001 && s[1] == 0x0db8)) // ドキュメント 2001:db8::/32
        }
    }
}

/// 解決結果を検証する（**1 つでも内部 IP を含めば全体を拒否**）。
///
/// 選り分けて公開 IP だけに繋ぐ実装にしない: 公開と内部が混在する応答は攻撃者制御 DNS の
/// 典型であり、後続の再解決で内部側を掴む余地を残すため、応答ごと捨てるのが正しい。
pub fn ensure_all_public(addrs: Vec<SocketAddr>) -> Result<Vec<SocketAddr>, &'static str> {
    if addrs.is_empty() {
        return Err("ホストを解決できませんでした");
    }
    if addrs.iter().any(|a| !is_public_ip(a.ip())) {
        return Err("内部/非グローバル IP へ解決されました");
    }
    Ok(addrs)
}

/// 名前解決の注入点（既定は [`SystemResolver`]。テストで DNS リバインディングを再現する）。
#[async_trait::async_trait]
pub trait HostResolver: Send + Sync {
    /// `host:port` の解決結果を**素のまま**返す（公開 IP 判定は呼び出し側が [`ensure_all_public`] で行う）。
    async fn lookup(&self, host: &str, port: u16) -> Result<Vec<SocketAddr>, &'static str>;
}

/// ホスト OS の DNS を引く既定リゾルバ。
pub struct SystemResolver;

#[async_trait::async_trait]
impl HostResolver for SystemResolver {
    async fn lookup(&self, host: &str, port: u16) -> Result<Vec<SocketAddr>, &'static str> {
        Ok(tokio::net::lookup_host((host, port))
            .await
            .map_err(|_| "ホストを解決できませんでした")?
            .collect())
    }
}

/// `host:port` を解決し、**全ての**解決先が公開 IP のときだけ検証済みアドレス列を返す。
pub async fn resolve_public_addrs(host: &str, port: u16) -> Result<Vec<SocketAddr>, &'static str> {
    ensure_all_public(SystemResolver.lookup(host, port).await?)
}

/// `host:port` の解決先が全て公開 IP か（アドレスを使わない呼び出し側向け）。
pub async fn ensure_public_host(host: &str, port: u16) -> Result<(), &'static str> {
    resolve_public_addrs(host, port).await.map(|_| ())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::net::{Ipv4Addr, Ipv6Addr};

    #[test]
    fn classifies_public_and_internal_ips() {
        assert!(is_public_ip(IpAddr::V4(Ipv4Addr::new(8, 8, 8, 8))));
        assert!(is_public_ip(IpAddr::V4(Ipv4Addr::new(1, 1, 1, 1))));
        for internal in [
            Ipv4Addr::LOCALHOST,
            Ipv4Addr::new(10, 0, 0, 1),
            Ipv4Addr::new(192, 168, 1, 1),
            Ipv4Addr::new(172, 16, 0, 1),
            Ipv4Addr::new(169, 254, 169, 254), // クラウドメタデータ（SSRF 頻出）
            Ipv4Addr::new(100, 64, 0, 1),      // CGNAT
            Ipv4Addr::UNSPECIFIED,
            Ipv4Addr::new(198, 18, 0, 1), // ベンチマーク
            Ipv4Addr::new(240, 0, 0, 1),  // 予約
        ] {
            assert!(!is_public_ip(IpAddr::V4(internal)), "{internal} は内部扱い");
        }
        // IPv6: ループバック・ULA・link-local・IPv4-mapped の内部アドレス。
        assert!(!is_public_ip(IpAddr::V6(Ipv6Addr::LOCALHOST)));
        assert!(!is_public_ip(IpAddr::V6(Ipv6Addr::new(
            0xfd00, 0, 0, 0, 0, 0, 0, 1
        ))));
        assert!(!is_public_ip(IpAddr::V6(Ipv6Addr::new(
            0xfe80, 0, 0, 0, 0, 0, 0, 1
        ))));
        assert!(!is_public_ip(IpAddr::V6(
            "::ffff:127.0.0.1".parse::<Ipv6Addr>().unwrap()
        )));
        assert!(is_public_ip(IpAddr::V6(Ipv6Addr::new(
            0x2606, 0x4700, 0, 0, 0, 0, 0, 1
        ))));
    }

    #[tokio::test]
    async fn localhost_resolution_is_rejected() {
        // ループバックへ解決する名前は拒否される（DNS リバインディングの典型）。
        assert!(resolve_public_addrs("localhost", 80).await.is_err());
        assert!(ensure_public_host("localhost", 80).await.is_err());
    }
}
