//! ミニアプリの関数の外部 HTTP（`http.request`）の送り口（BUG-196・PIT-36・PIT-48）。
//!
//! manifest の `egress_allowlist` でホストの名前を照らした後（[`crate::egress_allowed`]）、ここで次を守って送る。
//! ワークフローの送り口（api の `workflow_runtime/http_egress.rs`）・web.fetch と同じ決めごと:
//!
//! 1. 名前解決はここで 1 回だけ行い（[`RESOLVE_TIMEOUT`] まで）、確かめたアドレスに接続を固定する
//!    （`resolve_to_addrs`。HTTP の部品には名前を引き直させない）。確かめた後に解決先を付け替える DNS リバインディングで、
//!    内部のアドレスやメタデータに届かない。名前が無い（`net_guard::NO_SUCH_HOST`）ならやり直さない `egress_denied`、
//!    DNS に届かない・一時的に引けない・時間切れならやり直せる `egress_failed` にする（ワークフローの送り口と同じ・BUG-202）。
//! 2. 解決先に公開でない IP が 1 つでもあれば、応答ごと断る（公開と内部の混在も断る）。IP の分類は
//!    `sandbox_client::net_guard` が単一の正（web.fetch・ワークフロー・サンドボックスと同じ表）。例外は、管理者が
//!    デプロイ設定 `gateway.function_http_internal_hosts`（ミニアプリの関数の内部の送り先）に書いたホストだけで、内部の
//!    IP へも送る（ワークフローの `workflow.http_internal_hosts` と同じ書き方と照らし方・`sandbox_client::internal_hosts`・
//!    D-135）。ワークフローの設定とは分け、ミニアプリの関数には、管理者がこの設定に書いたホストだけを開ける
//!    （manifest の `egress_allowlist` にも書いてあること）。リンクローカルとクラウドのメタデータは、書いても送らない。
//! 3. 3xx は追わない（ゲストが Location を見てもう一度頼めば、その宛先も照らし直す・PIT-36）。
//! 4. 環境のプロキシ（`HTTPS_PROXY` など）を使わない（`no_proxy`。プロキシ経由だと接続の固定が効かない・PIT-48 の ④）。
//!    外へ出るのに中継が要る環境では、運用者が設定 `egress.proxy_url` に書いた中継だけを通す（ワークフローの送り口と
//!    同じ・GAP-72）。中継には確かめた IP アドレスへの CONNECT を頼み（名前を渡さない）、TLS はトンネルの中で URL の
//!    名前で結ぶ（[`open_tunnel`]・`sandbox_client::egress_proxy`）。
//! 5. 本文は [`MAX_BODY`] バイトまで流して読み、超えた分は読まない（巨大な応答の全量を持たない）。

use std::net::SocketAddr;
use std::time::Duration;

use sandbox_client::egress_proxy::{self, EgressProxy, ProxyTunnel};
use sandbox_client::net_guard::{self, AddrRefusal, HostResolver};
use script_runtime::HostResponse;

/// 応答の本文を読む上限（バイト）。超えた分は読まずに打ち切り、`truncated` を立てる。
pub(crate) const MAX_BODY: usize = 1024 * 1024;

/// 名前解決の上限（HTTP の時間の上限は接続の後にしか効かないので、解決にも別に掛ける）。
pub(crate) const RESOLVE_TIMEOUT: Duration = Duration::from_secs(5);

/// 1 回の送信の上限（接続から本文を読み終えるまで）。
const REQUEST_TIMEOUT: Duration = Duration::from_secs(15);

