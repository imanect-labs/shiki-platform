//! HTML → 共通スナップショット＋フォームの状態（テキストエンジン・docs/agent/browser.md §8）。
//!
//! JS 無効として解析する（dom_query は scripting 無効で解析するので `<noscript>` の中身が見える）。
//! 操作できる要素に ref を振り、行単位のテキストに埋め込む。DOM 構築と走査は CPU バウンドなので、
//! 呼び出し側が `spawn_blocking` へ逃がす（PIT-23）。

use std::collections::HashMap;

use dom_query::{Document, NodeRef};
use url::Url;

use crate::snapshot::{ElementInfo, ElementKind, EngineKind, NeedsJs, RefId};

/// 「本文をスクリプトで描く」と判定するリンク以外の本文の文字数（実サイト測定で決めた・browser.md §10.2）。
pub(crate) const NEEDS_JS_THRESHOLD: usize = 200;
/// 走査するノード数の上限（敵対的な巨大 DOM で止まらないように）。
const MAX_NODES: usize = 300_000;
/// ref を振る要素数の上限。
const MAX_ELEMENTS: usize = 3_000;

/// フォームの送信方式。
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Form {
    pub action: Url,
    pub post: bool,
    pub enctype: String,
    id: Option<String>,
}

/// 送信に載る項目（文書順）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Field {
    pub form: Option<usize>,
    pub name: String,
    pub kind: FieldKind,
    /// ref を振った要素の番号（hidden は無し）。
    pub element: Option<u32>,
    form_attr: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum FieldKind {
    Text {
        value: String,
    },
    Hidden {
        value: String,
    },
    Select {
        value: String,
    },
    Check {
        value: String,
        checked: bool,
        radio: bool,
    },
    Button {
        value: String,
        submits: bool,
    },
}

/// 利用者が入れた値（ref の番号ごと）。描画し直しても保つ。
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(crate) struct FieldState {
    pub value: Option<String>,
    pub checked: Option<bool>,
}

/// 描画結果。
#[derive(Debug, Clone)]
pub(crate) struct Rendered {
    pub title: String,
    pub text: String,
    pub elements: Vec<ElementInfo>,
    pub forms: Vec<Form>,
    pub fields: Vec<Field>,
    pub needs_js: Option<NeedsJs>,
    pub truncated: bool,
}

struct Walker<'a> {
    base: &'a Url,
    state: &'a HashMap<u32, FieldState>,
    generation: u32,
    labels_for: HashMap<String, String>,
    lines: Vec<String>,
    cur: String,
    visible_chars: usize,
    link_chars: usize,
    visited: usize,
    elements: Vec<ElementInfo>,
    forms: Vec<Form>,
    fields: Vec<Field>,
}

/// HTML を描画する。`max_chars` を超えた本文は切る（文字数で数える）。
pub(crate) fn render(
    html: &str,
    base: &Url,
    generation: u32,
    max_chars: usize,
    state: &HashMap<u32, FieldState>,
) -> Rendered {
    let doc = Document::from(html);
    let title = doc.select("title").first().text().trim().to_string();
    let mut labels_for = HashMap::new();
    for l in doc.select("label[for]").nodes() {
        if let Some(id) = l.attr("for") {
            labels_for.insert(id.to_string(), collapse(&label_text(l)));
        }
    }
    let has_script = doc.select("script").nodes().iter().any(|s| {
        let ty = s
            .attr("type")
            .map(|t| t.to_ascii_lowercase())
            .unwrap_or_default();
        !(ty.contains("json") || ty.contains("template"))
    });
    let mut w = Walker {
        base,
        state,
        generation,
        labels_for,
        lines: Vec::new(),
        cur: String::new(),
        visible_chars: 0,
        link_chars: 0,
        visited: 0,
        elements: Vec::new(),
        forms: Vec::new(),
        fields: Vec::new(),
    };
    let root = doc.body().unwrap_or_else(|| doc.root());
    w.walk(&root, None, None);
    w.flush("");
    w.bind_form_attrs();

    let nonlink = w.visible_chars.saturating_sub(w.link_chars);
    let needs_js = (has_script && nonlink < NEEDS_JS_THRESHOLD).then_some(NeedsJs::ScriptRendered);
    let mut text = w.lines.join("\n");
    let truncated = text.chars().count() > max_chars;
    if truncated {
        text = text.chars().take(max_chars).collect();
        text.push_str("\n…（以降は省略）");
    }
    Rendered {
        title,
        text,
        elements: w.elements,
        forms: w.forms,
        fields: w.fields,
        needs_js,
        truncated,
    }
}

