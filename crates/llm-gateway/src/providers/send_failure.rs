//! プロバイダへ要求を送れなかった誤りの文言（全アダプタ共通・BUG-218）。
//!
//! reqwest の送信の誤りの文（`error sending request for url (http://10.0.0.5:8000/v1/…)`）には送り先の URL が入る。
//! この文言はミニアプリの AI の能力の応答（502 の本文・SSE の error イベント）と agent.invoke の error イベントへ
//! そのまま運ばれるので、オンプレの vLLM などの内部のホスト名・IP・ポート（と、base_url に書いた userinfo）がアプリに
//! 見える。文言には誤りの種類（接続できない・名前を引けない・TLS・時間切れ）だけを入れ、URL（userinfo とクエリは
//! 伏せる）と下位の誤りはログにだけ残す。

use crate::provider::LlmError;

/// 送れなかった理由の種類。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum SendFailure {
    /// 送り先の名前を引けなかった。
    Dns,
    /// TLS の接続を確立できなかった（証明書を確かめられない・TLS でない相手など）。
    Tls,
    /// 時間内に接続できなかった・応答が無かった（reqwest の時間切れ）。
    Timeout,
    /// 接続できなかった（接続を断られた・経路が無いなど）。
    Connect,
    /// 接続の後で要求を送れなかった（応答の前に切られたなど）。
    Other,
}

/// hyper-util が名前解決の失敗に付ける文（`ConnectError::dns`）。
const DNS_ERROR: &str = "dns error";

impl SendFailure {
    /// reqwest の誤りの種類を見分ける（下位の誤りを辿る）。
    pub(crate) fn of(e: &reqwest::Error) -> Self {
        if e.is_timeout() {
            return SendFailure::Timeout;
        }
        for err in causes(e) {
            if err.to_string() == DNS_ERROR {
                return SendFailure::Dns;
            }
            // tokio-rustls は TLS の誤りを InvalidData の io::Error で包む。
            if err
                .downcast_ref::<std::io::Error>()
                .is_some_and(|io| io.kind() == std::io::ErrorKind::InvalidData)
            {
                return SendFailure::Tls;
            }
        }
        if e.is_connect() {
            SendFailure::Connect
        } else {
            SendFailure::Other
        }
    }

    /// 文言（`{label}: ` の後ろ）。時間切れは呼ぶ側がタイムアウトの文言に見分けられるよう timeout の語を残す。
    fn message(self) -> &'static str {
        match self {
            SendFailure::Dns => "プロバイダに接続できませんでした（送り先の名前を引けません）",
            SendFailure::Tls => "プロバイダに接続できませんでした（TLS の接続を確立できません）",
            SendFailure::Timeout => "プロバイダが時間内に応答しませんでした（timeout）",
            SendFailure::Connect => "プロバイダに接続できませんでした",
            SendFailure::Other => "プロバイダに要求を送れませんでした",
        }
    }
}

/// 送れなかった誤りを [`LlmError::Unavailable`] にする（文言に URL を入れない・ログにだけ残す）。
pub(crate) fn send_failure(label: &str, e: reqwest::Error) -> LlmError {
    let kind = SendFailure::of(&e);
    let url = e.url().map(redacted).unwrap_or_default();
    let e = e.without_url();
    tracing::warn!(
        provider = label,
        ?kind,
        url = %url,
        error = %chain(&e),
        "プロバイダへ要求を送れなかった"
    );
    LlmError::Unavailable(format!("{label}: {}", kind.message()))
}

/// ログに出す URL（userinfo・クエリ・フラグメントを伏せる）。
fn redacted(url: &reqwest::Url) -> String {
    let mut url = url.clone();
    let _ = url.set_username("");
    let _ = url.set_password(None);
    url.set_query(None);
    url.set_fragment(None);
    url.to_string()
}

/// 誤りと下位の誤りの文を `: ` でつなぐ（ログ用。reqwest の文は下位の誤りを含まない）。
fn chain(e: &reqwest::Error) -> String {
    causes(e)
        .iter()
        .map(ToString::to_string)
        .collect::<Vec<_>>()
        .join(": ")
}

