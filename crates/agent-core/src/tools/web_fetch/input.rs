//! `web_fetch` の入力検証と Content-Type 分類（#348 / #405）。
//!
//! URL 検証は**アプリ層の一次防壁**（SSRF の素地を断つ）。ここを通っても宛先は確定せず、
//! 解決後 IP の検証＋アドレス固定（`web_fetch` 本体）が最終判定を持つ。
//!
//! Content-Type 分類は「本文としてモデルへ渡してよいか」と「どの経路で読むか」を決める。
//! バイナリを本文として渡さないのはトークンを焼かないためでもある。

use url::{Host, Url};

use super::{MAX_OFFSET, MAX_QUERY_CHARS};
use crate::tool::ToolError;

/// 検証済みの取得先（接続を固定する host/port と正規化済み URL）。
pub(super) struct FetchTarget {
    pub url: Url,
    pub host: String,
    pub port: u16,
}

/// 入力 URL を検証する（モデル/ユーザー由来＝敵対的として扱う）。
///
/// - スキームは http/https のみ（gopher/file 等を拒否）。
/// - userinfo（`user:pass@`）付きは拒否（ホスト偽装・資格情報混入の防止）。
/// - ホストは **ドットを含む公開 FQDN のみ**: IP リテラル（v4/v6）・単一ラベル名
///   （compose のサービス名 `minio` 等）・localhost/.local/.internal/.lan/.home.arpa を拒否。
///   名前が公開 IP に解決されるかは、この後の [`net_guard::ensure_all_public`] が判定する。
pub(super) fn validate_url(input: &str) -> Result<FetchTarget, ToolError> {
    let invalid = |msg: &str| ToolError::Invalid(format!("URL が不正です: {msg}"));
    let url = Url::parse(input.trim()).map_err(|e| invalid(&e.to_string()))?;
    if !matches!(url.scheme(), "http" | "https") {
        return Err(invalid("http/https のみ取得できます"));
    }
    if !url.username().is_empty() || url.password().is_some() {
        return Err(invalid("userinfo（user:pass@）付き URL は使えません"));
    }
    let host = match url.host() {
        Some(Host::Domain(d)) => d.to_ascii_lowercase(),
        Some(Host::Ipv4(_) | Host::Ipv6(_)) => {
            return Err(invalid("IP アドレス直指定は使えません"));
        }
        None => return Err(invalid("ホストがありません")),
    };
    // 内部/ローカル名を拒否（SSRF・confused-deputy の素地を断つ）。
    let forbidden_suffixes = [".local", ".internal", ".localhost", ".lan", ".home.arpa"];
    if !host.contains('.')
        || host == "localhost"
        || forbidden_suffixes.iter().any(|s| host.ends_with(s))
    {
        return Err(invalid("内部/ローカルホストは取得できません"));
    }
    let port = url
        .port_or_known_default()
        .ok_or_else(|| invalid("ポートを特定できません"))?;
    Ok(FetchTarget { url, host, port })
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
