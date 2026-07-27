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
            // v6 に **埋め込まれた v4** は v4 の表で判定する（そうしないと `::ffff:169.254.169.254`
            // 系の変種が素通りする）。mapped だけでなく compatible/6to4/NAT64 も同じ穴になる。
            if let Some(v4) = embedded_ipv4(v6) {
                return is_public_ip(IpAddr::V4(v4));
            }
            let s = v6.segments();
            !(v6.is_loopback()
                || v6.is_unspecified()
                || v6.is_multicast()
                || (s[0] & 0xfe00) == 0xfc00 // ULA fc00::/7
                || (s[0] & 0xffc0) == 0xfe80 // link-local fe80::/10
                || (s[0] & 0xffc0) == 0xfec0 // site-local fec0::/10（廃止済みだが安全側で拒否）
                || (s[0] == 0x2001 && s[1] == 0x0db8)) // ドキュメント 2001:db8::/32
        }
    }
}

/// IPv6 アドレスに埋め込まれた IPv4 を取り出す（無ければ `None`）。
///
/// 対象: IPv4-mapped `::ffff:a.b.c.d` / IPv4-compatible `::a.b.c.d` /
/// 6to4 `2002:AABB:CCDD::/48`（上位 32bit が v4）/ NAT64 well-known prefix `64:ff9b::/96`。
/// いずれも「v6 の顔をした v4 宛先」であり、v4 側の内部レンジ判定を通さないと防御が抜ける。
fn embedded_ipv4(v6: std::net::Ipv6Addr) -> Option<std::net::Ipv4Addr> {
    if let Some(mapped) = v6.to_ipv4_mapped() {
        return Some(mapped);
    }
    let s = v6.segments();
    // 6to4: 2002:<v4 上位16>:<v4 下位16>::/48
    if s[0] == 0x2002 {
        return Some(std::net::Ipv4Addr::from(
            (u32::from(s[1]) << 16) | u32::from(s[2]),
        ));
    }
    // NAT64 well-known prefix: 64:ff9b::/96（末尾 32bit が v4）
    if s[0] == 0x0064 && s[1] == 0xff9b && s[2..6] == [0, 0, 0, 0] {
        return Some(std::net::Ipv4Addr::from(
            (u32::from(s[6]) << 16) | u32::from(s[7]),
        ));
    }
    // IPv4-compatible `::a.b.c.d`（`::` と `::1` は上の loopback/unspecified で扱う）
    if s[..6] == [0, 0, 0, 0, 0, 0] && (u32::from(s[6]) << 16 | u32::from(s[7])) > 1 {
        return Some(std::net::Ipv4Addr::from(
            (u32::from(s[6]) << 16) | u32::from(s[7]),
        ));
    }
    None
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

    /// **v6 の顔をした v4** は v4 の表で弾く（mapped 以外の埋め込みも塞ぐ）。
    #[test]
    fn embedded_ipv4_forms_are_classified_by_v4_table() {
        for internal in [
            "::ffff:169.254.169.254", // IPv4-mapped（メタデータ）
            "::ffff:127.0.0.1",
            "2002:a9fe:a9fe::1",  // 6to4 → 169.254.169.254
            "2002:7f00:0001::1",  // 6to4 → 127.0.0.1
            "64:ff9b::a9fe:a9fe", // NAT64 → 169.254.169.254
            "64:ff9b::a00:1",     // NAT64 → 10.0.0.1
            "::10.0.0.1",         // IPv4-compatible
        ] {
            let ip: Ipv6Addr = internal.parse().unwrap();
            assert!(!is_public_ip(IpAddr::V6(ip)), "{internal} は内部扱い");
        }
        // 埋め込み先が公開 IP なら通す（過剰拒否しない）。
        for public in ["::ffff:8.8.8.8", "2002:0808:0808::1", "64:ff9b::808:808"] {
            let ip: Ipv6Addr = public.parse().unwrap();
            assert!(is_public_ip(IpAddr::V6(ip)), "{public} は公開扱い");
        }
    }

    /// site-local fec0::/10 は廃止済みだが、内部網で使われ得るので拒否する。
    #[test]
    fn site_local_v6_is_rejected() {
        assert!(!is_public_ip(IpAddr::V6(
            "fec0::1".parse::<Ipv6Addr>().unwrap()
        )));
        assert!(!is_public_ip(IpAddr::V6(
            "feff::1".parse::<Ipv6Addr>().unwrap()
        )));
    }

    #[tokio::test]
    async fn localhost_resolution_is_rejected() {
        // ループバックへ解決する名前は拒否される（DNS リバインディングの典型）。
        assert!(resolve_public_addrs("localhost", 80).await.is_err());
        assert!(ensure_public_host("localhost", 80).await.is_err());
    }
}
