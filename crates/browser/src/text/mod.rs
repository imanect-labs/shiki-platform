//! JS を実行しないテキストエンジン（案 D・docs/agent/browser.md §8）。
//!
//! shiki-server のプロセス内で動く。取得は `web_access::public_fetch` の関門、文字コードは
//! `web_access::decode` を共有する（関門を二重実装しない・§2）。

mod fetch;
mod render;

use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use authz::AuthContext;
use sandbox_client::net_guard::HostResolver;
use url::Url;
use web_access::public_fetch::{self, AddrGuard};

use crate::cookie_jar::CookieJar;
use crate::engine::{
    Action, BrowserEngine, BrowserError, EnginePage, LoadRequest, Method, SessionOpts,
};
use crate::snapshot::{ElementKind, EngineKind, RefId, Snapshot};
use fetch::Fetcher;
use render::{FieldKind, FieldState, Rendered};

/// テキストエンジンの上限。
#[derive(Debug, Clone)]
pub struct TextLimits {
    /// 1 回の取得（リダイレクト込み）の時間。
    pub timeout: Duration,
    /// 本文の上限（バイト）。超えた分は読まずに切る。
    pub max_bytes: usize,
    /// リダイレクトの上限。
    pub max_redirects: usize,
    /// スナップショットの本文の上限（文字）。
    pub max_chars: usize,
    /// `back` のために持つ履歴の件数。
    pub max_history: usize,
    /// 履歴に持つ本文の合計（バイト）。溢れた古い項目には戻れない（再取得はしない）。
    pub max_history_bytes: usize,
}

impl Default for TextLimits {
    fn default() -> Self {
        TextLimits {
            timeout: Duration::from_secs(15),
            max_bytes: 2 * 1024 * 1024,
            max_redirects: 5,
            max_chars: 12_000,
            max_history: 20,
            max_history_bytes: 8 * 1024 * 1024,
        }
    }
}

const USER_AGENT: &str = "shiki-agent-browser/0.1 (text; no-js)";

/// テキストエンジン。
#[derive(Clone)]
pub struct TextEngine {
    fetcher: Fetcher,
    limits: TextLimits,
}

impl TextEngine {
    /// 本番の設定（解決後 IP は公開アドレスのみ）。
    #[must_use]
    pub fn new(resolver: Arc<dyn HostResolver>, limits: TextLimits) -> Self {
        Self::with_guard(resolver, public_fetch::public_only, limits)
    }

    /// 解決後 IP の検証を差し替える（テストのスタブ、管理者が許可した社内ホスト・§12-6）。
    #[must_use]
    pub fn with_guard(
        resolver: Arc<dyn HostResolver>,
        guard: AddrGuard,
        limits: TextLimits,
    ) -> Self {
        TextEngine {
            fetcher: Fetcher {
                resolver,
                guard,
                timeout: limits.timeout,
                max_bytes: limits.max_bytes,
                max_redirects: limits.max_redirects,
                user_agent: USER_AGENT.into(),
            },
            limits,
        }
    }
}

#[async_trait]
impl BrowserEngine for TextEngine {
    fn kind(&self) -> EngineKind {
        EngineKind::Text
    }

    async fn open(
        &self,
        _ctx: &AuthContext,
        opts: &SessionOpts,
    ) -> Result<Box<dyn EnginePage>, BrowserError> {
        Ok(Box::new(TextPage {
            engine: self.clone(),
            generation: opts.first_generation,
            history: Vec::new(),
            current: None,
        }))
    }
}

/// 履歴の 1 項目（受け取った主文書の応答を持ち、`back` で再取得しない・§5.3）。
struct HistoryEntry {
    url: Url,
    status: u16,
    html: String,
    body_truncated: bool,
}

/// 現在表示しているページ。
struct Current {
    rendered: Rendered,
    snapshot: Snapshot,
    state: HashMap<u32, FieldState>,
}

/// テキストエンジンの 1 セッション。
pub struct TextPage {
    engine: TextEngine,
    generation: u32,
    history: Vec<HistoryEntry>,
    current: Option<Current>,
}

fn is_htmlish(content_type: Option<&str>) -> bool {
    content_type.is_none_or(|ct| {
        let ct = ct.to_ascii_lowercase();
        ct.starts_with("text/") || ct.contains("html") || ct.contains("xml")
    })
}

