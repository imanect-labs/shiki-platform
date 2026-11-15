//! リンクのプレビュー（design §4.14 本文・Task 14.19）。
//!
//! - 取得は `crates/web-access` の `public_fetch`（URL 検証・解決後 IP の検証・接続先固定・PIT-48）
//!   **だけ**を通す。社内アドレス・メタデータエンドポイントは解決後の IP で弾く。
//! - 外部接続を持たない構成（エアギャップ・Web 検索を無効にした構成）では、この部品自体を付けない
//!   （[`crate::MessagingService::with_link_previews`] を呼ばない）ので取得が走らない。
//! - リダイレクトは辿らない（転送先の検証を素通りしない）。取得は投稿の後に非同期で行い、
//!   失敗したらプレビューを出さない（投稿は止めない）。

use authz::AuthContext;
use chat::model::ContentBlock;
use sandbox_client::net_guard::HostResolver;
use sqlx::PgPool;
use std::sync::Arc;
use std::time::Duration;
use storage::db_tenant::begin_tenant_tx;
use uuid::Uuid;
use web_access::public_fetch::{self, FetchTarget};

use crate::model::extract_urls;
use crate::pubsub::{Hub, Signal};
use crate::{LinkPreview, MessagingService};

/// 1 発言でプレビューを取る URL の数。
const MAX_URLS: usize = 3;
/// 1 ページの読み取り上限。
const MAX_BYTES: usize = 512 * 1024;
const TIMEOUT: Duration = Duration::from_secs(5);
const USER_AGENT: &str = "shiki-link-preview/1.0";

/// プレビューの取得器（外部接続のある構成でだけ作る）。
pub struct LinkPreviewer {
    resolver: Arc<dyn HostResolver>,
}

impl LinkPreviewer {
    #[must_use]
    pub fn new(resolver: Arc<dyn HostResolver>) -> Self {
        Self { resolver }
    }

    /// 投稿の後に非同期で取得し、発言に書き戻して配信の合図を送る。
    pub(crate) fn spawn(
        self: &Arc<Self>,
        svc: &MessagingService,
        ctx: &AuthContext,
        message: Uuid,
        channel: Uuid,
        blocks: &[ContentBlock],
    ) {
        let urls = extract_urls(blocks, MAX_URLS);
        if urls.is_empty() {
            return;
        }
        let this = Arc::clone(self);
        let pool: PgPool = svc.pool.clone();
        let hub: Hub = svc.hub.clone();
        let ctx = ctx.clone();
        tokio::spawn(async move {
            let mut previews = Vec::new();
            for url in urls {
                if let Some(p) = this.fetch(&url).await {
                    previews.push(p);
                }
            }
            if previews.is_empty() {
                return;
            }
            let Ok(mut tx) = begin_tenant_tx(&pool, ctx.tenant()).await else {
                return;
            };
            if crate::store_message::set_link_previews(&mut tx, &ctx, message, &previews)
                .await
                .is_ok()
                && tx.commit().await.is_ok()
            {
                hub.publish(ctx.tenant_id(), channel, Signal::Changed).await;
            }
        });
    }

    /// 1 つの URL のプレビュー（取れなければ `None`）。
    pub async fn fetch(&self, url: &str) -> Option<LinkPreview> {
        let target: FetchTarget = public_fetch::validate_url(url).ok()?;
        let client = public_fetch::pinned_client(
            self.resolver.as_ref(),
            &target,
            public_fetch::public_only,
            TIMEOUT,
            USER_AGENT,
        )
        .await
        .ok()?;
        let resp = client.get(target.url.clone()).send().await.ok()?;
        if !resp.status().is_success() {
            return None;
        }
        let ctype = resp
            .headers()
            .get(reqwest::header::CONTENT_TYPE)
            .and_then(|v| v.to_str().ok())
            .map(str::to_string);
        if !ctype
            .as_deref()
            .is_some_and(|c| c.to_ascii_lowercase().contains("text/html"))
        {
            return None;
        }
        let (body, _) = public_fetch::read_capped(resp, MAX_BYTES).await.ok()?;
        let html = web_access::decode::decode(&body, ctype.as_deref()).text;
        parse(url, &html)
    }
}

