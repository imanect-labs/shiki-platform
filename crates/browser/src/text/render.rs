//! HTML → 共通スナップショット＋フォームの状態（テキストエンジン・docs/agent/browser.md §8）。
//!
//! JS 無効として解析する（dom_query は scripting 無効で解析するので `<noscript>` の中身が見える）。
//! 操作できる要素に ref を振り、行単位のテキストに埋め込む。DOM 構築と走査は CPU バウンドなので、
//! 呼び出し側が `spawn_blocking` へ逃がす（PIT-23）。

use std::collections::{HashMap, HashSet};

use dom_query::{Document, NodeId, NodeRef};
use url::Url;

use crate::snapshot::{ElementInfo, ElementKind, EngineKind, NeedsJs, RefId};

/// 「本文をスクリプトで描く」と判定するリンク以外の本文の文字数（実サイト測定で決めた・browser.md §10.2）。
pub(crate) const NEEDS_JS_THRESHOLD: usize = 200;
/// 走査するノード数の上限（敵対的な巨大 DOM で止まらないように）。
const MAX_NODES: usize = 300_000;
/// ref を振る要素数の上限。
const MAX_ELEMENTS: usize = 3_000;
/// 降りる深さの上限。描画は再帰するので、深い入れ子でスタックを溢れさせない
/// （スタック溢れは panic でなく abort になり、プロセスごと落ちる）。実在のページは 100 段に満たない。
const MAX_DEPTH: u32 = 256;
/// 扱う URL の長さの上限（バイト）。ブラウザの実用上の上限に合わせる。
const MAX_URL_BYTES: usize = 8 * 1024;
/// ラベル・リンク・ボタンの表示名の上限（文字）。入れ子のラベルで二乗の連結をさせない。
const MAX_LABEL_CHARS: usize = 200;

/// フォームの送信方式。
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Form {
    /// 送り先。None は明示された action を解決できない（`javascript:` 等）＝送信しない。
    pub action: Option<Url>,
    pub post: bool,
    /// `method="dialog"`（ダイアログを閉じるだけで HTTP 要求を送らない）。テキストエンジンでは送信しない。
    pub dialog: bool,
    /// `novalidate`（送信前の制約検証＝`required` の確認をしない）。
    pub novalidate: bool,
    /// `accept-charset` の最初の対応する文字コード（無ければ文書の文字コードで送る）。
    pub accept_charset: Option<&'static encoding_rs::Encoding>,
    pub enctype: String,
    id: Option<String>,
}