impl TextPage {
    async fn show(
        &mut self,
        entry_index: usize,
        state: HashMap<u32, FieldState>,
        new_generation: bool,
    ) -> Result<Snapshot, BrowserError> {
        let entry = self.history.get(entry_index).ok_or(BrowserError::NoPage)?;
        if new_generation {
            self.generation += 1;
        }
        let html = entry.html.clone();
        let url = entry.url.clone();
        let status = entry.status;
        let body_truncated = entry.body_truncated;
        let generation = self.generation;
        let max_chars = self.engine.limits.max_chars;
        let render_state = state.clone();
        let render_url = url.clone();
        // DOM 構築は CPU バウンド。敵対的な巨大 DOM で他の run を止めない（PIT-23）。
        let rendered = tokio::task::spawn_blocking(move || {
            render::render(&html, &render_url, generation, max_chars, &render_state)
        })
        .await
        .map_err(|_| BrowserError::Fetch("ページの解析に失敗しました".into()))?;
        let snapshot = Snapshot {
            url,
            status,
            title: rendered.title.clone(),
            text: rendered.text.clone(),
            elements: rendered.elements.clone(),
            needs_js: rendered.needs_js.clone(),
            truncated: rendered.truncated || body_truncated,
        };
        self.current = Some(Current {
            rendered,
            snapshot: snapshot.clone(),
            state,
        });
        Ok(snapshot)
    }

    fn push_history(&mut self, entry: HistoryEntry) {
        self.history.push(entry);
        let limits = &self.engine.limits;
        while self.history.len() > limits.max_history
            || (self.history.len() > 1
                && self.history.iter().map(|h| h.html.len()).sum::<usize>()
                    > limits.max_history_bytes)
        {
            self.history.remove(0);
        }
    }

    fn current(&self) -> Result<&Current, BrowserError> {
        self.current.as_ref().ok_or(BrowserError::NoPage)
    }

    /// ref を現在のページの要素に引き当てる（別エンジン・古い世代の ref は拒否する）。
    fn element(&self, id: RefId) -> Result<(u32, ElementKind, String), BrowserError> {
        let cur = self.current()?;
        if id.engine != EngineKind::Text || id.generation != self.generation {
            return Err(BrowserError::StaleRef(id));
        }
        cur.snapshot
            .element(id)
            .map(|e| (e.id.index, e.kind.clone(), e.label.clone()))
            .ok_or(BrowserError::StaleRef(id))
    }

    async fn rerender(
        &mut self,
        state: HashMap<u32, FieldState>,
    ) -> Result<Snapshot, BrowserError> {
        let last = self
            .history
            .len()
            .checked_sub(1)
            .ok_or(BrowserError::NoPage)?;
        self.show(last, state, false).await
    }

    fn form_of(&self, index: u32) -> Option<usize> {
        self.current
            .as_ref()?
            .rendered
            .fields
            .iter()
            .find(|f| f.element == Some(index))
            .and_then(|f| f.form)
    }

    /// フォームを送信する要求を組み立てる（`submitter` は押したボタンの番号）。
    fn submission(
        &self,
        form_index: usize,
        submitter: Option<u32>,
    ) -> Result<LoadRequest, BrowserError> {
        let cur = self.current()?;
        let form = cur
            .rendered
            .forms
            .get(form_index)
            .ok_or(BrowserError::NotInteractive("送信"))?;
        if form.enctype.starts_with("multipart/") || form.enctype.starts_with("text/plain") {
            return Err(BrowserError::Unsupported(form.enctype.clone()));
        }
        let mut pairs = Vec::new();
        for f in cur
            .rendered
            .fields
            .iter()
            .filter(|f| f.form == Some(form_index))
        {
            if f.name.is_empty() {
                continue;
            }
            match &f.kind {
                FieldKind::Text { value }
                | FieldKind::Hidden { value }
                | FieldKind::Select { value } => {
                    pairs.push((f.name.clone(), value.clone()));
                }
                FieldKind::Check {
                    value,
                    checked: true,
                    ..
                } => pairs.push((f.name.clone(), value.clone())),
                FieldKind::Button { value, .. }
                    if f.element.is_some() && f.element == submitter =>
                {
                    pairs.push((f.name.clone(), value.clone()));
                }
                FieldKind::Check { .. } | FieldKind::Button { .. } => {}
            }
        }
        let initiator = Some(cur.snapshot.url.clone());
        if form.post {
            Ok(LoadRequest {
                url: form.action.clone(),
                method: Method::Post,
                form: pairs,
                initiator,
                approved_origin: Some(form.action.origin()),
            })
        } else {
            let mut url = form.action.clone();
            url.set_fragment(None);
            url.query_pairs_mut().clear().extend_pairs(&pairs);
            Ok(LoadRequest {
                url,
                method: Method::Get,
                form: Vec::new(),
                initiator,
                approved_origin: None,
            })
        }
    }

