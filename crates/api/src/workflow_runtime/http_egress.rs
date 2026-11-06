//! ワークフローの外部への送信（「外部 API 呼び出し」ブロックと script の `Shiki.http.request`）の送り口
//! （BUG-181・PIT-36 の ③・PIT-48・D-135）。
//!
//! 送り先の名前の照合（管理者の許可 `workflow.http_allowlist` × シークレットの宛先束縛）は、エンジンが URL の
//! ホスト部の文字で済ませてから呼ぶ（`nodes/capability_ai.rs` の `check_http_destination`）。ここでは名前解決の
//! 後のアドレスを確かめてから送る。
//!
//! 1. 名前解決はここで 1 回だけ行い、確かめたアドレスに接続を固定する（[`PinnedResolver`]。HTTP の部品には
//!    名前を引き直させない。検証と接続のあいだに解決先を付け替える DNS リバインディングを塞ぐ）。
//! 2. 解決先に内部の IP が 1 つでもあれば、応答ごと断る（公開と内部の混在も断る）。IP の分類は
//!    `sandbox_client::net_guard` が単一の正（web.fetch・ミニアプリ・サンドボックスと同じ表）。
//!    - 管理者がデプロイ設定 `workflow.http_internal_hosts` に書いたホスト（ポートを書けばそのポートだけ）にだけ、
//!      内部の IP への送信を許す（オンプレの社内システムを呼ぶ使い方・D-135）。送れる先の許可（`http_allowlist`）とは
//!      分ける: 許可は公開の SaaS を書く場所でもあり、そこに書いたホストまで内部へつながると、DNS を持つ人が解決先を
//!      内部へ向けるだけで内部の任意のポートへ届く（PIT-48 の ②が効かなくなる）。ワイルドカードは書けない。
//!      シークレットの宛先束縛だけでも許さない（宛先束縛はシークレットを作る人が決められる）。
//!    - リンクローカル（169.254.0.0/16・fe80::/10）とクラウドのメタデータのアドレスは、許可にかかわらず断る。
//! 3. 3xx は追わない（`redirect::Policy::none`。エンジンが `redirect_denied` で失敗にする）。
//! 4. 環境のプロキシを使わない（`no_proxy`。プロキシ経由だと接続の固定が効かない）。外へ出るのに中継が要る環境では、
//!    運用者が設定 `egress.proxy_url` に書いた中継だけを通す（GAP-72）。中継には確かめた IP アドレスへの CONNECT を
//!    頼み（名前を渡さない）、TLS はトンネルの中で URL の名前で結ぶ（`sandbox_client::egress_proxy`）。社内の送り先
//!    （内部の IP を含むアドレス）は中継を通さずに直接つなぐ。中継が断った・つなげなかったときは、送り先に何も
//!    送っていないことを言う（断られたのは [`EGRESS_PROXY_REFUSED`]・やり直さない。ほかは `unavailable`）。
//! 5. 本文は上限（8 MB）まで読み、超えた分は読まない。
//!
//! 断ったときは、作成者向けの文で [`DESTINATION_INTERNAL`]・[`DESTINATION_LINK_LOCAL`] の失敗にする（やり直さない）。
//! 名前のアドレスが無い（NXDOMAIN など）ときは [`DESTINATION_NOT_FOUND`]（URL の誤り・やり直さない）、DNS に届かない・
//! 一時的に引けないときは `unavailable`（やり直せる）にする（BUG-202）。
//! 監査はエンジンが code を見て残す（送り先のホストと理由）。

use std::net::SocketAddr;
use std::sync::Arc;
use std::time::Duration;

use sandbox_client::egress_proxy::{self, EgressProxy, ProxyError, ProxyTunnel};
use sandbox_client::internal_hosts::InternalHosts;
use sandbox_client::net_guard::{self, AddrRefusal, HostResolver, SystemResolver};
use workflow_engine::nodes::http::{
    DESTINATION_INTERNAL, DESTINATION_LINK_LOCAL, DESTINATION_NOT_FOUND, EGRESS_PROXY_REFUSED,
};
use workflow_engine::{HttpSendReq, HttpSendResp, PortError};

