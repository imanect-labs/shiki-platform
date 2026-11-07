//! 運用者が設定で明示した外への送信の中継（HTTP の CONNECT・GAP-72）。
//!
//! 外へ出るのに中継（フォワードプロキシ）が要る環境で、ワークフローの外部への送信・web.fetch・回答の画像の取得・
//! ミニアプリの関数の外部 HTTP を中継に通す。宛先の確かめ（PIT-48）は弱めない:
//!
//! 1. 名前解決と IP の確かめは、これまでどおり呼び出し側が 1 回だけ行う（[`crate::net_guard`]）。中継を通すのは、
//!    確かめたアドレスがすべて公開の IP のときだけ（[`via_proxy`]）。管理者が内部の送り先として許したホストは、
//!    中継を通さずに直接つなぐ。
//! 2. 中継には、確かめた **IP アドレス** への CONNECT を頼む（名前を渡さない）。中継が名前を引き直して別のアドレスへ
//!    つなぐ余地（DNS リバインディング）を作らない。TLS はトンネルの中で送り先と直接結び、名前の確かめ（SNI と
//!    証明書）は URL の名前で行う。平文の http も同じトンネルで送る（中継に URL を渡さない）。
//! 3. HTTP の部品（reqwest）は、トンネルにつながる Unix ソケットへつなぐ（`ClientBuilder::unix_socket`）。ソケットは
//!    このプロセスの利用者だけが入れるディレクトリ（0700）に置き、最初の 1 本だけを受ける（[`ProxyTunnel`] を落とすと
//!    閉じて消える）。
//! 4. 環境のプロキシ（`HTTPS_PROXY` など）は読まない。中継は設定で明示したものだけ。
//! 5. 中継の認証（Basic）は CONNECT の要求にだけ付ける（送り先には届かない）。`Debug`・[`EgressProxy::endpoint`]・
//!    誤りの種類（[`ProxyError`]）に資格情報を出さない。中継の URL に利用者名やパスワードを書かせない
//!    （誤りの文にも URL を映し返さない）。
//!
//! 中継が CONNECT を IP アドレスで受け、そのポート（https は 443）へのトンネルを許すこと、サーバーが外の名前を
//! 引けることが前提。TLS を中継で開いて見る（TLS インスペクションの）中継は、証明書の確かめが通らないので使えない。

use std::fmt;
use std::net::SocketAddr;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use base64::Engine as _;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpStream, UnixListener};
use tokio::task::JoinHandle;

/// 中継へつなぐ時間の上限（名前解決と TCP の接続）。
const CONNECT_TIMEOUT: Duration = Duration::from_secs(5);

/// CONNECT の応答を待つ時間の上限（中継が送り先へつなぐ時間を含む）。
const RESPONSE_TIMEOUT: Duration = Duration::from_secs(10);

/// CONNECT の応答の見出しの大きさの上限（バイト）。
const MAX_RESPONSE_HEAD: usize = 16 * 1024;

/// 試すアドレスの数の上限（中継が送り先につなげないときに次のアドレスを試す）。
const MAX_ATTEMPTS: usize = 2;

/// 確かめたアドレスへの送信を中継に通すか（すべてが公開の IP のとき）。
///
/// 管理者が内部の送り先として許したホスト（内部の IP を含む）は、中継を通さずに直接つなぐ。
#[must_use]
pub fn via_proxy(addrs: &[SocketAddr]) -> bool {
    !addrs.is_empty() && addrs.iter().all(|a| crate::net_guard::is_public_ip(a.ip()))
}

/// 運用者が設定で明示した中継（`http://host:port` と、任意の Basic 認証）。
#[derive(Clone)]
pub struct EgressProxy {
    inner: Arc<Inner>,
}

struct Inner {
    /// 中継の名前か IP（IPv6 は括弧なし）。
    host: String,
    port: u16,
    /// `Proxy-Authorization` の値（`Basic …`）。表に出さない。
    authorization: Option<String>,
}

