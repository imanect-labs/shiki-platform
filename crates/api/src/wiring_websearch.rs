//! web 検索プロバイダの配線（`wiring.rs` から分割・500 行規約）。

use std::sync::Arc;
use std::time::Duration;

use anyhow::Context;

use api::config::{AppConfig, WebSearchBackend};

/// web 検索プロバイダ（Brave/SearXNG/Stub）を構築する（未設定は None）。
/// web 検索プロバイダ（Phase 4 web ツール）の配線。`websearch.backend` 未指定なら `None`。
///
/// クラウド/オンプレ差は `SearchProvider` トレイト裏で吸収する（Brave=SaaS / SearXNG=オンプレ /
/// Stub=テスト・エアギャップ）。
pub(crate) fn wire_websearch(
    config: &AppConfig,
    http: &reqwest::Client,
) -> anyhow::Result<Option<Arc<dyn websearch::SearchProvider>>> {
    let Some(backend) = config.websearch.backend else {
        return Ok(None);
    };
    // 検索は対話パスで呼ばれるため、共有クライアント（無期限）ではなく短いタイムアウトを敷く。
    let _ = http;
    let search_http = reqwest::Client::builder()
        .timeout(Duration::from_secs(15))
        .build()
        .context("web 検索用 HTTP クライアントの構築に失敗")?;
    let provider: Arc<dyn websearch::SearchProvider> = match backend {
        WebSearchBackend::Brave => {
            // compose の `${VAR:-}` は空文字を渡し得るため、空も未設定として扱い fail-fast する。
            let api_key = config
                .websearch
                .brave_api_key
                .clone()
                .filter(|s| !s.is_empty())
                .ok_or_else(|| {
                    anyhow::anyhow!("websearch.backend=brave には brave_api_key が必要です")
                })?;
            Arc::new(websearch::BraveSearchProvider::new(
                search_http,
                api_key,
                None,
            ))
        }
        WebSearchBackend::Searxng => {
            let base_url = config
                .websearch
                .searxng_base_url
                .clone()
                .filter(|s| !s.is_empty())
                .ok_or_else(|| {
                    anyhow::anyhow!("websearch.backend=searxng には searxng_base_url が必要です")
                })?;
            Arc::new(websearch::SearxngSearchProvider::new(
                search_http,
                &base_url,
            ))
        }
        WebSearchBackend::Stub => Arc::new(websearch::StubSearchProvider::new()),
    };
    tracing::info!(
        provider = provider.name(),
        "web 検索プロバイダを配線しました"
    );
    Ok(Some(provider))
}
