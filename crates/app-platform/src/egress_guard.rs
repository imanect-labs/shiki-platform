//! egress の SSRF 防御（Task 9.12・PIT-23）。
//!
//! `egress_allowlist` はホスト名で許可するが、許可ホストが **内部/メタデータ IP に解決**
//! される場合（攻撃者制御 DNS・DNS リバインディング）を弾く。allowlist 通過後、送信前に
//! 名前解決して全解決先が公開 IP であることを確認する。
//!
//! IP 分類の実体は [`sandbox_client::net_guard`]（単一の正・#348）。web_fetch・egress プロキシ・
//! ミニアプリ HTTP の 3 経路で同じ表を使い、片方だけ緩い穴が空くのを防ぐ。
//!
//! 残存リスク（アルファ）: 検証と実接続の間に DNS 応答が変わる TOCTOU は残る。
//! web_fetch（#348）は検証済みアドレスへ接続を固定して塞いだが、ミニアプリの HTTP は
//! ホスト名のまま送るため、完全遮断には egress プロキシ経由が要る（ポストアルファ）。
//! それでも「許可ホスト→localhost/169.254.169.254」のような典型的 SSRF は本チェックで遮断できる。

use sandbox_client::net_guard;

/// `host:port` を解決し、全ての解決先が公開 IP なら `Ok`、内部/非解決なら拒否理由を返す。
pub(crate) async fn ensure_public_host(host: &str, port: u16) -> Result<(), &'static str> {
    net_guard::ensure_public_host(host, port).await
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn ensure_public_host_rejects_loopback_literal() {
        // リテラル IP は DNS 不要で解決される。
        assert!(ensure_public_host("127.0.0.1", 443).await.is_err());
        assert!(ensure_public_host("169.254.169.254", 80).await.is_err());
        assert!(ensure_public_host("1.1.1.1", 443).await.is_ok());
    }
}