impl fmt::Debug for EgressProxy {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("EgressProxy")
            .field("endpoint", &self.endpoint())
            .field("authenticated", &self.inner.authorization.is_some())
            .finish()
    }
}

/// 中継を通してつなげなかった理由。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProxyError {
    /// 中継へつなげない（名前を引けない・接続を断られた・時間切れ）。やり直せる。
    Unreachable,
    /// 中継が認証を受け付けない（407）。中継の設定の誤り。
    AuthRejected,
    /// 中継がその送り先へのトンネルを断った（407 以外の 4xx）。中継の方針。
    Refused,
    /// 中継が送り先へつなげなかった（5xx・応答の時間切れ）。やり直せる。
    UpstreamFailed,
    /// 中継の応答を読めない（HTTP でない・大きすぎる・途中で切れた・2xx と 4xx・5xx のどれでもない）。やり直せる。
    BadResponse,
    /// このサーバーの中でトンネルの口を開けなかった（一時ディレクトリ・ソケット）。やり直せる。
    Local,
}

impl ProxyError {
    /// やり直せば通りうるか（認証と中継の方針で断られたものは、設定を直すまで通らない）。
    #[must_use]
    pub const fn is_retryable(self) -> bool {
        !matches!(self, ProxyError::AuthRejected | ProxyError::Refused)
    }

    /// 何が起きたかの 1 文（送り口が、することの文を足して返す）。中継の場所と資格情報は含めない。
    #[must_use]
    pub const fn message(self) -> &'static str {
        match self {
            ProxyError::Unreachable => "外への通信の中継（プロキシ）に接続できませんでした。",
            ProxyError::AuthRejected => {
                "外への通信の中継（プロキシ）が、このサーバーの認証を受け付けませんでした。"
            }
            ProxyError::Refused => {
                "外への通信の中継（プロキシ）が、この送り先への接続を断りました。"
            }
            ProxyError::UpstreamFailed => {
                "外への通信の中継（プロキシ）から、送り先に接続できませんでした。"
            }
            ProxyError::BadResponse => "外への通信の中継（プロキシ）の応答を読めませんでした。",
            ProxyError::Local => "外への通信の中継（プロキシ）を使う準備ができませんでした。",
        }
    }
}

impl EgressProxy {
    /// 中継の URL（`http://host:port`）と、Basic 認証の利用者名とパスワード（任意）から組む。
    ///
    /// 誤りの文は管理者向けで、URL を映し返さない（資格情報を書いた URL がログに残らないように）。
    pub fn new(url: &str, credentials: Option<(&str, &str)>) -> Result<Self, String> {
        let parsed = url::Url::parse(url.trim()).map_err(|_| {
            "egress.proxy_url を URL として読めません。http://ホスト:ポート の形で書いてください。".to_string()
        })?;
        if !parsed.username().is_empty() || parsed.password().is_some() {
            return Err(
                "egress.proxy_url に利用者名やパスワードを書かないでください。egress.proxy_username と \
                 egress.proxy_password_file に分けて書きます。"
                    .into(),
            );
        }
        if parsed.scheme() != "http" {
            return Err(
                "egress.proxy_url は http:// で始めてください（中継との間を TLS にする https:// は使えません）。"
                    .into(),
            );
        }
        if !matches!(parsed.path(), "" | "/")
            || parsed.query().is_some()
            || parsed.fragment().is_some()
        {
            return Err(
                "egress.proxy_url には、ホストとポートだけを書いてください（例: http://proxy.corp.example:8080）。"
                    .into(),
            );
        }
        let host =
            match parsed.host() {
                Some(url::Host::Domain(d)) => d.to_string(),
                Some(url::Host::Ipv4(ip)) => ip.to_string(),
                Some(url::Host::Ipv6(ip)) => ip.to_string(),
                None => return Err(
                    "egress.proxy_url にホストがありません（例: http://proxy.corp.example:8080）。"
                        .into(),
                ),
            };
        let port = parsed.port_or_known_default().unwrap_or(80);
        let authorization = match credentials {
            None => None,
            Some(("", _)) => {
                return Err("egress.proxy_username が空です。".into());
            }
            Some((user, _)) if user.contains(':') || user.chars().any(char::is_control) => {
                return Err("egress.proxy_username に「:」や制御文字は使えません。".into());
            }
            Some((_, password)) if password.chars().any(|c| c == '\r' || c == '\n') => {
                return Err(
                    "egress.proxy_password_file のパスワードに改行を含められません。".into(),
                );
            }
            Some((user, password)) => Some(format!(
                "Basic {}",
                base64::engine::general_purpose::STANDARD.encode(format!("{user}:{password}"))
            )),
        };
        Ok(EgressProxy {
            inner: Arc::new(Inner {
                host,
                port,
                authorization,
            }),
        })
    }

