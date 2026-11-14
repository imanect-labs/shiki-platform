//! run 単位のブラウザセッション（docs/agent/browser.md §2・§5.1・§6.2）。
//!
//! cookie jar・現在のエンジンのページ・開いてよい URL の出所を持つ。エンジンは jar を持たない。
//! セッションは run の終了で破棄する（jar も一緒に消える。v1 では永続化しない）。

use std::collections::HashSet;
use std::sync::Arc;

use authz::AuthContext;
use url::Url;

use crate::cookie_jar::CookieJar;
use crate::engine::{
    Action, BrowserEngine, BrowserError, EnginePage, LoadRequest, PlannedRequest, RedirectCheck,
    SessionOpts,
};
use crate::snapshot::Snapshot;

/// 開いてよい URL の出所（browser.md §6.2）。
///
/// `browser` ツールが事前許可で開けるのは、①ユーザーがこの会話で書いた URL ②この run で開いたページに
/// リンクとして現れた URL だけ。判定はツールが行い、範囲外は `browser_act`（承認付き）に回す。
#[derive(Debug, Default, Clone)]
pub struct Provenance {
    user: HashSet<String>,
    links: HashSet<String>,
}

/// 出所の照合に使う形（フラグメントは送られないので落とす）。
fn key(url: &Url) -> String {
    let mut u = url.clone();
    u.set_fragment(None);
    u.to_string()
}

impl Provenance {
    /// ユーザーが会話で書いた URL を出所に加える。
    pub fn add_user_url(&mut self, url: &Url) {
        self.user.insert(key(url));
    }

    /// 開いたページに現れたリンクを出所に加える。
    pub fn record_links(&mut self, snapshot: &Snapshot) {
        for u in snapshot.link_urls() {
            self.links.insert(key(u));
        }
    }

    /// 事前許可で開ける URL か。
    #[must_use]
    pub fn allows(&self, url: &Url) -> bool {
        let k = key(url);
        self.user.contains(&k) || self.links.contains(&k)
    }
}

/// run 単位のセッション。
pub struct BrowserSession {
    ctx: AuthContext,
    engine: Arc<dyn BrowserEngine>,
    page: Option<Box<dyn EnginePage>>,
    jar: CookieJar,
    provenance: Provenance,
    redirect_check: Option<RedirectCheck>,
}

impl BrowserSession {
    /// `user_urls` はユーザーが会話で書いた URL（チャットのワーカーが run の開始時に抜き出して渡す）。
    pub fn new(
        ctx: AuthContext,
        engine: Arc<dyn BrowserEngine>,
        user_urls: impl IntoIterator<Item = Url>,
    ) -> Self {
        let mut provenance = Provenance::default();
        for u in user_urls {
            provenance.add_user_url(&u);
        }
        BrowserSession {
            ctx,
            engine,
            page: None,
            jar: CookieJar::new(),
            provenance,
            redirect_check: None,
        }
    }

    /// リダイレクトの転送先の照合を設定する（最初のページを開く前に呼ぶ・browser.md §6.2）。
    #[must_use]
    pub fn with_redirect_check(mut self, check: RedirectCheck) -> Self {
        self.redirect_check = Some(check);
        self
    }

    async fn page(&mut self) -> Result<&mut Box<dyn EnginePage>, BrowserError> {
        if self.page.is_none() {
            let page = self
                .engine
                .open(
                    &self.ctx,
                    &SessionOpts {
                        first_generation: 0,
                        redirect_check: self.redirect_check.clone(),
                    },
                )
                .await?;
            self.page = Some(page);
        }
        self.page.as_mut().ok_or(BrowserError::NoPage)
    }

    /// アドレスバー相当で開く（出所の判定は呼び出し側のツールが済ませている前提）。
    ///
    /// jar はセッションに置いたまま借用して渡す（取り出して戻す形だと、呼び出し側の期限で future が
    /// 途中で捨てられたときに jar ごと消え、ログイン状態を失う）。
    pub async fn navigate(&mut self, url: Url) -> Result<Snapshot, BrowserError> {
        self.page().await?;
        let result = match self.page.as_mut() {
            Some(page) => page.load(LoadRequest::get(url), &mut self.jar).await,
            None => Err(BrowserError::NoPage),
        };
        self.after(result)
    }

    /// ページへの操作。
    pub async fn act(&mut self, action: Action) -> Result<Snapshot, BrowserError> {
        let result = match self.page.as_mut() {
            Some(page) => page.act(action, &mut self.jar).await,
            None => Err(BrowserError::NoPage),
        };
        self.after(result)
    }

    /// 操作が送ることになる要求を、送らずに返す（ツールが承認の要否を判定する・browser.md §6.1）。
    pub fn plan(&self, action: &Action) -> Result<Option<PlannedRequest>, BrowserError> {
        match self.page.as_ref() {
            Some(page) => page.plan(action),
            None => Err(BrowserError::NoPage),
        }
    }

    /// 現在のスナップショット。
    pub async fn snapshot(&mut self) -> Result<Snapshot, BrowserError> {
        match self.page.as_mut() {
            Some(page) => page.snapshot().await,
            None => Err(BrowserError::NoPage),
        }
    }

    fn after(&mut self, result: Result<Snapshot, BrowserError>) -> Result<Snapshot, BrowserError> {
        if let Ok(snap) = &result {
            self.provenance.record_links(snap);
        }
        result
    }

    /// 開いてよい URL の出所。
    #[must_use]
    pub fn provenance(&self) -> &Provenance {
        &self.provenance
    }

    /// 監査用の cookie の要約（ホストごとの件数。名前と値は出さない）。
    #[must_use]
    pub fn cookie_summary(&self) -> Vec<(String, usize)> {
        self.jar.host_summary()
    }

    /// このセッションの主体（監査・上限の単位）。
    #[must_use]
    pub fn auth_context(&self) -> &AuthContext {
        &self.ctx
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::snapshot::{ElementInfo, ElementKind, EngineKind, RefId};

    #[test]
    fn provenance_tracks_user_urls_and_page_links() {
        let mut p = Provenance::default();
        let user = Url::parse("https://a.example/x#frag").unwrap();
        p.add_user_url(&user);
        assert!(
            p.allows(&Url::parse("https://a.example/x").unwrap()),
            "フラグメントは無視"
        );
        assert!(
            !p.allows(&Url::parse("https://a.example/x?q=1").unwrap()),
            "クエリを足したら別物"
        );

        let snap = Snapshot {
            url: user,
            status: 200,
            title: String::new(),
            text: String::new(),
            elements: vec![ElementInfo {
                id: RefId {
                    engine: EngineKind::Text,
                    generation: 1,
                    index: 1,
                },
                kind: ElementKind::Link {
                    href: Url::parse("https://b.example/next").unwrap(),
                },
                label: "次".into(),
            }],
            needs_js: None,
            truncated: false,
        };
        p.record_links(&snap);
        assert!(p.allows(&Url::parse("https://b.example/next").unwrap()));
        assert!(!p.allows(&Url::parse("https://attacker.example/?q=secret").unwrap()));
    }
}