/// 応答の本文を読む上限（バイト）。超えた分は読まずに打ち切る（巨大な応答の全量を持たない）。
const MAX_BODY: usize = 8 * 1024 * 1024;

/// 名前解決の上限（HTTP の時間の上限は接続の後にしか効かないので、解決にも別に掛ける）。
const RESOLVE_TIMEOUT: Duration = Duration::from_secs(5);

/// 内部の送り先の設定の名前（読めない項目の文に出す）。
const INTERNAL_HOSTS_SETTING: &str = "workflow.http_internal_hosts";

/// ワークフローの外部への送り口（管理者が内部の宛先を許したホストと、名前解決の注入点を持つ）。
pub struct HttpEgress {
    /// 内部の IP への送信を許すホスト（`workflow.http_internal_hosts`。読めない項目は除く）。
    internal_hosts: InternalHosts,
    /// 名前解決（既定は OS の DNS。テストで解決先を決める）。
    resolver: Arc<dyn HostResolver>,
    /// 本文を読む上限（既定 [`MAX_BODY`]）。
    max_body: usize,
    /// 運用者が設定した外への送信の中継（`egress.proxy_url`・無ければ直接つなぐ・GAP-72）。
    proxy: Option<EgressProxy>,
}

impl Default for HttpEgress {
    fn default() -> Self {
        Self::new(&[])
    }
}

impl HttpEgress {
    /// 管理者が内部の送り先として許したホスト（`workflow.http_internal_hosts`）から組む。
    ///
    /// 読めない項目（ワイルドカードなど）は内部の宛先を許さない（起動時の設定の確かめ [`check_internal_hosts`] が
    /// 先に断る）。
    #[must_use]
    pub fn new(internal_hosts: &[String]) -> Self {
        HttpEgress {
            internal_hosts: InternalHosts::lenient(internal_hosts),
            resolver: Arc::new(SystemResolver),
            max_body: MAX_BODY,
            proxy: None,
        }
    }

    /// 外への送信の中継を使う（`egress.proxy_url`・GAP-72）。社内の送り先は中継を通さない。
    #[must_use]
    pub fn with_proxy(mut self, proxy: Option<EgressProxy>) -> Self {
        self.proxy = proxy;
        self
    }

    /// 外への送信の中継（web.fetch も同じ中継を使う）。
    #[must_use]
    pub fn proxy(&self) -> Option<&EgressProxy> {
        self.proxy.as_ref()
    }

    /// 名前解決を差し替える（テストで DNS リバインディング・内部への解決を再現する）。
    #[must_use]
    pub fn with_resolver(mut self, resolver: Arc<dyn HostResolver>) -> Self {
        self.resolver = resolver;
        self
    }

    #[cfg(test)]
    fn with_max_body(mut self, max_body: usize) -> Self {
        self.max_body = max_body;
        self
    }

