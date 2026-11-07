//! 外への送信の中継の配線（`egress.*`・GAP-72・`wiring.rs` から分割・行数規約）。

use anyhow::Context;

use api::config::AppConfig;
use sandbox_client::egress_proxy::EgressProxy;

/// 環境のプロキシの変数（宛先を確かめる送信は読まない。書いてあれば、使わないことをログで知らせる）。
const ENV_PROXY_VARS: [&str; 6] = [
    "HTTPS_PROXY",
    "https_proxy",
    "HTTP_PROXY",
    "http_proxy",
    "ALL_PROXY",
    "all_proxy",
];

/// 外への送信の中継を組む（パスワードのファイルを読む）。起動で 1 回だけ組み、組めなければ起動を止める。
///
/// ワークフローの外部への送信・web.fetch・回答の画像の取得・ミニアプリの関数の外部 HTTP が、この 1 つを共有する。
/// 環境のプロキシの変数はこれらの送信には使わない（黙って拾わない・接続の固定を迂回させない）。変数があって中継を
/// 書いていなければ、外へ届かない理由が分かるよう警告を残す（値は出さない）。
pub(crate) fn wire_egress_proxy(config: &AppConfig) -> anyhow::Result<Option<EgressProxy>> {
    let proxy = config
        .egress
        .proxy()
        .context("外への送信の中継（egress）の設定を読めません")?;
    if let Some(p) = &proxy {
        tracing::info!(
            proxy = %p.endpoint(),
            "ワークフローの外部への送信・web.fetch・回答の画像の取得・ミニアプリの関数の外部 HTTP は、設定した中継を\
             通します（社内の送り先は直接つなぎます）"
        );
        return Ok(proxy);
    }
    let set: Vec<&str> = ENV_PROXY_VARS
        .into_iter()
        .filter(|k| std::env::var_os(k).is_some_and(|v| !v.is_empty()))
        .collect();
    if !set.is_empty() {
        tracing::warn!(
            vars = ?set,
            "環境のプロキシの変数がありますが、ワークフローの外部への送信・web.fetch・回答の画像の取得・ミニアプリの\
             関数の外部 HTTP には使いません。中継を通すなら egress.proxy_url に書いてください"
        );
    }
    Ok(None)
}