/// HTML から OGP・`<title>` を読む（スクリプトは実行しない・文字列を切り出すだけ）。
#[must_use]
pub fn parse(url: &str, html: &str) -> Option<LinkPreview> {
    let mut title = None;
    let mut description = None;
    let mut site_name = None;
    let lower = html.to_ascii_lowercase();
    let mut pos = 0;
    while let Some(i) = lower[pos..].find("<meta") {
        let start = pos + i;
        let Some(end) = lower[start..].find('>') else {
            break;
        };
        let tag = &html[start..start + end];
        pos = start + end;
        let key = attr(tag, "property").or_else(|| attr(tag, "name"));
        let Some(content) = attr(tag, "content") else {
            continue;
        };
        match key.as_deref().map(str::to_ascii_lowercase).as_deref() {
            Some("og:title" | "twitter:title") if title.is_none() => title = Some(content),
            Some("og:description" | "twitter:description" | "description")
                if description.is_none() =>
            {
                description = Some(content);
            }
            Some("og:site_name") => site_name = Some(content),
            _ => {}
        }
    }
    if title.is_none() {
        if let (Some(a), Some(b)) = (lower.find("<title"), lower.find("</title>")) {
            if let Some(gt) = lower[a..].find('>') {
                let s = a + gt + 1;
                if s < b {
                    title = Some(unescape(html[s..b].trim()));
                }
            }
        }
    }
    let clip = |s: String, n: usize| -> Option<String> {
        let s: String = s.split_whitespace().collect::<Vec<_>>().join(" ");
        let s: String = s.chars().take(n).collect();
        (!s.is_empty()).then_some(s)
    };
    let title = title.and_then(|t| clip(t, 200));
    let description = description.and_then(|d| clip(d, 300));
    let site_name = site_name.and_then(|s| clip(s, 100));
    if title.is_none() && description.is_none() {
        return None;
    }
    Some(LinkPreview {
        url: url.to_string(),
        title,
        description,
        site_name,
    })
}

fn attr(tag: &str, name: &str) -> Option<String> {
    let lower = tag.to_ascii_lowercase();
    let mut from = 0;
    while let Some(i) = lower[from..].find(name) {
        let at = from + i;
        from = at + name.len();
        let before_ok = at == 0 || lower.as_bytes()[at - 1].is_ascii_whitespace();
        let rest = lower[from..].trim_start();
        if !before_ok || !rest.starts_with('=') {
            continue;
        }
        let off = lower.len() - rest.len() + 1;
        let value = tag[off..].trim_start();
        let (quote, body) = match value.chars().next()? {
            q @ ('"' | '\'') => (Some(q), &value[1..]),
            _ => (None, value),
        };
        let end = match quote {
            Some(q) => body.find(q)?,
            None => body
                .find(|c: char| c.is_whitespace() || c == '/')
                .unwrap_or(body.len()),
        };
        return Some(unescape(&body[..end]));
    }
    None
}

fn unescape(s: &str) -> String {
    s.replace("&amp;", "&")
        .replace("&lt;", "<")
        .replace("&gt;", ">")
        .replace("&quot;", "\"")
        .replace("&#39;", "'")
        .replace("&#x27;", "'")
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::net::SocketAddr;

    #[test]
    fn reads_ogp_and_falls_back_to_title() {
        let html = r#"<html><head><meta property="og:title" content="社内報 &amp; お知らせ">
            <meta name="description" content='説明文'><meta property="og:site_name" content=Example>
            <title>ignored</title></head></html>"#;
        let p = parse("https://example.com", html).unwrap();
        assert_eq!(p.title.as_deref(), Some("社内報 & お知らせ"));
        assert_eq!(p.description.as_deref(), Some("説明文"));
        assert_eq!(p.site_name.as_deref(), Some("Example"));
        let p = parse("https://x.test", "<TITLE> Hello </TITLE>").unwrap();
        assert_eq!(p.title.as_deref(), Some("Hello"));
        assert!(parse("https://x.test", "<p>no meta</p>").is_none());
    }

    struct Fixed(Vec<SocketAddr>);
    #[async_trait::async_trait]
    impl HostResolver for Fixed {
        async fn lookup(&self, _h: &str, _p: u16) -> Result<Vec<SocketAddr>, &'static str> {
            Ok(self.0.clone())
        }
    }

    #[tokio::test]
    async fn never_fetches_internal_addresses() {
        // 解決結果が社内アドレス・メタデータエンドポイントなら接続しない。
        for ip in [
            "127.0.0.1:80",
            "10.0.0.5:443",
            "169.254.169.254:80",
            "[::1]:80",
        ] {
            let p = LinkPreviewer::new(Arc::new(Fixed(vec![ip.parse().unwrap()])));
            assert!(
                p.fetch("http://metadata.example.com/latest")
                    .await
                    .is_none(),
                "{ip}"
            );
        }
        // URL の段階で弾くもの（IP 直指定・ローカル名）。
        let p = LinkPreviewer::new(Arc::new(Fixed(vec!["93.184.216.34:80".parse().unwrap()])));
        for url in [
            "http://169.254.169.254/",
            "http://localhost/",
            "http://intranet.local/",
            "file:///etc/passwd",
        ] {
            assert!(p.fetch(url).await.is_none(), "{url}");
        }
    }
}
