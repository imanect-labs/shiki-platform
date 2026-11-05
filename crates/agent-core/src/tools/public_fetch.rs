//! 公開 Web への取得の共通関門（`web_fetch` と回答画像のプロキシ・#1044）。
//!
//! 外部へ HTTP を撃つ経路はここを通す（PIT-48: 「DNS を二度引かない」を 1 箇所で守る）。
//!
//! - [`validate_url`] — アプリ層の一次防壁。http/https 以外・userinfo 付き・IP リテラル・
//!   単一ラベル名・localhost/.local/.internal 等を拒否する。
//! - [`pinned_client`] — 名前解決 → 解決後 IP の検証（[`net_guard::ensure_all_public`]）→
//!   **その検証済みアドレスへ接続を固定した** client を作る。リダイレクトは追従しない
//!   （PIT-36。追従したい呼び出し側は Location を [`validate_url`] から通し直す）。
//!   環境プロキシは経由しない（アドレス固定を迂回させない）。資格情報は載せない。

use std::net::SocketAddr;
use std::time::Duration;

use sandbox_client::net_guard::{self, HostResolver};
use url::{Host, Url};

/// 接続確立の期限（解決後のアドレスへ繋がるまで）。
pub const CONNECT_TIMEOUT: Duration = Duration::from_secs(5);

/// 名前解決の期限（reqwest の timeout は client 構築後にしか効かないため別に掛ける）。
pub const RESOLVE_TIMEOUT: Duration = Duration::from_secs(5);

/// 検証済みの取得先（接続を固定する host/port と正規化済み URL）。
#[derive(Debug, Clone)]
pub struct FetchTarget {
    pub url: Url,
    pub host: String,
    pub port: u16,
}

/// 入力 URL を検証する（モデル/ユーザー/取得先由来＝敵対的として扱う）。
///
/// - スキームは http/https のみ（gopher/file 等を拒否）。
/// - userinfo（`user:pass@`）付きは拒否（ホスト偽装・資格情報混入の防止）。
/// - ホストは **ドットを含む公開 FQDN のみ**: IP リテラル（v4/v6）・単一ラベル名
///   （compose のサービス名 `rustfs` 等）・localhost/.local/.internal/.lan/.home.arpa を拒否。
///   名前が公開 IP に解決されるかは、この後の [`pinned_client`] が判定する。
///
/// # Errors
/// 拒否理由（人が読む短い文言）を返す。
pub fn validate_url(input: &str) -> Result<FetchTarget, String> {
    let url = Url::parse(input.trim()).map_err(|e| e.to_string())?;
    if !matches!(url.scheme(), "http" | "https") {
        return Err("http/https のみ取得できます".into());
    }
    if !url.username().is_empty() || url.password().is_some() {
        return Err("userinfo（user:pass@）付き URL は使えません".into());
    }
    let host = match url.host() {
        Some(Host::Domain(d)) => d.to_ascii_lowercase(),
        Some(Host::Ipv4(_) | Host::Ipv6(_)) => {
            return Err("IP アドレス直指定は使えません".into());
        }
        None => return Err("ホストがありません".into()),
    };
    // 内部/ローカル名を拒否（SSRF・confused-deputy の素地を断つ）。末尾のドット（`localhost.`・
    // `metadata.google.internal.` のような完全修飾の書き方）を落としてから比べる。
    let bare = host.trim_end_matches('.');
    let forbidden_suffixes = [".local", ".internal", ".localhost", ".lan", ".home.arpa"];
    if !bare.contains('.')
        || bare == "localhost"
        || forbidden_suffixes.iter().any(|s| bare.ends_with(s))
    {
        return Err("内部/ローカルホストは取得できません".into());
    }
    let port = url
        .port_or_known_default()
        .ok_or_else(|| "ポートを特定できません".to_string())?;
    Ok(FetchTarget { url, host, port })
}