    /// 送り先を確かめて送り、応答を本文の上限まで読む。
    pub async fn send(&self, req: HttpSendReq) -> Result<HttpSendResp, PortError> {
        let url = url::Url::parse(&req.url).map_err(|_| {
            PortError::invalid(
                "URL の書き方が正しくありません。https:// から、ドメインまで書いてください。",
            )
        })?;
        let shown = url.host_str().unwrap_or_default().to_string();
        let (domain, addrs) = self.resolve(&url, &shown).await?;
        // 外へ出るのに中継が要る環境では、確かめたアドレスへのトンネルを先に開く（送信を終えるまで持つ・GAP-72）。
        let tunnel = self.open_tunnel(&addrs, &shown).await?;
        let mut builder = reqwest::Client::builder()
            // 3xx は追わない（エンジンが redirect_denied にする・PIT-36）。
            .redirect(reqwest::redirect::Policy::none())
            // 環境のプロキシ（HTTPS_PROXY など）で接続の固定を迂回させない（PIT-48 の ④）。
            .no_proxy();
        if let Some(tunnel) = &tunnel {
            // トンネルの口にだけ接続する（中継には確かめた IP への CONNECT を頼んである。TLS は URL の名前で結ぶ）。
            builder = builder.unix_socket(tunnel.socket_path());
        } else if let Some(domain) = domain {
            // 確かめたアドレスにだけ接続する（HTTP の部品に名前を引き直させない）。
            builder = builder.dns_resolver(Arc::new(PinnedResolver {
                host: domain,
                addrs,
            }));
        }
        let client = builder.build().map_err(|_| {
            PortError::unavailable(
                "外部のサービスに送る準備ができませんでした。少し待ってから、もう一度実行してください。",
            )
        })?;
        let method = reqwest::Method::from_bytes(req.method.as_bytes())
            .map_err(|_| PortError::invalid("送り方（メソッド）が正しくありません。"))?;
        let mut request = client.request(method, url);
        for (k, v) in &req.headers {
            request = request.header(k, v);
        }
        if let Some(body) = req.body {
            request = request.body(body);
        }
        if let Some(ms) = req.timeout_ms {
            request = request.timeout(Duration::from_millis(ms));
        }
        let mut resp = request.send().await.map_err(|_| {
            PortError::unavailable(format!(
                "送り先のサービス（{shown}）に接続できませんでした。途中まで送れていることがあるので、送り先の状態を\
                 確かめてから、もう一度実行してください。"
            ))
        })?;
        let status = resp.status().as_u16();
        // 本文は上限まで**ストリーミング読み**する（巨大レスポンスの全量バッファで OOM しない）。
        let mut body = Vec::new();
        while let Some(chunk) = resp.chunk().await.map_err(|_| {
            PortError::unavailable(
                "応答を最後まで受け取れませんでした。送れていることがあるので、送り先の状態を確かめてください。",
            )
        })? {
            if body.len() + chunk.len() > self.max_body {
                body.extend_from_slice(&chunk[..self.max_body - body.len()]);
                break;
            }
            body.extend_from_slice(&chunk);
        }
        Ok(HttpSendResp { status, body })
    }

    /// 中継を使う設定で、確かめたアドレスがすべて公開の IP なら、中継にトンネルを開かせる（社内の送り先は直接つなぐ）。
    async fn open_tunnel(
        &self,
        addrs: &[SocketAddr],
        shown: &str,
    ) -> Result<Option<ProxyTunnel>, PortError> {
        let Some(proxy) = &self.proxy else {
            return Ok(None);
        };
        if !egress_proxy::via_proxy(addrs) {
            return Ok(None);
        }
        proxy.open(addrs).await.map(Some).map_err(|e| {
            // 管理者向けのログ（中継の場所は host:port だけ・資格情報は出さない）。
            tracing::warn!(proxy = %proxy.endpoint(), host = %shown, reason = ?e,
                "ワークフローの外部への送信で、中継のトンネルを開けませんでした");
            proxy_failure(e)
        })
    }

    /// URL のホストを 1 回だけ解決し、送ってよいアドレスかを確かめる。名前のホストは（名前・アドレス）を返す。
    async fn resolve(
        &self,
        url: &url::Url,
        shown: &str,
    ) -> Result<(Option<String>, Vec<SocketAddr>), PortError> {
        let port = url
            .port_or_known_default()
            .ok_or_else(|| PortError::invalid("URL は http:// か https:// で始めてください。"))?;
        // IP の書き方（10 進・8 進・16 進の IPv4、IPv4 を写した IPv6）は、URL を読んだ時点で IP にそろう。
        let (domain, resolved) =
            match url.host() {
                Some(url::Host::Domain(d)) => {
                    let found =
                        match tokio::time::timeout(RESOLVE_TIMEOUT, self.resolver.lookup(d, port))
                            .await
                        {
                            Ok(Ok(addrs)) => addrs,
                            // その名前のアドレスが無い（URL のドメインの誤り）。やり直しても直らない（BUG-202）。
                            Ok(Err(reason)) if reason == net_guard::NO_SUCH_HOST => {
                                return Err(not_found(shown))
                            }
                            // DNS に届かない・一時的に引けない・時間切れ（やり直せる・下の Unresolved）。
                            Ok(Err(_)) | Err(_) => Vec::new(),
                        };
                    (Some(d.to_string()), found)
                }
                Some(url::Host::Ipv4(ip)) => (None, vec![SocketAddr::new(ip.into(), port)]),
                Some(url::Host::Ipv6(ip)) => (None, vec![SocketAddr::new(ip.into(), port)]),
                None => {
                    return Err(PortError::invalid(
                        "URL にドメインがありません。例: https://api.example.com",
                    ))
                }
            };
        let seen = resolved.clone();
        let allow_internal = url
            .host()
            .is_some_and(|host| self.internal_hosts.allows(&host, port));
        match net_guard::check_addrs(resolved, allow_internal) {
            Ok(addrs) => Ok((domain, addrs)),
            Err(refusal) => {
                // 管理者向けのログ（作成者向けの文と監査にはアドレスを載せない・内部の名前とアドレスの対応を漏らさない）。
                tracing::warn!(host = %shown, addrs = ?seen, reason = ?refusal, "ワークフローの外部への送信を断りました");
                Err(refused(shown, refusal))
            }
        }
    }
}