/// URL のホストを 1 回だけ解決し、送ってよいアドレスかを確かめる。名前のホストは（名前・アドレス）を返す。
///
/// IP の書き方（10 進・8 進・16 進の IPv4、IPv4 を写した IPv6）は、URL を読んだ時点で IP にそろう。
/// `allow_internal` は、管理者が内部の送り先として許したホストか（内部の IP も通す。リンクローカル・メタデータは断る）。
pub(crate) async fn resolve_checked(
    resolver: &dyn HostResolver,
    url: &url::Url,
    port: u16,
    allow_internal: bool,
) -> Result<(Option<String>, Vec<SocketAddr>), HostResponse> {
    let shown = url.host_str().unwrap_or_default();
    let (domain, found_addrs) = match url.host() {
        Some(url::Host::Domain(d)) => {
            let found = match tokio::time::timeout(RESOLVE_TIMEOUT, resolver.lookup(d, port)).await {
                Ok(Ok(addrs)) => addrs,
                // その名前のアドレスが無い（URL のドメインの誤り）。やり直しても直らない（ワークフローの送り口と同じ・BUG-202）。
                Ok(Err(reason)) if reason == net_guard::NO_SUCH_HOST => {
                    return Err(super::functions::deny(format!(
                        "{shown} が見つかりませんでした。URL のドメインに誤りがないか確かめてください。"
                    )))
                }
                // DNS に届かない・一時的に引けない（やり直せる・下の Unresolved と同じ）。
                Ok(Err(_)) => Vec::new(),
                Err(_) => {
                    return Err(failed(format!(
                        "{shown} の場所を時間内に調べられませんでした。少し待ってから、もう一度試してください。"
                    )))
                }
            };
            (Some(d.to_string()), found)
        }
        Some(url::Host::Ipv4(ip)) => (None, vec![SocketAddr::new(ip.into(), port)]),
        Some(url::Host::Ipv6(ip)) => (None, vec![SocketAddr::new(ip.into(), port)]),
        None => return Err(super::functions::invalid("URL にホストがありません")),
    };
    let seen = found_addrs.clone();
    match net_guard::check_addrs(found_addrs, allow_internal) {
        Ok(addrs) => Ok((domain, addrs)),
        Err(refusal) => {
            // 管理者向けのログ（ゲストに返す文にはアドレスを載せない）。
            tracing::warn!(host = %shown, addrs = ?seen, reason = ?refusal,
                "ミニアプリの関数の外部への送信を断りました");
            Err(refused(shown, refusal))
        }
    }
}

/// 中継を使う設定で、確かめたアドレスがすべて公開の IP なら、中継にトンネルを開かせる（GAP-72）。
///
/// 開けなければ、送り先には何も送っていないことを言う（中継が認証・方針で断ったものはやり直さない `egress_denied`、
/// ほかはやり直せる `egress_failed`）。中継の場所と資格情報はゲストに返す文に出さない。
pub(crate) async fn open_tunnel(
    proxy: Option<&EgressProxy>,
    addrs: &[SocketAddr],
    host: &str,
) -> Result<Option<ProxyTunnel>, HostResponse> {
    let Some(proxy) = proxy else {
        return Ok(None);
    };
    if !egress_proxy::via_proxy(addrs) {
        return Ok(None);
    }
    proxy.open(addrs).await.map(Some).map_err(|e| {
        tracing::warn!(proxy = %proxy.endpoint(), host = %host, reason = ?e,
            "ミニアプリの関数の外部への送信で、中継のトンネルを開けませんでした");
        let what = e.message();
        if e.is_retryable() {
            failed(format!(
                "{what}送り先には何も送っていないので、少し待ってから、もう一度試してください。"
            ))
        } else {
            super::functions::deny(format!(
                "{what}送り先には何も送っていません。管理者に、中継の設定を確かめるよう頼んでください。"
            ))
        }
    })
}

