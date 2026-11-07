//! 公開 Web への取得の共通関門（`web_fetch` と回答画像のプロキシ・#1044）。
//!
//! 外部へ HTTP を撃つ経路はここを通す（PIT-48: 「DNS を二度引かない」を 1 箇所で守る）。
//!
//! - [`validate_url`] — アプリ層の一次防壁。http/https 以外・userinfo 付き・IP リテラル・
//!   単一ラベル名・localhost/.local/.internal 等を拒否する。
//! - [`resolve_verified`] — 名前解決 → 解決後 IP の検証（[`net_guard::ensure_all_public`]）。
//!   **接続先を確定させる唯一の関門**。
//! - [`connect`] — その検証済みアドレスにだけつなぐ client を作る。運用者が中継（`egress.proxy_url`・
//!   GAP-72）を設定していれば、検証済みの IP へのトンネルを通し、無ければ検証済みアドレスへ接続を固定する。
//!   リダイレクトは追従しない（PIT-36。追従したい呼び出し側は Location を [`validate_url`] から通し直す）。
//!   環境プロキシは経由しない（アドレス固定を迂回させない）。資格情報は載せない。`web_fetch` と回答の画像の
//!   代理取得が、この組み立てを共有する。

use std::net::SocketAddr;
use std::time::Duration;

use sandbox_client::egress_proxy::{self, EgressProxy, ProxyError, ProxyTunnel};
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

/// [`resolve_verified`] の失敗（宛先を確定できない）。呼び出し側が自分の文言へ写す。
#[derive(Debug, Clone, Copy)]
pub enum PinError {
    /// 名前解決が期限内に終わらなかった。
    ResolveTimeout,
    /// 名前解決に失敗した。
    Resolve(&'static str),
    /// 解決先に公開でないアドレスが含まれた（SSRF の拒否）。
    Rejected(&'static str),
}

/// 名前を解決し、解決後 IP を検証する（**接続先を確定させる唯一の関門**）。
///
/// `guard` は解決結果の検証。本番は [`net_guard::ensure_all_public`] を渡す（[`public_only`]）。
/// テストだけがループバックのスタブへ繋ぐために緩める。返したアドレス以外へは繋がない
/// （呼び出し側は [`connect`] にこのアドレスを渡す）。
///
/// # Errors
/// 解決の失敗・期限切れ・公開でないアドレス。
pub async fn resolve_verified(
    resolver: &dyn HostResolver,
    target: &FetchTarget,
    guard: impl FnOnce(Vec<SocketAddr>) -> Result<Vec<SocketAddr>, &'static str>,
) -> Result<Vec<SocketAddr>, PinError> {
    let looked_up =
        tokio::time::timeout(RESOLVE_TIMEOUT, resolver.lookup(&target.host, target.port))
            .await
            .map_err(|_| PinError::ResolveTimeout)?
            .map_err(PinError::Resolve)?;
    guard(looked_up).map_err(PinError::Rejected)
}

/// 公開 Web へ撃つ client の既定の設定（接続先の固定は呼び出し側が足す）。
///
/// リダイレクトは追従しない（PIT-36）。環境プロキシは経由しない（アドレス固定を迂回させない）。
pub fn client_builder(timeout: Duration, user_agent: &str) -> reqwest::ClientBuilder {
    reqwest::Client::builder()
        .redirect(reqwest::redirect::Policy::none()) // PIT-36
        .no_proxy() // 環境プロキシでアドレス固定を迂回させない
        .connect_timeout(CONNECT_TIMEOUT)
        .timeout(timeout)
        .user_agent(user_agent)
}

/// [`connect`] の失敗。
#[derive(Debug)]
pub enum ConnectError {
    /// 中継（`egress.proxy_url`）のトンネルを開けなかった。[`ProxyError::message`] は中継の場所と資格情報を含まない。
    Proxy(ProxyError),
    /// client を作れなかった。
    Client(String),
}

/// 検証済みのアドレス（[`resolve_verified`] の結果）にだけつなぐ client を作る。
///
/// 運用者が中継（`egress.proxy_url`・GAP-72）を設定していて、アドレスがすべて公開の IP なら
/// （[`egress_proxy::via_proxy`]）、そのアドレスへのトンネルを開き、client はトンネルの口にだけつなぐ（中継には
/// 検証済みの IP への CONNECT を頼み、名前を渡さない。TLS はトンネルの中で URL の名前で結ぶ）。それ以外は、検証済みの
/// アドレスへ接続を固定する（DNS を引き直させない）。トンネルは取得を終えるまで持つ（落とすと閉じる・最初の 1 本の
/// 接続だけを受けるので、リダイレクトを追うときは宛先ごとに作り直す）。
///
/// # Errors
/// 中継のトンネルを開けない・client 構築の失敗。
pub async fn connect(
    target: &FetchTarget,
    addrs: &[SocketAddr],
    proxy: Option<&EgressProxy>,
    timeout: Duration,
    user_agent: &str,
) -> Result<(reqwest::Client, Option<ProxyTunnel>), ConnectError> {
    let tunnel = match proxy {
        Some(proxy) if egress_proxy::via_proxy(addrs) => {
            Some(proxy.open(addrs).await.map_err(ConnectError::Proxy)?)
        }
        _ => None,
    };
    let builder = client_builder(timeout, user_agent);
    let builder = match &tunnel {
        // トンネルの口にだけ接続する（中継には検証済みの IP への CONNECT を頼んである）。
        Some(tunnel) => builder.unix_socket(tunnel.socket_path()),
        None => builder.resolve_to_addrs(&target.host, addrs), // ← DNS を引き直させない
    };
    let client = builder
        .build()
        .map_err(|e| ConnectError::Client(e.to_string()))?;
    Ok((client, tunnel))
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
    async fn resolve_verified_rejects_private_resolution() {
        let target = validate_url("https://rebind.example.com/").unwrap();
        let resolver = Fixed(vec!["10.0.0.5:443".parse().unwrap()]);
        let err = resolve_verified(&resolver, &target, public_only)
            .await
            .unwrap_err();
        assert!(matches!(err, PinError::Rejected(_)), "{err:?}");
    }

    /// 中継を書いていても、内部のアドレス（管理者が許した社内の送り先）は中継を通さずに直接つなぐ。中継へは
    /// つなぎに行かない（閉じたポートの中継でも失敗しない）。
    #[tokio::test]
    async fn internal_addresses_do_not_use_the_proxy() {
        let closed = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let proxy_addr = closed.local_addr().unwrap();
        drop(closed);
        let proxy = EgressProxy::new(&format!("http://{proxy_addr}"), None).unwrap();
        let target = validate_url("https://intra.example.com/").unwrap();
        let addrs = ["10.0.0.5:443".parse().unwrap()];
        let (_, tunnel) = connect(&target, &addrs, Some(&proxy), CONNECT_TIMEOUT, "t")
            .await
            .unwrap();
        assert!(tunnel.is_none());
        // 公開の IP なら中継へつなぎに行き、つなげなければ中継の失敗として返す。
        let public = ["93.184.216.34:443".parse().unwrap()];
        let err = connect(&target, &public, Some(&proxy), CONNECT_TIMEOUT, "t")
            .await
            .unwrap_err();
        assert!(
            matches!(err, ConnectError::Proxy(ProxyError::Unreachable)),
            "{err:?}"
        );
    }
}
