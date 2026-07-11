//! md シリアライズ往復の**契約テスト**（Task 11P.2・PIT-37③）。
//!
//! 往復保証対象（design §4.8.1 / phase-11-pre Task 11P.2 で列挙した要素）ごとに
//! 正準 md を固定し、次の 2 経路で安定することを検証する:
//! 1. `normalize_markdown`（md → AST → md）が恒等（正準形の不動点）
//! 2. Yjs 往復（md → Yjs → md）が恒等
//!
//! 生 HTML は往復対象外で、**一度の正規化でコードブロック/リテラルへ縮退**し、
//! 以後は安定する（stored XSS 遮断・Task 11P.6 の契約）。

// テストコード: pedantic/安全系 lint は本番コードのみ厳格化する方針のため許容する。
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::print_stdout,
    clippy::print_stderr,
    clippy::pedantic,
    clippy::cognitive_complexity
)]

use collab::note::{doc_to_markdown, import_markdown, normalize_markdown};
use yrs::Doc;

/// 正準 md が md→AST→md でも md→Yjs→md でも不動点であることを検証する。
fn assert_canonical_roundtrip(name: &str, canonical: &str) {
    let normalized = normalize_markdown(canonical);
    assert_eq!(
        normalized, canonical,
        "[{name}] md→AST→md が正準形を保つこと"
    );
    let doc = Doc::new();
    import_markdown(&doc, canonical);
    let via_yjs = doc_to_markdown(&doc);
    assert_eq!(via_yjs, canonical, "[{name}] md→Yjs→md が正準形を保つこと");
}

#[test]
fn headings_and_paragraphs() {
    assert_canonical_roundtrip(
        "headings",
        "# 見出し1\n\n## 見出し2\n\n### 見出し3\n\n本文の段落です。\n\n次の段落。\n",
    );
}

#[test]
fn inline_marks_and_links() {
    assert_canonical_roundtrip(
        "marks",
        "これは **太字** と *斜体* と ~~打消~~ と `inline code` の例。\n\n[シキのリンク](https://example.com/docs) を含む段落。\n",
    );
    // 複合マーク（太字＋斜体）とリンク内マーク。
    assert_canonical_roundtrip("nested-marks", "***強調斜体*** の段落。\n");
}

#[test]
fn hard_break() {
    assert_canonical_roundtrip("hardbreak", "1行目\\\n2行目\n");
}

#[test]
fn bullet_and_ordered_lists() {
    assert_canonical_roundtrip("bullet", "- 項目1\n- 項目2\n  - 子項目\n- 項目3\n");
    assert_canonical_roundtrip("ordered", "1. 一\n2. 二\n3. 三\n");
    assert_canonical_roundtrip("ordered-start", "3. 三から\n4. 四\n");
}

#[test]
fn task_list() {
    assert_canonical_roundtrip("task", "- [x] 完了タスク\n- [ ] 未完タスク\n");
}

#[test]
fn table_gfm() {
    assert_canonical_roundtrip(
        "table",
        "| 名前 | 値 |\n| --- | --- |\n| alpha | 1 |\n| beta | 2 |\n",
    );
}

#[test]
fn code_block_with_language() {
    assert_canonical_roundtrip(
        "codeblock",
        "```rust\nfn main() {\n    println!(\"hi\");\n}\n```\n",
    );
    // 言語なし。
    assert_canonical_roundtrip("codeblock-plain", "```\nplain text\n```\n");
}

#[test]
fn embed_reference_fence() {
    assert_canonical_roundtrip(
        "embed",
        "```shiki-embed\n{\"kind\":\"drive\",\"node_id\":\"00000000-0000-0000-0000-000000000001\"}\n```\n",
    );
}

#[test]
fn blockquote_multi_paragraph() {
    assert_canonical_roundtrip("quote", "> 引用の一段落目。\n>\n> 二段落目。\n");
}

#[test]
fn horizontal_rule() {
    assert_canonical_roundtrip("hr", "上の段落。\n\n---\n\n下の段落。\n");
}

#[test]
fn frontmatter_full() {
    assert_canonical_roundtrip(
        "frontmatter",
        "---\ntitle: \"設計メモ\"\nicon: \"📝\"\ntags: [\"design\", \"note\"]\nthread_id: \"th-123\"\nowner: \"alice\"\n---\n\n# 本文\n\nfrontmatter 付きノート。\n",
    );
}

#[test]
fn mixed_document() {
    assert_canonical_roundtrip(
        "mixed",
        // タスク/通常項目の混在リストは連続同種ごとに分割されるのが正規形
        // （TipTap taskList は taskItem のみ許容するため）。
        "---\ntitle: \"混在ドキュメント\"\n---\n\n# 全部入り\n\n序文の段落。\n\n- リスト\n  - ネスト\n\n- [ ] タスク混在は別リスト\n\n```shiki-embed\n{\"kind\":\"genui\"}\n```\n\n| a | b |\n| --- | --- |\n| 1 | 2 |\n\n> 引用で締める。\n",
    );
}

// ---------------------------------------------------------------------------
// 生 HTML の縮退（往復対象外・一度の正規化で安定＝XSS を残さない）
// ---------------------------------------------------------------------------

/// 正規化が 1 回で安定し（冪等）、script が実行可能な形で残らないことを検証する。
fn assert_degrades_safely(name: &str, input: &str) {
    let once = normalize_markdown(input);
    let twice = normalize_markdown(&once);
    assert_eq!(once, twice, "[{name}] 正規化が冪等であること");
    // Yjs 経由でも同じ縮退結果になる。
    let doc = Doc::new();
    import_markdown(&doc, input);
    assert_eq!(doc_to_markdown(&doc), once, "[{name}] Yjs 経由でも同じ縮退");
}

#[test]
fn raw_html_block_degrades_to_code_block() {
    let input = "<script>alert('xss')</script>\n";
    let once = normalize_markdown(input);
    assert!(
        once.starts_with("```html\n"),
        "ブロック HTML は html コードブロックへ縮退すること: {once}"
    );
    assert!(once.contains("<script>alert('xss')</script>"));
    assert_degrades_safely("html-block", input);
}

#[test]
fn inline_html_degrades_to_literal_text() {
    let input = "文中の <img src=x onerror=alert(1)> は無害化される。\n";
    let once = normalize_markdown(input);
    // インライン HTML はエスケープ済みリテラル（< がエスケープされる）。
    assert!(
        once.contains("\\<img"),
        "インライン HTML はリテラル化: {once}"
    );
    assert_degrades_safely("html-inline", input);
}

#[test]
fn markdown_special_chars_in_text_are_escaped_stably() {
    // ユーザーが書く * や _ を含むプレーンテキストの往復安定性。
    assert_degrades_safely("specials", "a*b und a_b und [x] und `y` und <z>\n");
}