/// 誤りから下位の誤りを順に辿る（自分を含む・深さは 16 まで）。
///
/// `io::Error` は包んだ誤りへ進む（`io::Error::source` は包んだ誤りを飛ばしてその下位を返すので、包んだ誤りの
/// 種類（TLS の誤りを包んだ InvalidData など）が見えなくなる）。
fn causes<'a>(
    e: &'a (dyn std::error::Error + 'static),
) -> Vec<&'a (dyn std::error::Error + 'static)> {
    let mut out = Vec::new();
    let mut next = Some(e);
    while let Some(err) = next {
        if out.len() >= 16 {
            break;
        }
        out.push(err);
        next = match err.downcast_ref::<std::io::Error>() {
            Some(io) => io
                .get_ref()
                .map(|inner| inner as &(dyn std::error::Error + 'static)),
            None => err.source(),
        };
    }
    out
}

#[cfg(test)]
mod tests {
    use std::net::SocketAddr;
    use std::sync::Arc;
    use std::time::Duration;

    use super::*;

    /// 送り先（userinfo とクエリ付き）に送って、送れなかった誤りを返す。
    async fn failed(client: &reqwest::Client, base: &str) -> reqwest::Error {
        client
            .post(format!("{base}/v1/chat/completions?key=q-secret"))
            .timeout(Duration::from_secs(5))
            .body("{}")
            .send()
            .await
            .expect_err("送れない")
    }

    fn no_proxy() -> reqwest::Client {
        reqwest::Client::builder().no_proxy().build().unwrap()
    }

    /// いま誰も待っていないポート（開いて閉じる）。
    async fn closed_port() -> SocketAddr {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        listener.local_addr().unwrap()
    }

    /// 文言に送り先（ホスト・ポート・パス・userinfo・クエリ）が入らない。
    fn assert_hides(msg: &str, addr: &str) {
        for leaked in [
            addr,
            "127.0.0.1",
            "llm.internal",
            "/v1/",
            "user",
            "pw-secret",
            "q-secret",
            "http",
        ] {
            assert!(!msg.contains(leaked), "{leaked} が漏れた: {msg}");
        }
    }

    #[tokio::test]
    async fn a_refused_connection_names_only_the_kind() {
        let addr = closed_port().await;
        let e = failed(&no_proxy(), &format!("http://user:pw-secret@{addr}")).await;
        assert!(
            e.to_string().contains(&addr.to_string()),
            "reqwest の文には入る: {e}"
        );
        assert_eq!(SendFailure::of(&e), SendFailure::Connect);
        let LlmError::Unavailable(msg) = send_failure("openai", e) else {
            panic!("Unavailable になる");
        };
        assert_eq!(msg, "openai: プロバイダに接続できませんでした");
        assert_hides(&msg, &addr.port().to_string());
    }

    /// 名前を引けない（解決の失敗は名前解決の差し込みで決定的に起こす）。
    #[tokio::test]
    async fn an_unresolvable_name_says_so() {
        struct NoSuchHost;
        impl reqwest::dns::Resolve for NoSuchHost {
            fn resolve(&self, _name: reqwest::dns::Name) -> reqwest::dns::Resolving {
                Box::pin(async {
                    Err::<reqwest::dns::Addrs, _>(
                        std::io::Error::other("failed to lookup address information").into(),
                    )
                })
            }
        }
        let client = reqwest::Client::builder()
            .no_proxy()
            .dns_resolver(Arc::new(NoSuchHost))
            .build()
            .unwrap();
        let e = failed(&client, "http://user:pw-secret@llm.internal:8000").await;
        assert_eq!(SendFailure::of(&e), SendFailure::Dns, "{e:?}");
        let LlmError::Unavailable(msg) = send_failure("anthropic", e) else {
            panic!("Unavailable になる");
        };
        assert_eq!(
            msg,
            "anthropic: プロバイダに接続できませんでした（送り先の名前を引けません）"
        );
        assert_hides(&msg, "8000");
    }

    /// TLS の相手でない（HTTP で答える）送り先に https で送ると、TLS の誤りと言う。
    #[tokio::test]
    async fn a_failed_tls_handshake_says_so() {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move {
            while let Ok((mut sock, _)) = listener.accept().await {
                use tokio::io::AsyncWriteExt;
                let _ = sock
                    .write_all(b"HTTP/1.1 400 Bad Request\r\ncontent-length: 0\r\n\r\n")
                    .await;
            }
        });
        let e = failed(&no_proxy(), &format!("https://{addr}")).await;
        assert_eq!(SendFailure::of(&e), SendFailure::Tls, "{e:?}");
        let LlmError::Unavailable(msg) = send_failure("responses", e) else {
            panic!("Unavailable になる");
        };
        assert_eq!(
            msg,
            "responses: プロバイダに接続できませんでした（TLS の接続を確立できません）"
        );
        assert_hides(&msg, &addr.port().to_string());
    }

    /// reqwest の時間切れは、呼ぶ側がタイムアウトの文言に見分けられる語（timeout）を残す。
    #[tokio::test]
    async fn a_timeout_keeps_the_timeout_word() {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        // 受け付けて何も返さない。
        let held = tokio::spawn(async move {
            let mut socks = Vec::new();
            while let Ok((sock, _)) = listener.accept().await {
                socks.push(sock);
            }
        });
        let e = no_proxy()
            .post(format!("http://{addr}/v1/messages"))
            .timeout(Duration::from_millis(100))
            .send()
            .await
            .expect_err("時間切れ");
        assert_eq!(SendFailure::of(&e), SendFailure::Timeout, "{e:?}");
        let LlmError::Unavailable(msg) = send_failure("openai", e) else {
            panic!("Unavailable になる");
        };
        assert_eq!(
            msg,
            "openai: プロバイダが時間内に応答しませんでした（timeout）"
        );
        assert_hides(&msg, &addr.port().to_string());
        held.abort();
    }

    #[test]
    fn logged_urls_hide_userinfo_and_query() {
        let url =
            reqwest::Url::parse("http://user:pw-secret@llm.internal:8000/v1/x?key=q-secret#f")
                .unwrap();
        assert_eq!(redacted(&url), "http://llm.internal:8000/v1/x");
    }
}