/// 中継のトンネルを開けなかったときの作成者向けの文（送り先には何も送っていない・GAP-72）。
fn proxy_failure(e: ProxyError) -> PortError {
    let what = e.message();
    if e.is_retryable() {
        PortError::unavailable(format!(
            "{what}送り先には何も送っていないので、少し待ってから、もう一度実行してください。続くときは、\
             管理者に知らせてください。"
        ))
    } else {
        PortError::new(
            EGRESS_PROXY_REFUSED,
            format!("{what}送り先には何も送っていません。管理者に、中継の設定を確かめるよう頼んでください。"),
            false,
        )
    }
}

/// 送り先の名前のアドレスが無いときの作成者向けの文（[`DESTINATION_NOT_FOUND`]・やり直さない・BUG-202）。
fn not_found(host: &str) -> PortError {
    PortError::new(
        DESTINATION_NOT_FOUND,
        format!("送り先のサービス（{host}）が見つかりませんでした。URL のドメインに誤りがないか確かめてください。"),
        false,
    )
}

/// 断った理由を作成者向けの文にする（どれもやり直さない。解決できないときだけやり直せる）。
fn refused(host: &str, refusal: AddrRefusal) -> PortError {
    match refusal {
        // 名前が無いと分かったときは [`not_found`]。ここは DNS に届かない・一時的に引けない・時間切れ。
        AddrRefusal::Unresolved => PortError::unavailable(format!(
            "送り先のサービス（{host}）の場所を調べられませんでした。少し待ってから、もう一度実行してください。\
             続くときは、URL のドメインを確かめてください。"
        )),
        AddrRefusal::Internal => PortError::new(
            DESTINATION_INTERNAL,
            format!(
                "送り先 {host} は内部のネットワークにつながるため、送るのをやめました。社内のサービスに送るには、\
                 管理者に、この送り先を社内の送り先として登録するよう頼んでください。"
            ),
            false,
        ),
        AddrRefusal::LinkLocalOrMetadata => PortError::new(
            DESTINATION_LINK_LOCAL,
            format!(
                "送り先 {host} は、サーバーの設定を読み出せる特別なアドレスにつながるため、送れません。\
                 管理者の許可があっても送れないので、URL を直してください。"
            ),
            false,
        ),
    }
}

/// 起動時に `workflow.http_internal_hosts` を確かめる（読めない項目があれば、その文を返して起動を止める）。
///
/// 黙って外すと、管理者は社内の送り先を書いたつもりで、実行のときに断られる理由が分からない。
pub fn check_internal_hosts(entries: &[String]) -> Result<(), String> {
    InternalHosts::parse(INTERNAL_HOSTS_SETTING, entries).map(|_| ())
}

/// 確かめたアドレスだけを返す名前解決（ほかの名前は引かない）。
struct PinnedResolver {
    host: String,
    addrs: Vec<SocketAddr>,
}

impl reqwest::dns::Resolve for PinnedResolver {
    fn resolve(&self, name: reqwest::dns::Name) -> reqwest::dns::Resolving {
        let found = name
            .as_str()
            .eq_ignore_ascii_case(&self.host)
            .then(|| self.addrs.clone());
        Box::pin(async move {
            match found {
                Some(addrs) => Ok(Box::new(addrs.into_iter()) as reqwest::dns::Addrs),
                None => Err("確かめていない名前は引きません".into()),
            }
        })
    }
}

#[cfg(test)]
#[path = "http_egress_tests.rs"]
mod tests;