/// 送信に載る項目（文書順）。CSS で隠れた項目も持つ（ブラウザは見た目によらず送る）。
// HTML の部品の属性（disabled / readonly / required）と検証状態をそのまま写す。
#[allow(clippy::struct_excessive_bools)]
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Field {
    pub form: Option<usize>,
    pub name: String,
    pub kind: FieldKind,
    /// ref を振った要素の番号（hidden・見えない項目は無し）。
    pub element: Option<u32>,
    /// `disabled`（祖先の `<fieldset disabled>` を含む）。送信に載せず、押せない。
    pub disabled: bool,
    /// `readonly`。値は送るが、利用者（モデル）は書き換えられない。
    pub readonly: bool,
    /// `required`（未入力なら送信しない）。
    pub required: bool,
    /// セレクトの未入力（何も選ばれていない・placeholder だけが選ばれている）。他の部品では使わない。
    pub value_missing: bool,
    /// セレクトで選ばれている選択肢の位置（有効な選択肢の並びでの位置）。複数選択の切り替えに使う。
    pub selected_options: Vec<usize>,
    /// Enter による暗黙の送信を阻む入力欄（text・password・number 等）か。
    pub blocks_implicit: bool,
    form_attr: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum FieldKind {
    Text {
        value: String,
        /// パスワード欄（値を承認ダイアログで伏せる）。
        secret: bool,
    },
    Hidden {
        value: String,
    },
    /// 選ばれている value（`multiple` でなければ 0〜1 個）。
    Select {
        values: Vec<String>,
    },
    Check {
        value: String,
        checked: bool,
        radio: bool,
    },
    Button {
        value: String,
        submits: bool,
        /// `<input type="image">`。押すと name=value ではなく座標（`name.x` / `name.y`）を送る。
        image: bool,
        /// 押したボタンの `formaction` / `formmethod` / `formenctype`（フォームの値を上書きする）。
        overrides: Option<Box<SubmitOverrides>>,
    },
}

/// 送信ボタンが持つ、フォームの送信方式の上書き（HTML 標準の form submission）。
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(crate) struct SubmitOverrides {
    pub action: Option<Url>,
    /// `formaction` が明示されているのに解決できない（`javascript:` 等）＝送信しない。
    pub bad_action: bool,
    pub post: Option<bool>,
    /// `formmethod="dialog"`。
    pub dialog: Option<bool>,
    /// `formnovalidate`（このボタンで送るときは制約検証をしない）。
    pub novalidate: bool,
    pub enctype: Option<String>,
}

/// 利用者が入れた値（ref の番号ごと）。描画し直しても保つ。
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(crate) struct FieldState {
    pub value: Option<String>,
    pub checked: Option<bool>,
    /// セレクトで選んだ value（`multiple` でなければ 1 個）。
    /// 選んだ選択肢の位置（スナップショットに出した有効な選択肢の並びでの位置）。値は重複し得るので位置で持つ。
    pub selected: Option<Vec<usize>>,
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
    /// 文書の URL（action を省いたフォームの送り先）。
    base: &'a Url,
    /// リンクと明示的な action の解決の基準（文書の最初の `<base href>`。無ければ文書の URL）。
    link_base: Url,
    state: &'a HashMap<usize, FieldState>,
    generation: u32,
    labels_for: HashMap<String, String>,
    lines: Vec<String>,
    cur: String,
    visible_chars: usize,
    /// これまでに描いた本文のバイト数と、その上限（`max_chars` から決める）。
    out_bytes: usize,
    budget: usize,
    /// 上限に達して以降のテキストを捨てたか。
    overflow: bool,
    link_chars: usize,
    visited: usize,
    elements: Vec<ElementInfo>,
    forms: Vec<Form>,
    fields: Vec<Field>,
    /// 文書内のフォームの id（`form="id"` の送信ボタンが送れるかの判定に使う）。
    form_ids: HashSet<String>,
    /// CSS 等で見えない部分木の中にいる深さ（項目は集めるが、表示も ref もしない）。
    invisible: u32,
    /// 同じグループの後のラジオに外される `checked`（[`superseded_radios`]）。
    superseded_radios: HashSet<NodeId>,
    /// 開いている表ごとの、表の中で開いたフォーム（form element pointer の代わり）。
    table_forms: Vec<Option<usize>>,
    /// `<pre>` の中にいる深さ（改行と字下げを保つ）。
    pre: u32,
    /// `<fieldset disabled>` の中にいる深さ。
    disabled: u32,
    /// いまの深さ（[`MAX_DEPTH`] で打ち切る）。
    depth: u32,
}

/// 同じラジオグループ（フォームの所有者と名前が同じ）に `checked` が複数あるとき、最後のもの以外。
///
/// HTML 標準では、ラジオが checked になると同じグループの他のラジオが外れる。解析時も同じなので、
/// 文書順で最後の `checked` だけが選ばれた状態になる。所有者は `form` 属性（あればそれが優先）か祖先のフォーム。
fn superseded_radios(doc: &Document) -> HashSet<NodeId> {
    let form_by_id: HashMap<String, NodeId> = doc
        .select("form[id]")
        .nodes()
        .iter()
        .rev()
        .filter_map(|f| f.attr("id").map(|i| (i.to_string(), f.id)))
        .collect();
    let mut last: HashMap<(Option<NodeId>, String), NodeId> = HashMap::new();
    let mut superseded = HashSet::new();
    for r in doc.select("input[checked]").nodes() {
        let is_radio = r
            .attr("type")
            .is_some_and(|t| t.eq_ignore_ascii_case("radio"));
        let Some(name) = r.attr("name").filter(|n| !n.is_empty()) else {
            continue;
        };
        if !is_radio {
            continue;
        }
        let owner = match r.attr("form") {
            Some(id) => form_by_id.get(&*id).copied(),
            None => r
                .ancestors_it(None)
                .find(|a| a.node_name().is_some_and(|n| &*n == "form"))
                .map(|f| f.id),
        };
        if let Some(prev) = last.insert((owner, name.to_string()), r.id) {
            superseded.insert(prev);
        }
    }
    superseded
}

/// HTML を描画する。`max_chars` を超えた本文は切る（文字数で数える）。
pub(crate) fn render(
    html: &str,
    base: &Url,
    generation: u32,
    max_chars: usize,
    state: &HashMap<usize, FieldState>,
) -> Rendered {
    // 入れ子が深すぎる文書は、解析器へ渡す前に切る（二乗の解析時間を防ぐ・guard.rs）。
    let (html, clipped) = super::guard::clip_depth(html, super::guard::MAX_PARSE_DEPTH);
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
    let form_ids = doc
        .select("form[id]")
        .nodes()
        .iter()
        .filter_map(|f| f.attr("id").map(|i| i.to_string()))
        .collect();
    let link_base = doc
        .select("base[href]")
        .first()
        .attr("href")
        .and_then(|h| base.join(h.trim()).ok())
        .filter(|u| matches!(u.scheme(), "http" | "https"))
        .unwrap_or_else(|| base.clone());
    let superseded_radios = superseded_radios(&doc);
    let mut w = Walker {
        superseded_radios,
        base,
        link_base,
        state,
        generation,
        labels_for,
        lines: Vec::new(),
        cur: String::new(),
        pre: 0,
        table_forms: Vec::new(),
        visible_chars: 0,
        link_chars: 0,
        visited: 0,
        elements: Vec::new(),
        forms: Vec::new(),
        fields: Vec::new(),
        form_ids,
        invisible: 0,
        disabled: 0,
        depth: 0,
        out_bytes: 0,
        // 文字数の上限を UTF-8 で最大 4 バイトとして見積もる（全部描いてから切るのでは確保が先に膨らむ）。
        budget: max_chars.saturating_mul(4).saturating_add(4096),
        overflow: false,
    };
    let root = doc.body().unwrap_or_else(|| doc.root());
    w.walk(&root, None, None);
    w.flush("");
    w.bind_form_attrs();

    let nonlink = w.visible_chars.saturating_sub(w.link_chars);
    let needs_js = (has_script && nonlink < NEEDS_JS_THRESHOLD).then_some(NeedsJs::ScriptRendered);
    let mut text = w.lines.join("\n");
    let truncated = clipped || w.overflow || text.chars().count() > max_chars;
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
    fn collect(node: &NodeRef<'_>, out: &mut String, depth: u32) {
        if depth > MAX_DEPTH {
            return;
        }
        for c in node.children_it(false) {
            if out.len() > MAX_LABEL_CHARS * 4 {
                return;
            }
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
                _ => collect(&c, out, depth + 1),
            }
        }
    }
    let mut out = String::new();
    collect(node, &mut out, 0);
    collapse(&out).chars().take(MAX_LABEL_CHARS).collect()
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
        if self.invisible > 0 || self.full() {
            return;
        }
        if self.pre > 0 {
            // 整形済みテキストは空白を詰めず、改行ごとに行を分ける。
            let s = s.replace("\r\n", "\n");
            let mut parts = s.split('\n');
            if let Some(first) = parts.next() {
                self.visible_chars += first.chars().count();
                self.cur.push_str(first);
            }
            for part in parts {
                // 行を積むたびに本文の上限を確かめる（短い行を大量に並べた pre で溜め込まない）。
                if self.full() {
                    return;
                }
                let line = std::mem::take(&mut self.cur);
                let line = line.trim_end().to_string();
                self.out_bytes += line.len() + 1;
                self.lines.push(line);
                self.visible_chars += part.chars().count();
                self.cur.push_str(part);
            }
            return;
        }
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
        if self.invisible > 0 || self.full() {
            return;
        }
        if !self.cur.is_empty() && !self.cur.ends_with(' ') {
            self.cur.push(' ');
        }
        self.cur.push_str(t);
        self.cur.push(' ');
    }

    fn flush(&mut self, prefix: &str) {
        // 整形済みテキストの字下げは残す。
        let line = if self.pre > 0 {
            self.cur.trim_end()
        } else {
            self.cur.trim()
        };
        if !line.is_empty() {
            let line = format!("{prefix}{line}");
            self.out_bytes += line.len() + 1;
            self.lines.push(line);
        }
        self.cur.clear();
    }

    fn resolve(&self, href: &str) -> Option<Url> {
        // 長すぎる URL は扱わない（長い base と多数の相対リンクで、リンクごとに巨大な URL を複製させない）。
        if self.link_base.as_str().len() + href.len() > MAX_URL_BYTES {
            return None;
        }
        let u = self.link_base.join(href.trim()).ok()?;
        (matches!(u.scheme(), "http" | "https") && u.as_str().len() <= MAX_URL_BYTES).then_some(u)
    }

    /// 本文の蓄積が上限に達したか（達したら以降のテキストを捨て、切り詰めたことにする）。
    fn full(&mut self) -> bool {
        if self.out_bytes + self.cur.len() >= self.budget {
            self.overflow = true;
        }
        self.overflow
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
        if self.invisible > 0 || self.elements.len() >= MAX_ELEMENTS {
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

    fn walk(&mut self, node: &NodeRef<'_>, form: Option<usize>, wrap: Option<&str>) {
        self.visited += 1;
        if self.visited > MAX_NODES || self.depth > MAX_DEPTH {
            return;
        }
        self.depth += 1;
        self.walk_inner(node, form, wrap);
        self.depth -= 1;
    }

    fn walk_inner(&mut self, node: &NodeRef<'_>, form: Option<usize>, wrap: Option<&str>) {
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
            "script" | "style" | "template" | "head" | "svg" | "iframe" | "object" | "datalist"
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
            // 見えない部分木も、送信に載る項目だけは集める（CSRF トークン等を CSS で隠すサイトがある）。
            self.invisible += 1;
            self.descend(&name, node, form, wrap);
            self.invisible -= 1;
            return;
        }
        if self.leaf(&name, node, form, wrap) {
            return;
        }
        self.descend(&name, node, form, wrap);
    }

    /// 子へ降りる（ブロックの改行・表のセル区切り・フォーム・ラベル・`<fieldset disabled>`）。
    fn descend(&mut self, name: &str, node: &NodeRef<'_>, form: Option<usize>, wrap: Option<&str>) {
        if self.invisible > 0 && self.leaf(name, node, form, wrap) {
            return;
        }

        let block = is_block(name);
        if block {
            self.flush("");
        }
        if matches!(name, "td" | "th") && !self.cur.is_empty() {
            self.cur.push_str(" | ");
        }
        let mut form = form;
        if name == "form" {
            // 省略・空なら文書の URL。明示されていて解決できない（`javascript:` 等）なら送信させない
            // （現在のページへの送信にすり替えない）。
            let action = match node.attr("action").filter(|a| !a.trim().is_empty()) {
                Some(a) => self.resolve(&a),
                None => Some(self.base.clone()),
            };
            self.forms.push(Form {
                action,
                post: node
                    .attr("method")
                    .is_some_and(|m| m.eq_ignore_ascii_case("post")),
                dialog: node
                    .attr("method")
                    .is_some_and(|m| m.eq_ignore_ascii_case("dialog")),
                novalidate: node.has_attr("novalidate"),
                // 属性が無ければ文書の文字コード。あるのに対応する候補が無ければ UTF-8（HTML 標準）。
                accept_charset: node.attr("accept-charset").map(|a| {
                    a.split_ascii_whitespace()
                        .find_map(|l| encoding_rs::Encoding::for_label(l.as_bytes()))
                        .unwrap_or(encoding_rs::UTF_8)
                }),
                enctype: node.attr("enctype").map_or_else(
                    || "application/x-www-form-urlencoded".to_string(),
                    |e| e.to_ascii_lowercase(),
                ),
                id: node.attr("id").map(|i| i.to_string()),
            });
            form = Some(self.forms.len() - 1);
            // 表の中（table・行グループ・行の直下）で開いたフォームは、解析器がすぐ閉じるが form element
            // pointer として残り、同じ表の後続の部品がそのフォームに属する（HTML 標準の "form" in table）。
            let in_table_part = node.parent().is_some_and(|p| {
                p.node_name()
                    .is_some_and(|n| matches!(&*n, "table" | "tbody" | "thead" | "tfoot" | "tr"))
            });
            if in_table_part {
                if let Some(slot) = self.table_forms.last_mut() {
                    *slot = form;
                }
            }
        }
        let is_table = name == "table";
        if is_table {
            self.table_forms.push(None);
        }
        let label = (name == "label").then(|| label_text(node));
        let wrap = label.as_deref().or(wrap);
        let fieldset_disabled = name == "fieldset" && node.has_attr("disabled");
        if fieldset_disabled {
            self.disabled += 1;
        }
        let pre = matches!(name, "pre" | "listing" | "plaintext");
        if pre {
            self.pre += 1;
        }
        let mut legend_seen = false;
        for c in node.children_it(false) {
            // 表の中で開いたフォームがあれば、所有者の無い後続の部品はそのフォームに属する。
            let form = form.or_else(|| self.table_forms.last().copied().flatten());
            // `<fieldset disabled>` の最初の `<legend>` の中は、その fieldset による無効化を受けない（HTML 標準）。
            let first_legend = fieldset_disabled
                && !legend_seen
                && c.is_element()
                && c.node_name().is_some_and(|n| &*n == "legend");
            if first_legend {
                legend_seen = true;
                self.disabled -= 1;
                self.walk(&c, form, wrap);
                self.disabled += 1;
            } else {
                self.walk(&c, form, wrap);
            }
        }
        if fieldset_disabled {
            self.disabled -= 1;
        }
        if is_table {
            self.table_forms.pop();
        }
        if block {
            self.flush(line_prefix(name));
        }
        if pre {
            self.pre -= 1;
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
                    // リンクの中の送信項目（hidden の CSRF トークン等）も集める。表示と ref は付けない。
                    self.invisible += 1;
                    for c in node.children_it(false) {
                        self.walk(&c, form, wrap);
                    }
                    self.invisible -= 1;
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
                // ボタンの中の送信項目（hidden 等）も集める。表示と ref は付けない。
                self.invisible += 1;
                for c in node.children_it(false) {
                    self.walk(&c, form, wrap);
                }
                self.invisible -= 1;
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
        if self.invisible > 0 {
            return;
        }
        let n = label.chars().count();
        self.visible_chars += n;
        self.link_chars += n;
        match self.push_element(ElementKind::Link { href: href.clone() }, &label) {
            Some(id) => self.token(&format!("[{id} link \"{label}\" → {href}]")),
            None => self.text(&label),
        }
    }

    /// `form="id"` 属性で外のフォームに属する項目を結び付ける（フォームが後に現れてもよい）。
    fn bind_form_attrs(&mut self) {
        // id → 最初のフォームの位置（項目ごとに全フォームを探して二乗にしない）。
        let mut by_id: HashMap<&str, usize> = HashMap::new();
        for (i, form) in self.forms.iter().enumerate() {
            if let Some(id) = form.id.as_deref() {
                by_id.entry(id).or_insert(i);
            }
        }
        for f in &mut self.fields {
            if let Some(target) = &f.form_attr {
                f.form = by_id.get(target.as_str()).copied();
            }
        }
    }
}

fn button_token(id: RefId, label: &str, submits: bool, disabled: bool) -> String {
    let note = if disabled {
        " (無効)"
    } else if submits {
        ""
    } else {
        " (要JS)"
    };
    format!("[{id} button \"{label}\"{note}]")
}

#[path = "controls.rs"]
pub(super) mod controls;

#[cfg(test)]
#[path = "render_tests.rs"]
mod tests;