fn collapse(s: &str) -> String {
    s.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// ラベル・リンク・ボタンの表示名（select / option / script の中身は含めず、画像は alt を使う）。
fn label_text(node: &NodeRef<'_>) -> String {
    fn collect(node: &NodeRef<'_>, out: &mut String) {
        for c in node.children_it(false) {
            if c.is_text() {
                out.push_str(&c.text());
                out.push(' ');
                continue;
            }
            match c.node_name().as_deref() {
                Some("select" | "option" | "script" | "style" | "textarea" | "template") => {}
                Some("img") => {
                    if let Some(alt) = c.attr("alt") {
                        out.push_str(&alt);
                        out.push(' ');
                    }
                }
                _ => collect(&c, out),
            }
        }
    }
    let mut out = String::new();
    collect(node, &mut out);
    collapse(&out)
}

fn is_hidden(node: &NodeRef<'_>) -> bool {
    if node.has_attr("hidden") || node.attr("aria-hidden").is_some_and(|v| &*v == "true") {
        return true;
    }
    let style = node
        .attr("style")
        .map(|s| s.to_ascii_lowercase().replace(' ', ""))
        .unwrap_or_default();
    style.contains("display:none") || style.contains("visibility:hidden")
}

fn is_block(name: &str) -> bool {
    matches!(
        name,
        "p" | "div"
            | "section"
            | "article"
            | "main"
            | "header"
            | "footer"
            | "nav"
            | "aside"
            | "ul"
            | "ol"
            | "li"
            | "h1"
            | "h2"
            | "h3"
            | "h4"
            | "h5"
            | "h6"
            | "table"
            | "tr"
            | "dl"
            | "dt"
            | "dd"
            | "form"
            | "details"
            | "summary"
            | "br"
            | "blockquote"
            | "pre"
            | "fieldset"
            | "noscript"
            | "body"
            | "figure"
            | "figcaption"
            | "hr"
    )
}

fn line_prefix(name: &str) -> &'static str {
    match name {
        "h1" => "# ",
        "h2" => "## ",
        "h3" | "h4" | "h5" | "h6" => "### ",
        "li" | "dt" => "- ",
        "tr" => "| ",
        "dd" => "  : ",
        "summary" => "▸ ",
        _ => "",
    }
}