    /// ログに出す中継の場所（`host:port`・資格情報を含まない）。
    #[must_use]
    pub fn endpoint(&self) -> String {
        if self.inner.host.contains(':') {
            format!("[{}]:{}", self.inner.host, self.inner.port)
        } else {
            format!("{}:{}", self.inner.host, self.inner.port)
        }
    }

    /// 確かめたアドレスのどれかへ、中継のトンネルを開く（IPv4 を先に、[`MAX_ATTEMPTS`] 個まで試す）。
    ///
    /// 中継が送り先へつなげないとき（[`ProxyError::UpstreamFailed`]・[`ProxyError::BadResponse`]）だけ、次のアドレスを
    /// 試す。中継へつなげない・断られたときは、ほかのアドレスでも同じなので試さない。
    pub async fn open(&self, addrs: &[SocketAddr]) -> Result<ProxyTunnel, ProxyError> {
        let mut ordered: Vec<SocketAddr> = addrs.iter().filter(|a| a.is_ipv4()).copied().collect();
        ordered.extend(addrs.iter().filter(|a| a.is_ipv6()));
        let mut last = ProxyError::UpstreamFailed;
        for target in ordered.into_iter().take(MAX_ATTEMPTS) {
            match self.connect(target).await {
                Ok((stream, early)) => return ProxyTunnel::serve(stream, early),
                Err(e @ (ProxyError::UpstreamFailed | ProxyError::BadResponse)) => last = e,
                Err(e) => return Err(e),
            }
        }
        Err(last)
    }

    /// 中継へつなぎ、`target` への CONNECT を頼む。成功なら（トンネルの接続・見出しの後に届いていたバイト）。
    async fn connect(&self, target: SocketAddr) -> Result<(TcpStream, Vec<u8>), ProxyError> {
        let mut stream = tokio::time::timeout(
            CONNECT_TIMEOUT,
            TcpStream::connect((self.inner.host.as_str(), self.inner.port)),
        )
        .await
        .map_err(|_| ProxyError::Unreachable)?
        .map_err(|_| ProxyError::Unreachable)?;
        // SocketAddr の表示は IPv6 を括弧で包む（CONNECT [2001:db8::1]:443）。
        let mut request = format!("CONNECT {target} HTTP/1.1\r\nHost: {target}\r\n");
        if let Some(auth) = &self.inner.authorization {
            request.push_str("Proxy-Authorization: ");
            request.push_str(auth);
            request.push_str("\r\n");
        }
        request.push_str("\r\n");
        stream
            .write_all(request.as_bytes())
            .await
            .map_err(|_| ProxyError::Unreachable)?;
        let (status, early) = tokio::time::timeout(RESPONSE_TIMEOUT, read_head(&mut stream))
            .await
            .map_err(|_| ProxyError::UpstreamFailed)??;
        match status {
            200..=299 => Ok((stream, early)),
            407 => Err(ProxyError::AuthRejected),
            400..=499 => Err(ProxyError::Refused),
            500..=599 => Err(ProxyError::UpstreamFailed),
            _ => Err(ProxyError::BadResponse),
        }
    }
}

