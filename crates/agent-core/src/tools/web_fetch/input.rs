//! `web_fetch` の入力検証と Content-Type 分類（#348 / #405）。
//!
//! URL 検証は**アプリ層の一次防壁**（SSRF の素地を断つ）。ここを通っても宛先は確定せず、
//! 解決後 IP の検証＋アドレス固定（`web_fetch` 本体）が最終判定を持つ。
//!
//! Content-Type 分類は「本文としてモデルへ渡してよいか」と「どの経路で読むか」を決める。
//! バイナリを本文として渡さないのはトークンを焼かないためでもある。

use url::Url;

use super::{MAX_OFFSET, MAX_QUERY_CHARS};
use crate::tool::ToolError;

pub(super) use crate::tools::public_fetch::FetchTarget;

/// 入力 URL を検証する（判定は [`crate::tools::public_fetch::validate_url`] に一本化・#1044）。
pub(super) fn validate_url(input: &str) -> Result<FetchTarget, ToolError> {
    crate::tools::public_fetch::validate_url(input)
        .map_err(|msg| ToolError::Invalid(format!("URL が不正です: {msg}")))
}

/// Content-Type の型部分（`; charset=` を落として小文字化）。
pub(super) fn base_type(content_type: Option<&str>) -> String {
    content_type
        .unwrap_or("")
        .split(';')
        .next()
        .unwrap_or("")
        .trim()
        .to_ascii_lowercase()
}

/// モデルが読めるテキストか（バイナリを本文として渡さない）。
///
/// Content-Type 無しは許可する（省略するサーバが実在し、本文は文字コード判定を通すため害がない）。
pub(super) fn is_textual(content_type: Option<&str>) -> bool {
    if content_type.is_none() {
        return true;
    }
    let ct = base_type(content_type);
    ct.starts_with("text/")
        || ct.ends_with("+json")
        || ct.ends_with("+xml")
        || matches!(
            ct.as_str(),
            "application/json"
                | "application/xml"
                | "application/javascript"
                | "application/ecmascript"
                | "application/x-ndjson"
                | "application/graphql"
                | ""
        )
}

/// HTML として本文抽出に掛けるか。
pub(super) fn is_html(content_type: Option<&str>) -> bool {
    matches!(
        base_type(content_type).as_str(),
        "text/html" | "application/xhtml+xml"
    )
}

/// 先頭を見て HTML か判定する範囲（文字）。HTML5 の sniffing に倣う。
const SNIFF_CHARS: usize = 1024;

/// **Content-Type を返さない**応答が HTML か。
///
/// `is_textual(None)` は取得を許可する（省略するサーバが実在するため）が、`is_html(None)` は
/// false なので、判定をここで足さないと**生 HTML がそのままモデルへ流れる**。本 PR の中心である
/// 「生 HTML を渡さない」保証が、許可済みの応答で破れてしまう。
///
/// タグで始まらないもの（JSON・プレーンテキスト）は対象外にする（誤って抽出へ回さない）。
pub(super) fn sniff_html(text: &str) -> bool {
    let head = text.trim_start();
    if !head.starts_with('<') {
        return false;
    }
    let head: String = head
        .chars()
        .take(SNIFF_CHARS)
        .collect::<String>()
        .to_ascii_lowercase();
    head.starts_with("<!doctype html")
        || head.contains("<html")
        || head.contains("<head")
        || head.contains("<body")
}

/// 入力から検証済みのオプションを取り出す。
pub(super) struct Options {
    pub query: Option<String>,
    pub offset: usize,
}

pub(super) fn parse_options(input: &serde_json::Value) -> Options {
    let query = input
        .get("query")
        .and_then(serde_json::Value::as_str)
        .map(str::trim)
        .filter(|q| !q.is_empty())
        .map(|q| q.chars().take(MAX_QUERY_CHARS).collect::<String>());
    let offset = input
        .get("offset")
        .and_then(serde_json::Value::as_u64)
        .and_then(|v| usize::try_from(v).ok())
        .unwrap_or(0)
        .min(MAX_OFFSET);
    Options { query, offset }
}

/// URL の末尾セグメント（Docling の形式判定に効くファイル名）。
pub(super) fn file_name_of(url: &Url) -> String {
    url.path_segments()
        .and_then(|mut s| s.next_back())
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .unwrap_or("document")
        .chars()
        .take(120)
        .collect()
}
