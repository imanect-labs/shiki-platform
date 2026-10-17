//! kit 内スクリプト実行（`executescript`）と書き出し（`downloadas`）のワイヤ形式（#725）。
//!
//! 実機（Collabora 26.04）で確かめた形:
//! - 送信: `executescript <callId> (<関数ソース>).apply(null, <引数 JSON 配列>);`
//!   （ブラウザの拡張機能 `Extension_Call` が送る形と同じ。改行を含んでよい）
//! - 受信: `executescriptresult: {"id":"<callId>","ok":<戻り値>}` または `{"id":..,"err":"<文字列>"}`
//! - 送信: `downloadas name=<n> id=<tag> format=<ext> options=`
//! - 受信: `downloadas: downloadid=<id> port=<p> id=<tag> filename=<n>` →
//!   `GET /cool/<enc(WOPISrc)>/download/<downloadid>` で本体を取る
//!
//! **スクリプトはリポジトリ管理の固定ソースだけを送る**（PIT-68 ⑤）。LLM の出力は
//! 引数 JSON としてだけ入る（関数ソースへ文字列連結しない）。

/// `executescript` の 1 行を組み立てる。`args` は JSON 配列（関数の引数列）。
pub(super) fn executescript_line(call_id: u64, source: &str, args: &serde_json::Value) -> String {
    // JSON はそのまま JS の式として有効（U+2028/2029 も ES2019 以降は文字列内で合法）。
    format!("executescript {call_id} ({source}).apply(null, {args});")
}

/// スクリプトの結果。
#[derive(Debug, Clone, PartialEq)]
pub enum ScriptResult {
    /// 関数の戻り値（JSON）。
    Ok(serde_json::Value),
    /// kit 側の例外（構文エラー・未捕捉の例外）。中身は kit が文字列化したもの。
    Err(String),
}

/// `executescriptresult:` を (callId, 結果) に分解する。形が違えば None。
pub(super) fn parse_executescriptresult(line: &str) -> Option<(String, ScriptResult)> {
    let json = line.strip_prefix("executescriptresult:")?.trim();
    let mut value: serde_json::Value = serde_json::from_str(json).ok()?;
    let id = match value.get("id")? {
        serde_json::Value::String(s) => s.clone(),
        serde_json::Value::Number(n) => n.to_string(),
        _ => return None,
    };
    let obj = value.as_object_mut()?;
    if let Some(ok) = obj.remove("ok") {
        return Some((id, ScriptResult::Ok(ok)));
    }
    let err = match obj.remove("err") {
        Some(serde_json::Value::String(s)) => s,
        Some(other) => other.to_string(),
        None => "結果に ok も err もありません".to_string(),
    };
    Some((id, ScriptResult::Err(err)))
}

/// `downloadas` の 1 行（name は拡張子つきのファイル名・format は出力形式）。
pub(super) fn downloadas_line(name: &str, format: &str) -> String {
    format!("downloadas name={name} id=shiki-export format={format} options=")
}

/// `downloadas:` から downloadid を抜き出す（`id=shiki-export` のものだけ）。
pub(super) fn parse_downloadas(line: &str) -> Option<String> {
    let rest = line.strip_prefix("downloadas:")?;
    let mut download_id = None;
    let mut tag = None;
    for token in rest.split_whitespace() {
        if let Some((name, value)) = token.split_once('=') {
            match name {
                "downloadid" => download_id = Some(value.to_string()),
                "id" => tag = Some(value),
                _ => {}
            }
        }
    }
    (tag == Some("shiki-export")).then_some(download_id?)
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]

    use super::*;

    #[test]
    fn builds_executescript_with_json_args() {
        let line = executescript_line(
            7,
            "function(req){return req}",
            &serde_json::json!([{"a": "x\"y"}]),
        );
        assert_eq!(
            line,
            r#"executescript 7 (function(req){return req}).apply(null, [{"a":"x\"y"}]);"#
        );
    }

    #[test]
    fn parses_ok_and_err_results() {
        let (id, r) =
            parse_executescriptresult(r#"executescriptresult: {"id":"7","ok":{"n":1}}"#).unwrap();
        assert_eq!(id, "7");
        assert_eq!(r, ScriptResult::Ok(serde_json::json!({"n": 1})));
        let (_, r) =
            parse_executescriptresult(r#"executescriptresult: {"id":"8","err":"expecting ';'"}"#)
                .unwrap();
        assert_eq!(r, ScriptResult::Err("expecting ';'".into()));
        // ok が null（関数が undefined を返した）も成功として扱う。
        let (_, r) =
            parse_executescriptresult(r#"executescriptresult: {"id":"9","ok":null}"#).unwrap();
        assert_eq!(r, ScriptResult::Ok(serde_json::Value::Null));
        assert!(parse_executescriptresult("executescriptresult: not json").is_none());
        assert!(parse_executescriptresult("loaded: viewid=1").is_none());
    }

    #[test]
    fn parses_downloadas_only_for_our_tag() {
        let line = "downloadas: downloadid=abc123 port=9982 id=shiki-export filename=preview.pdf";
        assert_eq!(parse_downloadas(line).as_deref(), Some("abc123"));
        let other = "downloadas: downloadid=zzz port=9982 id=export filename=x.pdf";
        assert!(parse_downloadas(other).is_none());
        assert_eq!(
            downloadas_line("preview.pdf", "pdf"),
            "downloadas name=preview.pdf id=shiki-export format=pdf options="
        );
    }
}