/// CONNECT の応答の見出しを読み、状態の番号と、見出しの後に続けて届いたバイトを返す。
async fn read_head(stream: &mut TcpStream) -> Result<(u16, Vec<u8>), ProxyError> {
    let mut buf = Vec::with_capacity(512);
    let mut chunk = [0u8; 1024];
    let end = loop {
        let n = stream
            .read(&mut chunk)
            .await
            .map_err(|_| ProxyError::BadResponse)?;
        if n == 0 {
            return Err(ProxyError::BadResponse);
        }
        // 区切りが前の読みとまたがっても見つかるよう、足す前の末尾 3 バイトから探す。
        let from = buf.len().saturating_sub(3);
        buf.extend_from_slice(&chunk[..n]);
        if let Some(pos) = buf[from..].windows(4).position(|w| w == b"\r\n\r\n") {
            break from + pos + 4;
        }
        if buf.len() > MAX_RESPONSE_HEAD {
            return Err(ProxyError::BadResponse);
        }
    };
    let line_end = buf.iter().position(|&b| b == b'\r').unwrap_or(end);
    let line = std::str::from_utf8(&buf[..line_end]).map_err(|_| ProxyError::BadResponse)?;
    let mut parts = line.split(' ');
    let version = parts.next().unwrap_or_default();
    if !version.starts_with("HTTP/1.") {
        return Err(ProxyError::BadResponse);
    }
    let status = parts
        .next()
        .and_then(|s| s.parse::<u16>().ok())
        .ok_or(ProxyError::BadResponse)?;
    Ok((status, buf[end..].to_vec()))
}

/// 中継のトンネルにつながる Unix ソケット（HTTP の部品は [`ProxyTunnel::socket_path`] へつなぐ）。
///
/// 最初の 1 本の接続だけを受けてトンネルにつなぎ、ほかは受けない。落とすと、受けていない口を閉じ、中継との接続を
/// 切り、ソケットのディレクトリを消す（送信を終えてから落とす）。
pub struct ProxyTunnel {
    socket: PathBuf,
    relay: JoinHandle<()>,
    /// ソケットを置いたディレクトリ（0700）。落とすと中ごと消える。
    _dir: tempfile::TempDir,
}

impl ProxyTunnel {
    fn serve(upstream: TcpStream, early: Vec<u8>) -> Result<Self, ProxyError> {
        use std::os::unix::fs::PermissionsExt as _;
        let dir = tempfile::Builder::new()
            .prefix("shiki-egress-")
            .permissions(std::fs::Permissions::from_mode(0o700))
            .tempdir()
            .map_err(|e| {
                tracing::warn!(error = %e, "外への送信の中継のトンネルの口（一時ディレクトリ）を作れませんでした");
                ProxyError::Local
            })?;
        let socket = dir.path().join("t.sock");
        let listener = UnixListener::bind(&socket).map_err(|e| {
            tracing::warn!(error = %e, "外への送信の中継のトンネルの口（Unix ソケット）を開けませんでした");
            ProxyError::Local
        })?;
        let relay = tokio::spawn(async move {
            let Ok((mut local, _)) = listener.accept().await else {
                return;
            };
            // 2 本目からは受けない（送信 1 回に 1 本・ほかのものにトンネルを使わせない）。
            drop(listener);
            let mut upstream = upstream;
            if !early.is_empty() && local.write_all(&early).await.is_err() {
                return;
            }
            let _ = tokio::io::copy_bidirectional(&mut local, &mut upstream).await;
        });
        Ok(ProxyTunnel {
            socket,
            relay,
            _dir: dir,
        })
    }

    /// HTTP の部品がつなぐソケットの場所。
    #[must_use]
    pub fn socket_path(&self) -> &Path {
        &self.socket
    }
}

impl fmt::Debug for ProxyTunnel {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("ProxyTunnel")
            .field("socket", &self.socket)
            .finish_non_exhaustive()
    }
}

impl Drop for ProxyTunnel {
    fn drop(&mut self) {
        self.relay.abort();
    }
}

#[cfg(test)]
#[path = "egress_proxy_tests.rs"]
mod tests;
