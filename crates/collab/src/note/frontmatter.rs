//! frontmatter 型軽量メタデータ（Task 11P.2・design §4.8.1）。
//!
//! Notion 型プロパティ DB はやらない（将来トラック #248）。メタデータは
//! タイトル・アイコン・タグ・任意 key-value・紐付く thread_id のフラットな集合で、
//! YAML frontmatter へ**往復可能**に落とす。値は常に文字列（型検証・集計はしない）。
//!
//! 正規形（本モジュールが出力する形式・往復保証の対象）:
//! - 区切りは `---` 行。キー順は title / icon / tags / thread_id / その他（辞書順）。
//! - 値は JSON 文字列としてクォート（YAML の double-quoted scalar と互換）。
//! - tags は JSON 文字列の flow list `["a", "b"]`。
//!
//! 外部生成の frontmatter は寛容にパースする（`key: value` の素朴な行分割・
//! クォート無し値は生文字列扱い）。パース不能な行は任意 kv として原文保持し、
//! 情報を落とさない（fail-open だが表示専用値のため安全）。

use std::collections::BTreeMap;
use std::fmt::Write as _;

/// ノートのメタデータ（frontmatter ⇔ Yjs Map "meta" の共通表現）。
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct NoteMeta {
    pub title: Option<String>,
    pub icon: Option<String>,
    pub tags: Vec<String>,
    /// 紐付くチャットスレッド（11P.5。スレッド共有とは別 ReBAC・id 参照のみ）。
    pub thread_id: Option<String>,
    /// 任意 key-value（文字列のみ・キー辞書順で正規化）。
    pub extra: BTreeMap<String, String>,
}

impl NoteMeta {
    pub fn is_empty(&self) -> bool {
        self.title.is_none()
            && self.icon.is_none()
            && self.tags.is_empty()
            && self.thread_id.is_none()
            && self.extra.is_empty()
    }
}

/// 予約キー（extra に混ぜない）。
const RESERVED: [&str; 4] = ["title", "icon", "tags", "thread_id"];

/// md 全文から frontmatter を切り出し、(メタ, 本文) を返す。
pub fn split_frontmatter(md: &str) -> (NoteMeta, &str) {
    let Some(rest) = md
        .strip_prefix("---\n")
        .or_else(|| md.strip_prefix("---\r\n"))
    else {
        return (NoteMeta::default(), md);
    };
    // 終端 `---` 行を探す（見つからなければ frontmatter 無しとして全文を本文に）。
    let mut offset = 0usize;
    for line in rest.split_inclusive('\n') {
        if line.trim_end_matches(['\r', '\n']) == "---" {
            let meta_src = &rest[..offset];
            let body = &rest[offset + line.len()..];
            let body = body.strip_prefix('\n').unwrap_or(body);
            return (parse_meta_lines(meta_src), body);
        }
        offset += line.len();
    }
    (NoteMeta::default(), md)
}

/// メタと本文から md 全文を組み立てる（メタが空なら frontmatter を出さない）。
pub fn compose_markdown(meta: &NoteMeta, body: &str) -> String {
    if meta.is_empty() {
        return body.to_string();
    }
    let mut out = String::from("---\n");
    if let Some(title) = &meta.title {
        let _ = writeln!(out, "title: {}", json_str(title));
    }
    if let Some(icon) = &meta.icon {
        let _ = writeln!(out, "icon: {}", json_str(icon));
    }
    if !meta.tags.is_empty() {
        let tags: Vec<String> = meta.tags.iter().map(|t| json_str(t)).collect();
        let _ = writeln!(out, "tags: [{}]", tags.join(", "));
    }
    if let Some(thread_id) = &meta.thread_id {
        let _ = writeln!(out, "thread_id: {}", json_str(thread_id));
    }
    for (key, value) in &meta.extra {
        let _ = writeln!(out, "{}: {}", sanitize_key(key), json_str(value));
    }
    out.push_str("---\n\n");
    out.push_str(body);
    out
}

fn parse_meta_lines(src: &str) -> NoteMeta {
    let mut meta = NoteMeta::default();
    for line in src.lines() {
        let line = line.trim_end();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let Some((key, raw)) = line.split_once(':') else {
            continue;
        };
        let key = key.trim();
        let raw = raw.trim();
        match key {
            "title" => meta.title = Some(parse_scalar(raw)),
            "icon" => meta.icon = Some(parse_scalar(raw)),
            "thread_id" => meta.thread_id = Some(parse_scalar(raw)),
            "tags" => meta.tags = parse_tags(raw),
            _ if key.is_empty() => {}
            _ => {
                meta.extra.insert(key.to_string(), parse_scalar(raw));
            }
        }
    }
    meta
}

/// スカラー値: JSON 文字列ならデコード、それ以外は原文（外部 frontmatter への寛容パース）。
fn parse_scalar(raw: &str) -> String {
    if raw.starts_with('"') {
        if let Ok(serde_json::Value::String(s)) = serde_json::from_str(raw) {
            return s;
        }
    }
    // 単一クォートの素朴な除去（YAML single-quoted の最低限互換）。
    if raw.len() >= 2 && raw.starts_with('\'') && raw.ends_with('\'') {
        return raw[1..raw.len() - 1].replace("''", "'");
    }
    raw.to_string()
}

/// tags: flow list `[a, "b"]` またはカンマ区切りを許容する。
fn parse_tags(raw: &str) -> Vec<String> {
    let inner = raw
        .strip_prefix('[')
        .and_then(|s| s.strip_suffix(']'))
        .unwrap_or(raw);
    inner
        .split(',')
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(parse_scalar)
        .collect()
}

/// 値を JSON 文字列（= YAML double-quoted scalar 互換）へ。
fn json_str(value: &str) -> String {
    serde_json::to_string(value).unwrap_or_else(|_| format!("\"{value}\""))
}

/// extra キーの正規化: 行構造を壊す文字を除去する（値は JSON クォートで安全）。
fn sanitize_key(key: &str) -> String {
    let cleaned: String = key
        .chars()
        .filter(|c| !matches!(c, ':' | '\n' | '\r'))
        .collect();
    let trimmed = cleaned.trim();
    if trimmed.is_empty() || RESERVED.contains(&trimmed) {
        format!("kv_{trimmed}")
    } else {
        trimmed.to_string()
    }
}