/// 確かめたアドレスにだけ接続して送り、応答を本文の上限まで読む（3xx は追わない・環境のプロキシを使わない）。
///
/// `domain` は URL のホストが名前のとき（[`resolve_checked`] の戻り値）。IP のホストは名前解決が無いので固定しない。
/// `tunnel` があれば、その口にだけ接続する（中継には確かめた IP への CONNECT を頼んである・[`open_tunnel`]）。
pub(crate) async fn send_pinned(
    url: url::Url,
    method: reqwest::Method,
    body: Option<&serde_json::Value>,
    domain: Option<&str>,
    addrs: &[SocketAddr],
    tunnel: Option<&ProxyTunnel>,
    max_body: usize,
) -> HostResponse {
    let shown = url.host_str().unwrap_or_default().to_string();
    let mut builder = reqwest::Client::builder()
        // 3xx は追わない（PIT-36）。
        .redirect(reqwest::redirect::Policy::none())
        // 環境のプロキシで接続の固定を迂回させない（PIT-48 の ④）。
        .no_proxy()
        .timeout(REQUEST_TIMEOUT);
    if let Some(tunnel) = tunnel {
        // トンネルの口にだけ接続する（TLS は URL の名前で結ぶ）。
        builder = builder.unix_socket(tunnel.socket_path());
    } else if let Some(domain) = domain {
        // 確かめたアドレスにだけ接続する（名前を引き直させない）。
        builder = builder.resolve_to_addrs(domain, addrs);
    }
    let Ok(client) = builder.build() else {
        return failed(
            "外部のサービスに送る準備ができませんでした。少し待ってから、もう一度試してください。"
                .into(),
        );
    };
    let mut req = client.request(method, url);
    if let Some(b) = body {
        req = req.json(b);
    }
    let Ok(mut resp) = req.send().await else {
        return failed(format!(
            "外部のサービス（{shown}）に接続できませんでした。送れていることがあるので、送り先の状態を確かめてください。"
        ));
    };
    let status = resp.status().as_u16();
    let mut bytes = Vec::new();
    let mut truncated = false;
    loop {
        match resp.chunk().await {
            Ok(Some(chunk)) => {
                if bytes.len() + chunk.len() > max_body {
                    bytes.extend_from_slice(&chunk[..max_body - bytes.len()]);
                    truncated = true;
                    break;
                }
                bytes.extend_from_slice(&chunk);
            }
            Ok(None) => break,
            Err(_) => {
                return failed(format!(
                    "外部のサービス（{shown}）の応答を最後まで受け取れませんでした。送れていることがあるので、\
                     送り先の状態を確かめてください。"
                ))
            }
        }
    }
    if truncated {
        trim_partial_utf8(&mut bytes);
    }
    let text = String::from_utf8_lossy(&bytes).into_owned();
    HostResponse::Ok(serde_json::json!({
        "status": status,
        "body": text,
        "truncated": truncated,
    }))
}

/// 上限で切った末尾の、途中で切れた UTF-8 の 1 文字を外す（置き換えの文字にしない）。
fn trim_partial_utf8(bytes: &mut Vec<u8>) {
    let n = bytes.len();
    for back in 1..=n.min(4) {
        let b = bytes[n - back];
        if b & 0xC0 == 0x80 {
            // 続きのバイト。文字の頭を探す。
            continue;
        }
        let need = match b {
            0x00..=0x7F => 1,
            0xF0..=0xFF => 4,
            0xE0..=0xEF => 3,
            _ => 2,
        };
        if need > back {
            bytes.truncate(n - back);
        }
        return;
    }
}

/// 断った理由をゲストに返す文にする（どれもやり直さない。解決できないときだけやり直せる）。
fn refused(host: &str, refusal: AddrRefusal) -> HostResponse {
    let message = match refusal {
        // 名前が無いと分かったときは [`resolve_checked`] が先に断る。ここは DNS に届かない・一時的に引けない
        // （ワークフローの送り口と同じくやり直せる失敗・BUG-202）。
        AddrRefusal::Unresolved => {
            return failed(format!(
                "{host} の場所を調べられませんでした。少し待ってから、もう一度試してください。\
                 続くときは、URL のドメインを確かめてください。"
            ))
        }
        AddrRefusal::Internal => format!(
            "{host} は内部のネットワークにつながるため、接続しませんでした。社内のサービスに送るには、管理者に、\
             この送り先をミニアプリの関数の社内の送り先として登録するよう頼んでください。"
        ),
        AddrRefusal::LinkLocalOrMetadata => format!(
            "{host} は、サーバーの設定を読み出せる特別なアドレスにつながるため、接続しませんでした。"
        ),
    };
    super::functions::deny(message)
}

/// 送れなかった（もう一度試せば通るかもしれない）。
fn failed(message: String) -> HostResponse {
    HostResponse::Err {
        message,
        code: "egress_failed".into(),
        retryable: true,
    }
}

#[cfg(test)]
#[path = "function_http_tests.rs"]
mod tests;
