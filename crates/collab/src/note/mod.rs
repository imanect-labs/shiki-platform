//! md ドキュメント種「ノート」（Task 11P.2・design §4.8.1）。
//!
//! **真実は Yjs ドキュメント**。md は保存時の正規化シリアライズ形式であり、
//! `md ⇔ AST ⇔ Yjs` の 2 段変換で往復する（AST が正準層・[`ast`]）。
//!
//! # 往復保証の契約(PIT-37③)
//!
//! 次の要素は「Yjs → md → Yjs → md」で正規形が安定する（contract テストで固定）:
//! 見出し(1-6)・段落・箇条書き/番号付きリスト（ネスト含む）・チェックリスト・表（GFM）・
//! コードブロック（言語タグ付き）・引用・埋め込みブロック参照（```shiki-embed フェンス）・
//! 水平線・強制改行・マーク（bold/italic/strike/code/link）・YAML frontmatter
//! （title/icon/tags/thread_id/任意 kv）。
//!
//! **md に落ちない情報**（サジェスト提案マーク・コメント・awareness・CRDT 内部参照）は
//! 往復対象外で、Yjs snapshot（collab_doc.snapshot）を正本として保全する（crate docs 参照）。
//!
//! # 外部書込の取り込み（単方向規約）
//!
//! md ファイル側の直接書込（エージェントの file write・新版アップロード等）は
//! 「インポート」としてロード時に Yjs へ取り込む（[`saver`]）。編集セッション中は
//! Yjs が真実であり続け、保存はファイルを新バージョンで上書きする（外部書込は
//! バージョン履歴に残り失われない）。逆方向（Yjs→md）は保存経路のみ。

pub mod ast;
pub mod frontmatter;
pub mod md_parse;
pub mod saver;
pub mod yjs_map;
pub mod yjs_meta;

pub use frontmatter::NoteMeta;

use yrs::{Doc, Transact};

/// ノート（md）としてこの collab 経路の対象になるファイルか。
pub fn is_note_file(name: &str) -> bool {
    name.to_lowercase().ends_with(".md")
}

/// Yjs ドキュメント全体を正規化 md（frontmatter 付き）へシリアライズする。
pub fn doc_to_markdown(doc: &Doc) -> String {
    let fragment = doc.get_or_insert_xml_fragment(yjs_map::FRAGMENT_NAME);
    let meta_map = doc.get_or_insert_map(yjs_map::META_MAP_NAME);
    let txn = doc.transact();
    let blocks = yjs_map::read_blocks(&txn, &fragment);
    let meta = yjs_meta::read_meta(&txn, &meta_map);
    frontmatter::compose_markdown(&meta, &ast::render_markdown(&blocks))
}

/// md 全文（frontmatter 含む）を Yjs ドキュメントへ**全置換**で取り込む（インポート）。
pub fn import_markdown(doc: &Doc, markdown: &str) {
    let fragment = doc.get_or_insert_xml_fragment(yjs_map::FRAGMENT_NAME);
    let meta_map = doc.get_or_insert_map(yjs_map::META_MAP_NAME);
    let (meta, body) = frontmatter::split_frontmatter(markdown);
    let blocks = md_parse::parse_markdown(body);
    let mut txn = doc.transact_mut();
    yjs_meta::write_meta(&mut txn, &meta_map, &meta);
    yjs_map::write_blocks(&mut txn, &fragment, &blocks);
}

/// md 文字列を正規形へ正規化する（md → AST → md。テスト・note_ref 保存経路で使用）。
pub fn normalize_markdown(markdown: &str) -> String {
    let (meta, body) = frontmatter::split_frontmatter(markdown);
    let blocks = md_parse::parse_markdown(body);
    frontmatter::compose_markdown(&meta, &ast::render_markdown(&blocks))
}
