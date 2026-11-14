//! テキストエンジンの取得（docs/agent/browser.md §8・PIT-81）。
//!
//! - 取得は `web_access::public_fetch` の関門を通す（URL 検証 → 解決後 IP の検証 → 検証済みアドレスへの接続固定）。
//! - リダイレクトは追従するが、**1 回ごとに関門を通し直す**（公開ホストから私的アドレスへ渡らせない）。
//! - メソッドは Fetch 標準に従う: 301 / 302 / 303 は POST を本文なしの GET に、307 / 308 はメソッドと本文を保つ。
//!   承認した送り先のある POST が 307 / 308 で別オリジンへ転送されるときは、本文を再送せずに止める。
//! - cookie は jar から付け、応答の `Set-Cookie` は各ホップの URL で jar へ取り込む（SameSite は jar が強制する）。

use std::sync::Arc;
use std::time::Duration;

use reqwest::header::{ACCEPT, CONTENT_TYPE, COOKIE, LOCATION, SET_COOKIE};
use reqwest::StatusCode;
use sandbox_client::net_guard::HostResolver;
use time::OffsetDateTime;
use url::Url;
use web_access::public_fetch::{self, AddrGuard, PinError};

use crate::cookie_jar::{same_site, CookieJar, RequestContext};
use crate::engine::{BrowserError, LoadRequest, Method, RedirectCheck};

/// 取得の設定。
#[derive(Clone)]
pub(crate) struct Fetcher {
    pub resolver: Arc<dyn HostResolver>,
    pub guard: AddrGuard,
    pub timeout: Duration,
    pub max_bytes: usize,
    pub max_redirects: usize,
    pub user_agent: String,
}

/// 取得結果（最終ホップ）。
#[derive(Debug, Clone)]
pub(crate) struct Fetched {
    pub url: Url,
    pub status: u16,
    pub content_type: Option<String>,
    pub body: Vec<u8>,
    pub truncated: bool,
}

impl Fetcher {
    /// 取得する。`timeout` はリダイレクトを含めた 1 回の取得全体にかかる。
    /// `check` は 301/302/303/307/308 の転送先を照合する口（出所の規則・browser.md §6.2。呼び出し側が渡す）。
    pub(crate) async fn fetch(
        &self,
        req: &LoadRequest,
        jar: &mut CookieJar,
        check: Option<&RedirectCheck>,
    ) -> Result<Fetched, BrowserError> {
        tokio::time::timeout(self.timeout, self.fetch_inner(req, jar, check))
            .await
            .map_err(|_| BrowserError::Fetch("時間内に取得が終わりませんでした".into()))?
    }

    async fn fetch_inner(
        &self,
        req: &LoadRequest,
        jar: &mut CookieJar,
        check: Option<&RedirectCheck>,
    ) -> Result<Fetched, BrowserError> {
        // SameSite の判定の基点（要求元のページ。アドレスバー相当なら最初の URL）。
        let chain_root = req.initiator.clone().unwrap_or_else(|| req.url.clone());
        let mut crossed = false;
        let mut url = req.url.clone();
        let mut method = req.method;
        let mut form = req.form.clone();
        for hop in 0..=self.max_redirects {
            let target =
                public_fetch::validate_url(url.as_str()).map_err(BrowserError::Rejected)?;
            let client = public_fetch::pinned_client(
                self.resolver.as_ref(),
                &target,
                self.guard,
                self.timeout,
                &self.user_agent,
            )
            .await
            .map_err(pin_error)?;
            let mut builder = match method {
                Method::Get => client.get(target.url.clone()),
                Method::Post => client
                    .post(target.url.clone())
                    .header(CONTENT_TYPE, "application/x-www-form-urlencoded")
                    .body(
                        url::form_urlencoded::Serializer::new(String::new())
                            .extend_pairs(&form)
                            .finish(),
                    ),
            }
            .header(ACCEPT, "text/html,application/xhtml+xml;q=0.9,text/*;q=0.5");
            // この要求を送る前に判定する（最初の別サイトへの転送先から Strict を外す）。
            if !same_site(&chain_root, &target.url) {
                crossed = true;
            }
            let ctx = RequestContext {
                initiator: req.initiator.clone(),
                top_level: true,
                safe_method: method == Method::Get,
                cross_site_redirect: crossed,
            };
            if let Some(cookie) = jar.header_for(&target.url, &ctx, OffsetDateTime::now_utc()) {
                builder = builder.header(COOKIE, cookie);
            }
            let response = builder
                .send()
                .await
                .map_err(|e| BrowserError::Fetch(without_url(&e)))?;
            let now = OffsetDateTime::now_utc();
            for value in response.headers().get_all(SET_COOKIE) {
                if let Ok(v) = value.to_str() {
                    if let Err(reason) = jar.store(&target.url, &target.url, v, now) {
                        tracing::debug!(target: "browser", host = %target.host, ?reason, "set-cookie を取り込まなかった");
                    }
                }
            }
            let status = response.status();
            if status.is_redirection() {
                let Some(next) = response
                    .headers()
                    .get(LOCATION)
                    .and_then(|l| l.to_str().ok())
                    .and_then(|l| target.url.join(l).ok())
                else {
                    return Err(BrowserError::Fetch(format!(
                        "リダイレクト（{}）に転送先がありません",
                        status.as_u16()
                    )));
                };
                if hop == self.max_redirects {
                    break;
                }
                if check.is_some_and(|c| !c(&next)) {
                    return Err(BrowserError::RedirectNotAllowed { to: next });
                }
                match status {
                    StatusCode::TEMPORARY_REDIRECT | StatusCode::PERMANENT_REDIRECT => {
                        if method == Method::Post {
                            if let Some(approved) = &req.approved_origin {
                                if next.origin() != *approved {
                                    return Err(BrowserError::RedirectBlocked {
                                        approved: approved.ascii_serialization(),
                                        to: next,
                                    });
                                }
                            }
                        }
                    }
                    _ => {
                        // 301 / 302 / 303: POST は本文なしの GET になる。
                        method = Method::Get;
                        form.clear();
                    }
                }
                url = next;
                continue;
            }
            let content_type = response
                .headers()
                .get(CONTENT_TYPE)
                .and_then(|v| v.to_str().ok())
                .map(str::to_string);
            let (body, truncated) = public_fetch::read_capped(response, self.max_bytes)
                .await
                .map_err(|e| BrowserError::Fetch(without_url(&e)))?;
            return Ok(Fetched {
                url: target.url,
                status: status.as_u16(),
                content_type,
                body,
                truncated,
            });
        }
        Err(BrowserError::TooManyRedirects)
    }
}

fn pin_error(e: PinError) -> BrowserError {
    match e {
        PinError::ResolveTimeout => {
            BrowserError::Fetch("名前解決が時間内に終わりませんでした".into())
        }
        PinError::Resolve(m) => BrowserError::Fetch(format!("名前解決に失敗しました（{m}）")),
        PinError::Rejected(m) => BrowserError::Rejected(m.to_string()),
        PinError::Client(m) => BrowserError::Fetch(m),
    }
}

/// reqwest のエラー文言から URL を落とす（クエリに載った値をエラー経由で再掲しない）。
fn without_url(e: &reqwest::Error) -> String {
    if e.is_timeout() {
        "時間内に応答がありませんでした".into()
    } else if e.is_connect() {
        "接続できませんでした".into()
    } else {
        let s = e.to_string();
        match e.url() {
            Some(u) => s.replace(u.as_str(), "<url>"),
            None => s,
        }
    }
}