    fn toggle(&self, index: u32) -> Result<HashMap<u32, FieldState>, BrowserError> {
        let cur = self.current()?;
        let mut state = cur.state.clone();
        let field = cur
            .rendered
            .fields
            .iter()
            .find(|f| f.element == Some(index))
            .ok_or(BrowserError::NotInteractive("クリック"))?;
        let FieldKind::Check { checked, radio, .. } = &field.kind else {
            return Err(BrowserError::NotInteractive("クリック"));
        };
        if *radio {
            // 同じフォーム・同じ名前の他のラジオを外す。
            for other in cur.rendered.fields.iter().filter(|o| {
                o.element != Some(index)
                    && o.form == field.form
                    && o.name == field.name
                    && matches!(o.kind, FieldKind::Check { radio: true, .. })
            }) {
                if let Some(e) = other.element {
                    state.entry(e).or_default().checked = Some(false);
                }
            }
            state.entry(index).or_default().checked = Some(true);
        } else {
            state.entry(index).or_default().checked = Some(!checked);
        }
        Ok(state)
    }
}

#[async_trait]
impl EnginePage for TextPage {
    async fn load(
        &mut self,
        req: LoadRequest,
        jar: &mut CookieJar,
    ) -> Result<Snapshot, BrowserError> {
        let fetched = self.engine.fetcher.fetch(&req, jar).await?;
        if !is_htmlish(fetched.content_type.as_deref()) {
            return Err(BrowserError::NotHtml {
                content_type: fetched.content_type.unwrap_or_default(),
            });
        }
        let decoded = web_access::decode::decode(&fetched.body, fetched.content_type.as_deref());
        self.push_history(HistoryEntry {
            url: fetched.url,
            status: fetched.status,
            html: decoded.text,
            body_truncated: fetched.truncated,
        });
        let last = self.history.len() - 1;
        self.show(last, HashMap::new(), true).await
    }

    async fn act(&mut self, action: Action, jar: &mut CookieJar) -> Result<Snapshot, BrowserError> {
        match action {
            Action::Click(id) => {
                let (index, kind, label) = self.element(id)?;
                match kind {
                    ElementKind::Link { href } => {
                        let initiator = Some(self.current()?.snapshot.url.clone());
                        let mut req = LoadRequest::get(href);
                        req.initiator = initiator;
                        self.load(req, jar).await
                    }
                    ElementKind::Checkbox | ElementKind::Radio => {
                        let state = self.toggle(index)?;
                        self.rerender(state).await
                    }
                    ElementKind::Button { submits: true } => {
                        let form = self
                            .form_of(index)
                            .ok_or(BrowserError::NotInteractive("送信"))?;
                        let req = self.submission(form, Some(index))?;
                        self.load(req, jar).await
                    }
                    ElementKind::Button { submits: false } => Err(BrowserError::NeedsJs { label }),
                    ElementKind::TextBox { .. } | ElementKind::Select { .. } => {
                        Err(BrowserError::NotInteractive("クリック"))
                    }
                }
            }
            Action::Fill(id, value) => {
                let (index, kind, _) = self.element(id)?;
                if !matches!(kind, ElementKind::TextBox { .. }) {
                    return Err(BrowserError::NotInteractive("入力"));
                }
                let mut state = self.current()?.state.clone();
                state.entry(index).or_default().value = Some(value);
                self.rerender(state).await
            }
            Action::Select(id, wanted) => {
                let (index, kind, _) = self.element(id)?;
                let ElementKind::Select { options } = kind else {
                    return Err(BrowserError::NotInteractive("選択"));
                };
                let value = options
                    .iter()
                    .find(|(v, label)| *v == wanted || *label == wanted)
                    .map(|(v, _)| v.clone())
                    .ok_or(BrowserError::NoSuchOption)?;
                let mut state = self.current()?.state.clone();
                state.entry(index).or_default().value = Some(value);
                self.rerender(state).await
            }
            Action::Submit(id) => {
                let (index, _, _) = self.element(id)?;
                let form = self
                    .form_of(index)
                    .ok_or(BrowserError::NotInteractive("送信"))?;
                let req = self.submission(form, None)?;
                self.load(req, jar).await
            }
            Action::Back => {
                if self.history.len() < 2 {
                    return Err(BrowserError::NoHistory);
                }
                self.history.pop();
                let last = self.history.len() - 1;
                self.show(last, HashMap::new(), true).await
            }
        }
    }

    async fn snapshot(&mut self) -> Result<Snapshot, BrowserError> {
        Ok(self.current()?.snapshot.clone())
    }

    fn next_generation(&self) -> u32 {
        self.generation + 1
    }
}