/// [`pinned_client`] の失敗。呼び出し側が自分の文言へ写す。
#[derive(Debug)]
pub enum PinError {
    /// 名前解決が期限内に終わらなかった。
    ResolveTimeout,
    /// 名前解決に失敗した。
    Resolve(&'static str),
    /// 解決先に公開でないアドレスが含まれた（SSRF の拒否）。
    Rejected(&'static str),
    /// client を作れなかった。
    Client(String),
}

/// 解決後 IP を検証し、検証済みアドレスへ固定した client を作る（**接続先を確定させる唯一の関門**）。
///
/// `guard` は解決結果の検証。本番は [`net_guard::ensure_all_public`] を渡す（[`public_only`]）。
/// テストだけがループバックのスタブへ繋ぐために緩める。
///
/// # Errors
/// 解決の失敗・期限切れ・公開でないアドレス・client 構築の失敗。
pub async fn pinned_client(
    resolver: &dyn HostResolver,
    target: &FetchTarget,
    guard: impl FnOnce(Vec<SocketAddr>) -> Result<Vec<SocketAddr>, &'static str>,
    timeout: Duration,
    user_agent: &str,
) -> Result<reqwest::Client, PinError> {
    let looked_up =
        tokio::time::timeout(RESOLVE_TIMEOUT, resolver.lookup(&target.host, target.port))
            .await
            .map_err(|_| PinError::ResolveTimeout)?
            .map_err(PinError::Resolve)?;
    let addrs = guard(looked_up).map_err(PinError::Rejected)?;
    reqwest::Client::builder()
        .redirect(reqwest::redirect::Policy::none()) // PIT-36
        .no_proxy() // 環境プロキシでアドレス固定を迂回させない
        .connect_timeout(CONNECT_TIMEOUT)
        .timeout(timeout)
        .user_agent(user_agent)
        .resolve_to_addrs(&target.host, &addrs) // ← DNS を引き直させない
        .build()
        .map_err(|e| PinError::Client(e.to_string()))
}

/// 解決後 IP の検証関数の型（本番は [`public_only`]・テストだけがループバックを許す）。
pub type AddrGuard = fn(Vec<SocketAddr>) -> Result<Vec<SocketAddr>, &'static str>;

/// 本番の解決後 IP 検証（全アドレスが公開 IP であること）。
///
/// # Errors
/// 公開でないアドレスが 1 つでも含まれたら拒否する。
pub fn public_only(addrs: Vec<SocketAddr>) -> Result<Vec<SocketAddr>, &'static str> {
    net_guard::ensure_all_public(addrs)
}

/// 本文を `cap` バイトまで読む（上限に達したら以降は受け取らない）。返り値の bool は打ち切ったか。
///
/// # Errors
/// 本文の受信に失敗した（接続断・期限切れ）。
pub async fn read_capped(
    mut response: reqwest::Response,
    cap: usize,
) -> Result<(Vec<u8>, bool), reqwest::Error> {
    let mut body: Vec<u8> = Vec::new();
    while let Some(chunk) = response.chunk().await? {
        let remaining = cap - body.len();
        if chunk.len() >= remaining {
            body.extend_from_slice(&chunk[..remaining]);
            return Ok((body, true));
        }
        body.extend_from_slice(&chunk);
    }
    Ok((body, false))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn accepts_public_fqdn() {
        let t = validate_url("https://cdn.example.com/a.png?x=1").unwrap();
        assert_eq!(t.host, "cdn.example.com");
        assert_eq!(t.port, 443);
    }

    #[test]
    fn rejects_dangerous_inputs() {
        for bad in [
            "file:///etc/passwd",
            "https://user:pw@example.com/",
            "http://127.0.0.1/",
            "http://[::1]/",
            "http://rustfs/",
            "http://localhost/",
            "http://svc.internal/",
            "http://printer.local/",
            "http://localhost./",
            "http://rustfs./",
            "http://metadata.google.internal./",
        ] {
            assert!(validate_url(bad).is_err(), "should reject {bad:?}");
        }
    }

    struct Fixed(Vec<SocketAddr>);
    #[async_trait::async_trait]
    impl HostResolver for Fixed {
        async fn lookup(&self, _: &str, _: u16) -> Result<Vec<SocketAddr>, &'static str> {
            Ok(self.0.clone())
        }
    }

    #[tokio::test]
    async fn pinned_client_rejects_private_resolution() {
        let target = validate_url("https://rebind.example.com/").unwrap();
        let resolver = Fixed(vec!["10.0.0.5:443".parse().unwrap()]);
        let err = pinned_client(&resolver, &target, public_only, CONNECT_TIMEOUT, "t")
            .await
            .unwrap_err();
        assert!(matches!(err, PinError::Rejected(_)), "{err:?}");
    }
}
