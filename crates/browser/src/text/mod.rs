//! JS を実行しないテキストエンジン（案 D・docs/agent/browser.md §8）。
//!
//! shiki-server のプロセス内で動く。取得は `web_access::public_fetch` の関門、文字コードは
//! `web_access::decode` を共有する（関門を二重実装しない・§2）。

mod fetch;
mod guard;
mod render;
mod submit;

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
    Action, BrowserEngine, BrowserError, EnginePage, LoadRequest, PlannedRequest, RedirectCheck,
    SessionOpts,
};
use crate::snapshot::{ElementKind, EngineKind, RefId, Snapshot};
use fetch::Fetcher;
use render::{Field, FieldKind, FieldState, Form};

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
            redirect_check: opts.redirect_check.clone(),
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
    /// 描画し直すたびに複製しないよう共有する。
    html: Arc<str>,
    body_truncated: bool,
}

/// 現在表示しているページ。
struct Current {
    snapshot: Snapshot,
    forms: Vec<Form>,
    fields: Vec<Field>,
    state: HashMap<usize, FieldState>,
}

/// テキストエンジンの 1 セッション。
pub struct TextPage {
    engine: TextEngine,
    redirect_check: Option<RedirectCheck>,
    generation: u32,
    history: Vec<HistoryEntry>,
    current: Option<Current>,
}

/// HTML（またはテキスト）として読んでよい応答か（browser.md §8: テキスト系の Content-Type のみ）。
///
/// `xml` を含むだけの型（docx・xlsx・pptx の `…openxmlformats…`）は通さない。Content-Type が無いときは
/// 中身で判定する（先頭が `<` で、PDF・ZIP の magic bytes でない）。
fn is_htmlish(content_type: Option<&str>, body: &[u8]) -> bool {
    if let Some(ct) = content_type {
        let mime = ct
            .split(';')
            .next()
            .unwrap_or("")
            .trim()
            .to_ascii_lowercase();
        return matches!(
            mime.as_str(),
            "text/html" | "application/xhtml+xml" | "text/plain"
        );
    }
    let text = body.strip_prefix(b"\xef\xbb\xbf").unwrap_or(body);
    let first = text.iter().find(|c| !c.is_ascii_whitespace());
    first == Some(&b'<') && !body.starts_with(b"%PDF") && !body.starts_with(b"PK")
}

/// フラグメント以外が同じ（同じ文書の中の移動）で、フラグメントがあるか。
fn same_document(current: &Url, href: &Url) -> bool {
    if href.fragment().is_none() {
        return false;
    }
    let mut a = current.clone();
    let mut b = href.clone();
    a.set_fragment(None);
    b.set_fragment(None);
    a == b
}

fn is_plain_text(content_type: Option<&str>) -> bool {
    content_type.is_some_and(|ct| {
        ct.split(';')
            .next()
            .unwrap_or("")
            .trim()
            .eq_ignore_ascii_case("text/plain")
    })
}

fn escape_html(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
}