impl Walker<'_> {
    fn text(&mut self, s: &str) {
        let collapsed = collapse(s);
        let leading = s.starts_with(char::is_whitespace);
        let trailing = s.ends_with(char::is_whitespace);
        if collapsed.is_empty() {
            if !s.is_empty() && !self.cur.is_empty() && !self.cur.ends_with(' ') {
                self.cur.push(' ');
            }
            return;
        }
        if leading && !self.cur.is_empty() && !self.cur.ends_with(' ') {
            self.cur.push(' ');
        }
        self.visible_chars += collapsed.chars().count();
        self.cur.push_str(&collapsed);
        if trailing {
            self.cur.push(' ');
        }
    }

    fn token(&mut self, t: &str) {
        if !self.cur.is_empty() && !self.cur.ends_with(' ') {
            self.cur.push(' ');
        }
        self.cur.push_str(t);
        self.cur.push(' ');
    }

    fn flush(&mut self, prefix: &str) {
        let line = self.cur.trim();
        if !line.is_empty() {
            self.lines.push(format!("{prefix}{line}"));
        }
        self.cur.clear();
    }

    fn resolve(&self, href: &str) -> Option<Url> {
        let u = self.base.join(href.trim()).ok()?;
        matches!(u.scheme(), "http" | "https").then_some(u)
    }

    fn label_for(&self, node: &NodeRef<'_>, wrap: Option<&str>) -> String {
        if let Some(l) = node.attr("aria-label") {
            return collapse(&l);
        }
        if let Some(l) = node.attr("id").and_then(|id| self.labels_for.get(&*id)) {
            return l.clone();
        }
        if let Some(w) = wrap.filter(|w| !w.is_empty()) {
            return w.to_string();
        }
        node.attr("placeholder")
            .or_else(|| node.attr("title"))
            .or_else(|| node.attr("name"))
            .map(|s| collapse(&s))
            .unwrap_or_default()
    }

    /// 要素に ref を振る。上限を超えたら None（テキストとしては残す）。
    fn push_element(&mut self, kind: ElementKind, label: &str) -> Option<RefId> {
        if self.elements.len() >= MAX_ELEMENTS {
            return None;
        }
        let id = RefId {
            engine: EngineKind::Text,
            generation: self.generation,
            index: self.elements.len() as u32 + 1,
        };
        self.elements.push(ElementInfo {
            id,
            kind,
            label: label.to_string(),
        });
        Some(id)
    }

    fn value(&self, id: Option<RefId>, default: String) -> String {
        id.and_then(|i| self.state.get(&i.index))
            .and_then(|s| s.value.clone())
            .unwrap_or(default)
    }

    fn checked(&self, id: Option<RefId>, default: bool) -> bool {
        id.and_then(|i| self.state.get(&i.index))
            .and_then(|s| s.checked)
            .unwrap_or(default)
    }

    fn push_field(
        &mut self,
        node: &NodeRef<'_>,
        form: Option<usize>,
        kind: FieldKind,
        element: Option<RefId>,
    ) {
        self.fields.push(Field {
            form,
            name: node.attr("name").map(|n| n.to_string()).unwrap_or_default(),
            kind,
            element: element.map(|e| e.index),
            form_attr: node.attr("form").map(|f| f.to_string()),
        });
    }

    fn walk(&mut self, node: &NodeRef<'_>, form: Option<usize>, wrap: Option<&str>) {
        self.visited += 1;
        if self.visited > MAX_NODES {
            return;
        }
        if node.is_text() {
            self.text(&node.text());
            return;
        }
        let Some(name) = node.node_name().map(|n| n.to_string()) else {
            for c in node.children_it(false) {
                self.walk(&c, form, wrap);
            }
            return;
        };
        if !node.is_element() {
            for c in node.children_it(false) {
                self.walk(&c, form, wrap);
            }
            return;
        }
        if matches!(
            name.as_str(),
            "script" | "style" | "template" | "head" | "svg" | "iframe" | "object"
        ) {
            return;
        }
        if name == "input"
            && node
                .attr("type")
                .is_some_and(|t| t.eq_ignore_ascii_case("hidden"))
        {
            let value = node
                .attr("value")
                .map(|v| v.to_string())
                .unwrap_or_default();
            self.push_field(node, form, FieldKind::Hidden { value }, None);
            return;
        }
        if is_hidden(node) {
            return;
        }
        if self.leaf(&name, node, form, wrap) {
            return;
        }

        let block = is_block(&name);
        if block {
            self.flush("");
        }
        if matches!(name.as_str(), "td" | "th") && !self.cur.is_empty() {
            self.cur.push_str(" | ");
        }
        let mut form = form;
        if name == "form" {
            let action = node
                .attr("action")
                .filter(|a| !a.trim().is_empty())
                .and_then(|a| self.resolve(&a))
                .unwrap_or_else(|| self.base.clone());
            self.forms.push(Form {
                action,
                post: node
                    .attr("method")
                    .is_some_and(|m| m.eq_ignore_ascii_case("post")),
                enctype: node.attr("enctype").map_or_else(
                    || "application/x-www-form-urlencoded".to_string(),
                    |e| e.to_ascii_lowercase(),
                ),
                id: node.attr("id").map(|i| i.to_string()),
            });
            form = Some(self.forms.len() - 1);
        }
        let label = (name == "label").then(|| label_text(node));
        let wrap = label.as_deref().or(wrap);
        for c in node.children_it(false) {
            self.walk(&c, form, wrap);
        }
        if block {
            self.flush(line_prefix(&name));
        }
    }

    /// 葉として扱う要素（画像・リンク・入力欄・ボタン）を描く。扱ったら true。
    fn leaf(
        &mut self,
        name: &str,
        node: &NodeRef<'_>,
        form: Option<usize>,
        wrap: Option<&str>,
    ) -> bool {
        match name {
            "img" => {
                if let Some(alt) = node.attr("alt") {
                    self.text(&format!(" {alt} "));
                }
                true
            }
            "a" => match node.attr("href").and_then(|h| self.resolve(&h)) {
                Some(href) => {
                    self.link(node, &href);
                    true
                }
                None => false,
            },
            "input" => {
                self.input(node, form, wrap);
                true
            }
            "textarea" => {
                self.textarea(node, form, wrap);
                true
            }
            "select" => {
                self.select(node, form, wrap);
                true
            }
            "button" => {
                self.button(node, form);
                true
            }
            _ => false,
        }
    }

    fn link(&mut self, node: &NodeRef<'_>, href: &Url) {
        let mut label = label_text(node);
        if label.is_empty() {
            label = node
                .attr("aria-label")
                .or_else(|| node.attr("title"))
                .map(|s| collapse(&s))
                .unwrap_or_default();
        }
        let n = label.chars().count();
        self.visible_chars += n;
        self.link_chars += n;
        match self.push_element(ElementKind::Link { href: href.clone() }, &label) {
            Some(id) => self.token(&format!("[{id} link \"{label}\" → {href}]")),
            None => self.text(&label),
        }
    }

    fn textarea(&mut self, node: &NodeRef<'_>, form: Option<usize>, wrap: Option<&str>) {
        let label = self.label_for(node, wrap);
        let id = self.push_element(
            ElementKind::TextBox {
                input_type: "textarea".into(),
            },
            &label,
        );
        let value = self.value(id, node.text().to_string());
        self.push_field(
            node,
            form,
            FieldKind::Text {
                value: value.clone(),
            },
            id,
        );
        if let Some(id) = id {
            self.token(&format!(
                "[{id} textbox \"{label}\" value=\"{}\"]",
                collapse(&value)
            ));
        }
    }

    fn button(&mut self, node: &NodeRef<'_>, form: Option<usize>) {
        let ty = node
            .attr("type")
            .map_or_else(|| "submit".to_string(), |t| t.to_ascii_lowercase());
        let submits = ty == "submit" && form.is_some();
        let mut label = label_text(node);
        if label.is_empty() {
            label = self.label_for(node, None);
        }
        let value = node
            .attr("value")
            .map(|v| v.to_string())
            .unwrap_or_default();
        let id = self.push_element(ElementKind::Button { submits }, &label);
        self.push_field(node, form, FieldKind::Button { value, submits }, id);
        if let Some(id) = id {
            self.token(&button_token(id, &label, submits));
        }
    }

    fn input(&mut self, node: &NodeRef<'_>, form: Option<usize>, wrap: Option<&str>) {
        let ty = node
            .attr("type")
            .map_or_else(|| "text".to_string(), |t| t.to_ascii_lowercase());
        let value = node
            .attr("value")
            .map(|v| v.to_string())
            .unwrap_or_default();
        let label = self.label_for(node, wrap);
        match ty.as_str() {
            "checkbox" | "radio" => {
                let kind = if ty == "radio" {
                    ElementKind::Radio
                } else {
                    ElementKind::Checkbox
                };
                let id = self.push_element(kind, &label);
                let checked = self.checked(id, node.has_attr("checked"));
                let value = if value.is_empty() { "on".into() } else { value };
                self.push_field(
                    node,
                    form,
                    FieldKind::Check {
                        value,
                        checked,
                        radio: ty == "radio",
                    },
                    id,
                );
                if let Some(id) = id {
                    self.token(&format!("[{id} {ty} \"{label}\" checked={checked}]"));
                }
            }
            "submit" | "image" | "button" | "reset" => {
                let submits = matches!(ty.as_str(), "submit" | "image") && form.is_some();
                let text = if value.is_empty() {
                    "送信".to_string()
                } else {
                    value.clone()
                };
                let id = self.push_element(ElementKind::Button { submits }, &text);
                self.push_field(node, form, FieldKind::Button { value, submits }, id);
                if let Some(id) = id {
                    self.token(&button_token(id, &text, submits));
                }
            }
            "file" => self.text(&format!(" （ファイル選択欄「{label}」は扱えません） ")),
            _ => {
                let id = self.push_element(
                    ElementKind::TextBox {
                        input_type: ty.clone(),
                    },
                    &label,
                );
                let value = self.value(id, value);
                self.push_field(
                    node,
                    form,
                    FieldKind::Text {
                        value: value.clone(),
                    },
                    id,
                );
                if let Some(id) = id {
                    // パスワード欄の値は常に伏せる（browser.md §4）。
                    let shown = if ty == "password" {
                        "*".repeat(value.chars().count())
                    } else {
                        value
                    };
                    self.token(&format!(
                        "[{id} textbox \"{label}\" type={ty} value=\"{shown}\"]"
                    ));
                }
            }
        }
    }

    fn select(&mut self, node: &NodeRef<'_>, form: Option<usize>, wrap: Option<&str>) {
        let label = self.label_for(node, wrap);
        let mut options = Vec::new();
        let mut selected = None;
        for d in node.descendants_it() {
            if d.node_name().is_some_and(|n| &*n == "option") {
                let text = collapse(&d.text());
                let value = d
                    .attr("value")
                    .map_or_else(|| text.clone(), |v| v.to_string());
                if d.has_attr("selected") || selected.is_none() && options.is_empty() {
                    selected = Some(value.clone());
                }
                options.push((value, text));
            }
        }
        let listed = options
            .iter()
            .map(|(v, t)| format!("{v:?}={t}"))
            .collect::<Vec<_>>()
            .join(", ");
        let id = self.push_element(ElementKind::Select { options }, &label);
        let value = self.value(id, selected.unwrap_or_default());
        self.push_field(
            node,
            form,
            FieldKind::Select {
                value: value.clone(),
            },
            id,
        );
        if let Some(id) = id {
            self.token(&format!(
                "[{id} select \"{label}\" value={value:?} options: {listed}]"
            ));
        }
    }

    /// `form="id"` 属性で外のフォームに属する項目を結び付ける（フォームが後に現れてもよい）。
    fn bind_form_attrs(&mut self) {
        for f in &mut self.fields {
            if let Some(target) = &f.form_attr {
                f.form = self
                    .forms
                    .iter()
                    .position(|form| form.id.as_deref() == Some(target));
            }
        }
    }
}

fn button_token(id: RefId, label: &str, submits: bool) -> String {
    if submits {
        format!("[{id} button \"{label}\"]")
    } else {
        format!("[{id} button \"{label}\" (要JS)]")
    }
}

#[cfg(test)]
#[path = "render_tests.rs"]
mod tests;