impl TextPage {
    async fn show(
        &mut self,
        entry_index: usize,
        state: HashMap<usize, FieldState>,
        new_generation: bool,
    ) -> Result<Snapshot, BrowserError> {
        let entry = self.history.get(entry_index).ok_or(BrowserError::NoPage)?;
        if new_generation {
            self.generation += 1;
        }
        let html = Arc::clone(&entry.html);
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
            title: rendered.title,
            text: rendered.text,
            elements: rendered.elements,
            needs_js: rendered.needs_js,
            truncated: rendered.truncated || body_truncated,
        };
        self.current = Some(Current {
            snapshot: snapshot.clone(),
            forms: rendered.forms,
            fields: rendered.fields,
            state,
        });
        Ok(snapshot)
    }

    fn push_history(&mut self, entry: HistoryEntry) {
        self.history.push(entry);
        let limits = &self.engine.limits;
        // 現在のページは必ず残す（max_history が 0 でも開けるように）。
        while self.history.len() > limits.max_history.max(1)
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
        state: HashMap<usize, FieldState>,
    ) -> Result<Snapshot, BrowserError> {
        let last = self
            .history
            .len()
            .checked_sub(1)
            .ok_or(BrowserError::NoPage)?;
        self.show(last, state, false).await
    }

    /// ref の番号に対応する送信項目の位置（`rendered.fields` の添字）。
    fn field_of(&self, index: u32) -> Result<usize, BrowserError> {
        self.current()?
            .fields
            .iter()
            .position(|f| f.element == Some(index))
            .ok_or(BrowserError::NotInteractive("操作"))
    }

    /// 無効な部品は操作させない（ブラウザでも押せない・入力できない）。
    fn enabled_field(&self, index: u32) -> Result<usize, BrowserError> {
        let i = self.field_of(index)?;
        if self.current()?.fields[i].disabled {
            return Err(BrowserError::NotInteractive("操作（無効になっている部品）"));
        }
        Ok(i)
    }

    /// フォームを送信する要求（`submitter` は押したボタンの項目の位置。None なら Enter による送信）。
    fn submission(
        &self,
        field: usize,
        submitter: Option<usize>,
    ) -> Result<LoadRequest, BrowserError> {
        let cur = self.current()?;
        let form = cur.fields[field]
            .form
            .ok_or(BrowserError::NotInteractive("送信（フォームの外の部品）"))?;
        let submitter = match submitter {
            Some(s) => Some(s),
            None => submit::default_submitter(&cur.fields, form)?,
        };
        submit::build(&cur.forms, &cur.fields, &cur.snapshot.url, form, submitter)
    }

    /// 送信の要求と、承認ダイアログで伏せる項目（同じフォームのパスワード欄）。
    fn planned(
        &self,
        field: usize,
        submitter: Option<usize>,
    ) -> Result<PlannedRequest, BrowserError> {
        let request = self.submission(field, submitter)?;
        let cur = self.current()?;
        let form = cur.fields[field].form;
        let masked_fields = cur
            .fields
            .iter()
            .filter(|f| f.form == form && matches!(f.kind, FieldKind::Text { secret: true, .. }))
            .map(|f| f.name.clone())
            .collect();
        Ok(PlannedRequest {
            request,
            masked_fields,
        })
    }

    /// チェックボックス・ラジオを切り替える（状態は項目の位置で持つ）。ラジオは同じフォーム・同じ名前の
    /// 他の項目を、ref の無いもの（CSS で隠れたもの）も含めて外す。
    fn toggle(&self, field: usize) -> Result<HashMap<usize, FieldState>, BrowserError> {
        let cur = self.current()?;
        let mut state = cur.state.clone();
        let target = &cur.fields[field];
        let FieldKind::Check { checked, radio, .. } = &target.kind else {
            return Err(BrowserError::NotInteractive("クリック"));
        };
        if *radio {
            for (pos, other) in cur.fields.iter().enumerate() {
                if pos != field
                    && other.form == target.form
                    && other.name == target.name
                    && matches!(other.kind, FieldKind::Check { radio: true, .. })
                {
                    state.entry(pos).or_default().checked = Some(false);
                }
            }
            state.entry(field).or_default().checked = Some(true);
        } else {
            state.entry(field).or_default().checked = Some(!checked);
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
        let fetched = self
            .engine
            .fetcher
            .fetch(&req, jar, self.redirect_check.as_ref())
            .await?;
        if !is_htmlish(fetched.content_type.as_deref(), &fetched.body) {
            return Err(BrowserError::NotHtml {
                content_type: fetched.content_type.unwrap_or_default(),
            });
        }
        let decoded = web_access::decode::decode(&fetched.body, fetched.content_type.as_deref());
        // プレーンテキストは HTML として解析しない（中の `<a>` や `<form>` の例を操作できる部品にしない）。
        let html = if is_plain_text(fetched.content_type.as_deref()) {
            format!("<pre>{}</pre>", escape_html(&decoded.text))
        } else {
            decoded.text
        };
        self.push_history(HistoryEntry {
            url: fetched.url,
            status: fetched.status,
            html: Arc::from(html),
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
                        let current = self.current()?.snapshot.url.clone();
                        // 同じ文書の中のフラグメントへのリンクは再取得しない（入力内容と、POST の結果ページを保つ）。
                        if same_document(&current, &href) {
                            return Ok(self.current()?.snapshot.clone());
                        }
                        let mut req = LoadRequest::get(href);
                        req.initiator = Some(current);
                        self.load(req, jar).await
                    }
                    ElementKind::Checkbox | ElementKind::Radio => {
                        let field = self.enabled_field(index)?;
                        let state = self.toggle(field)?;
                        self.rerender(state).await
                    }
                    ElementKind::Button { submits: true } => {
                        let field = self.enabled_field(index)?;
                        let req = self.submission(field, Some(field))?;
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
                let field = self.enabled_field(index)?;
                let mut state = self.current()?.state.clone();
                state.entry(field).or_default().value = Some(value);
                self.rerender(state).await
            }
            Action::Select(id, wanted) => {
                let (index, kind, _) = self.element(id)?;
                let ElementKind::Select { options, multiple } = kind else {
                    return Err(BrowserError::NotInteractive("選択"));
                };
                let field = self.enabled_field(index)?;
                let value = options
                    .iter()
                    .find(|(v, label)| *v == wanted || *label == wanted)
                    .map(|(v, _)| v.clone())
                    .ok_or(BrowserError::NoSuchOption)?;
                let cur = self.current()?;
                let mut state = cur.state.clone();
                let selected = if multiple {
                    // 複数選択は選ぶたびに切り替える。
                    let mut now = match &cur.fields[field].kind {
                        FieldKind::Select { values } => values.clone(),
                        _ => Vec::new(),
                    };
                    match now.iter().position(|v| *v == value) {
                        Some(p) => {
                            now.remove(p);
                        }
                        None => now.push(value),
                    }
                    now
                } else {
                    vec![value]
                };
                state.entry(field).or_default().selected = Some(selected);
                self.rerender(state).await
            }
            Action::Submit(id) => {
                let (index, _, _) = self.element(id)?;
                let field = self.enabled_field(index)?;
                let req = self.submission(field, None)?;
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

    fn plan(&self, action: &Action) -> Result<Option<PlannedRequest>, BrowserError> {
        match action {
            Action::Click(id) => {
                let (index, kind, _) = self.element(*id)?;
                match kind {
                    ElementKind::Link { href } => {
                        let current = self.current()?.snapshot.url.clone();
                        if same_document(&current, &href) {
                            return Ok(None);
                        }
                        let mut request = LoadRequest::get(href);
                        request.initiator = Some(current);
                        Ok(Some(PlannedRequest {
                            request,
                            masked_fields: Vec::new(),
                        }))
                    }
                    ElementKind::Button { submits: true } => {
                        let field = self.enabled_field(index)?;
                        self.planned(field, Some(field)).map(Some)
                    }
                    _ => Ok(None),
                }
            }
            Action::Submit(id) => {
                let (index, _, _) = self.element(*id)?;
                let field = self.enabled_field(index)?;
                self.planned(field, None).map(Some)
            }
            Action::Fill(..) | Action::Select(..) | Action::Back => Ok(None),
        }
    }

    fn next_generation(&self) -> u32 {
        self.generation + 1
    }
}

#[cfg(test)]
mod content_type_tests {
    use super::is_htmlish;

    #[test]
    fn content_types() {
        assert!(is_htmlish(Some("text/html; charset=utf-8"), b""));
        assert!(is_htmlish(Some("Application/XHTML+XML"), b""));
        assert!(!is_htmlish(
            Some("application/vnd.openxmlformats-officedocument.wordprocessingml.document"),
            b"PK"
        ));
        assert!(!is_htmlish(Some("application/pdf"), b"%PDF"));
        assert!(is_htmlish(None, b"\xef\xbb\xbf  <html>"));
        assert!(!is_htmlish(None, b"%PDF-1.7"));
        assert!(!is_htmlish(None, b"PK\x03\x04"));
        assert!(!is_htmlish(None, b""));
    }
}
