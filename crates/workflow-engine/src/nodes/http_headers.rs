//! script の `Shiki.http.request` の `headers`（作成者が決めるヘッダー・BUG-179）。
//!
//! 以前は `headers` を黙って捨て、シークレット・`Idempotency-Key`・`Content-Type` だけを付けていた。いまは作成者が
//! 書いたヘッダーを送る。ただし次のものは送らずに `invalid` で断る（黙って捨てない）。
//! - Shiki が決めるヘッダー（[`RESERVED`]）: 送り先（Host）・本文の長さと送り方（Content-Length・Transfer-Encoding）・
//!   接続の扱い（Connection ほか hop-by-hop）・応答の圧縮（Accept-Encoding・応答は圧縮を解かずに読むため）。Host を
//!   変えると、宛先の照合（URL のホスト部）と違う相手に届きうる。
//! - シークレットを付けるヘッダー（既定は Authorization）と同じ名前（平文を作成者の値で上書き・並記させない）。
//! - 名前が HTTP の token でない・値に制御文字（タブを除く）がある（改行を混ぜたヘッダーの差し込みを防ぐ）・
//!   同じ名前が 2 つある（大文字と小文字は区別しない）・数と長さの上限（[`MAX_HEADERS`]・[`MAX_NAME_BYTES`]・
//!   [`MAX_VALUE_BYTES`]・[`MAX_TOTAL_BYTES`]）を超える。
//!
//! 作成者が `Idempotency-Key`・`Content-Type` を書いたら、Shiki が付ける値の代わりにそれを送る。宛先の照合
//! （egress allowlist・シークレットの送り先）は変えない。

use serde_json::Value;

use super::ports::PortError;

/// 書けるヘッダーの数の上限。
pub const MAX_HEADERS: usize = 20;
/// ヘッダーの名前の長さの上限（バイト）。
pub const MAX_NAME_BYTES: usize = 64;
/// ヘッダーの値の長さの上限（バイト・4 KB）。
pub const MAX_VALUE_BYTES: usize = 4 * 1024;
/// 名前と値を合わせた長さの上限（バイト・8 KB。多くのサーバーが受ける要求のヘッダーの大きさ）。
pub const MAX_TOTAL_BYTES: usize = 8 * 1024;

/// Shiki が決めるので作成者が書けないヘッダー（小文字）。
pub const RESERVED: &[&str] = &[
    "host",
    "content-length",
    "transfer-encoding",
    "connection",
    "keep-alive",
    "proxy-connection",
    "proxy-authorization",
    "te",
    "trailer",
    "upgrade",
    "expect",
    "accept-encoding",
];

/// 誤りの文の前置き（どの呼び出しの誤りかを言う）。
const WHAT: &str = "Shiki.http.request の headers";

/// 作成者が書いた `headers` を、送るヘッダーの並び（書いた名前のまま）にする。
///
/// `secret_header` はシークレットを付けるヘッダーの名前（シークレットを使わないときは `None`）。`headers` が無い・
/// null なら空。断るものは [`module`](self) の説明のとおり。
pub fn author_headers(
    raw: Option<&Value>,
    secret_header: Option<&str>,
) -> Result<Vec<(String, String)>, HeaderRefused> {
    let map = match raw {
        None | Some(Value::Null) => return Ok(Vec::new()),
        Some(Value::Object(map)) => map,
        Some(_) => {
            return Err(HeaderRefused::Invalid(format!(
                "{WHAT} は、ヘッダーの名前と値の組（{{ \"X-Request-Id\": \"…\" }} の形）で書いてください。"
            )))
        }
    };
    if map.len() > MAX_HEADERS {
        return Err(HeaderRefused::Invalid(format!(
            "{WHAT} は {MAX_HEADERS} 個までにしてください。"
        )));
    }
    let mut out: Vec<(String, String)> = Vec::with_capacity(map.len());
    let mut total = 0usize;
    for (name, value) in map {
        check_name(name)?;
        let shown = shown(name);
        let lower = name.to_ascii_lowercase();
        if RESERVED.contains(&lower.as_str()) {
            return Err(HeaderRefused::Reserved(format!(
                "{WHAT} の「{shown}」は Shiki が決めるため、指定できません。"
            )));
        }
        if secret_header.is_some_and(|s| s.eq_ignore_ascii_case(name)) {
            return Err(HeaderRefused::Reserved(format!(
                "{WHAT} の「{shown}」は、シークレットを付けるヘッダーと同じため送れません。secret.attach か headers の\
                 どちらかを変えてください。"
            )));
        }
        if out.iter().any(|(n, _)| n.eq_ignore_ascii_case(name)) {
            return Err(HeaderRefused::Invalid(format!(
                "{WHAT} に「{shown}」が 2 回あります（大文字と小文字は区別しません）。1 回にしてください。"
            )));
        }
        let Value::String(value) = value else {
            return Err(HeaderRefused::Invalid(format!(
                "{WHAT} の「{shown}」の値は文字で書いてください。"
            )));
        };
        if value.chars().any(|c| c != '\t' && c.is_control()) {
            return Err(HeaderRefused::Invalid(format!(
                "{WHAT} の「{shown}」の値に、改行などの制御文字は入れられません。"
            )));
        }
        if value.len() > MAX_VALUE_BYTES {
            return Err(HeaderRefused::Invalid(format!(
                "{WHAT} の「{shown}」の値が長すぎます（{} KB まで）。",
                MAX_VALUE_BYTES / 1024
            )));
        }
        total += name.len() + value.len();
        if total > MAX_TOTAL_BYTES {
            return Err(HeaderRefused::Invalid(format!(
                "{WHAT} が長すぎます（名前と値を合わせて {} KB まで）。",
                MAX_TOTAL_BYTES / 1024
            )));
        }
        out.push((name.clone(), value.clone()));
    }
    Ok(out)
}

/// シークレットを付けるヘッダーの名前（`secret.attach.header`）を確かめる（名前の形と、Shiki が決めるヘッダー）。
pub fn check_secret_header(name: &str) -> Result<(), HeaderRefused> {
    check_name(name).map_err(|_| {
        HeaderRefused::Invalid(format!(
            "Shiki.http.request の secret.attach.header「{}」は使えません。英数字と - などの記号で、{MAX_NAME_BYTES} 字までに\
             してください。",
            shown(name)
        ))
    })?;
    if RESERVED.contains(&name.to_ascii_lowercase().as_str()) {
        return Err(HeaderRefused::Reserved(format!(
            "Shiki.http.request の secret.attach.header「{}」は Shiki が決めるため、指定できません。",
            shown(name)
        )));
    }
    Ok(())
}

/// 作成者のヘッダーに `name` があるか（大文字と小文字は区別しない）。
#[must_use]
pub fn has_header(headers: &[(String, String)], name: &str) -> bool {
    headers.iter().any(|(n, _)| n.eq_ignore_ascii_case(name))
}

/// ヘッダーを断った理由。`Reserved` は Shiki が決めるヘッダー・シークレットのヘッダーを書いた（監査に残す）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum HeaderRefused {
    /// 形・数・長さの誤り。
    Invalid(String),
    /// 送ってはいけないヘッダー。
    Reserved(String),
}

impl HeaderRefused {
    /// 能力の誤り（どちらも `invalid`・やり直しても同じ）。
    #[must_use]
    pub fn into_port_error(self) -> PortError {
        match self {
            HeaderRefused::Invalid(m) | HeaderRefused::Reserved(m) => PortError::invalid(m),
        }
    }
}

/// 名前が HTTP の token（RFC 9110 の tchar）で、長さの上限の中か。
fn check_name(name: &str) -> Result<(), HeaderRefused> {
    let tchar = |b: u8| b.is_ascii_alphanumeric() || b"!#$%&'*+-.^_`|~".contains(&b);
    if name.is_empty() || name.len() > MAX_NAME_BYTES || !name.bytes().all(tchar) {
        return Err(HeaderRefused::Invalid(format!(
            "{WHAT} の名前「{}」は使えません。英数字と - などの記号で、{MAX_NAME_BYTES} 字までにしてください。",
            shown(name)
        )));
    }
    Ok(())
}

/// 誤りの文に出す名前（制御文字は ? にし、長すぎる名前は切る）。
fn shown(name: &str) -> String {
    let mut s: String = name
        .chars()
        .take(MAX_NAME_BYTES)
        .map(|c| if c.is_control() { '?' } else { c })
        .collect();
    if name.chars().count() > MAX_NAME_BYTES {
        s.push('…');
    }
    s
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]
    use super::*;
    use serde_json::json;

    fn refused(raw: &Value, secret: Option<&str>) -> HeaderRefused {
        author_headers(Some(raw), secret).unwrap_err()
    }

    /// 名前を計算で決めるヘッダーの組。
    fn obj(pairs: &[(String, Value)]) -> Value {
        Value::Object(pairs.iter().cloned().collect())
    }

    #[test]
    fn keeps_the_headers_the_author_wrote() {
        let h = author_headers(
            Some(&json!({ "X-Request-Id": "abc", "Accept": "application/json", "User-Agent": "経費 bot\t1" })),
            Some("Authorization"),
        )
        .unwrap();
        // 名前は書いたまま（並びは名前の順）。値のタブと日本語はそのまま。
        assert_eq!(
            h,
            vec![
                ("Accept".to_string(), "application/json".to_string()),
                ("User-Agent".to_string(), "経費 bot\t1".to_string()),
                ("X-Request-Id".to_string(), "abc".to_string()),
            ]
        );
        assert!(has_header(&h, "x-request-id") && !has_header(&h, "Content-Type"));
        assert_eq!(author_headers(None, None).unwrap(), vec![]);
        assert_eq!(author_headers(Some(&Value::Null), None).unwrap(), vec![]);
        // シークレットを使わないなら Authorization も作成者が書ける。
        assert_eq!(
            author_headers(Some(&json!({ "Authorization": "Basic eA==" })), None).unwrap(),
            vec![("Authorization".to_string(), "Basic eA==".to_string())]
        );
    }

    #[test]
    fn refuses_headers_shiki_decides_and_the_secret_header() {
        for name in [
            "Host",
            "content-length",
            "Transfer-Encoding",
            "Connection",
            "Accept-Encoding",
            "Proxy-Authorization",
        ] {
            let e = refused(&obj(&[(name.to_string(), json!("x"))]), None);
            assert_eq!(
                e,
                HeaderRefused::Reserved(format!(
                    "Shiki.http.request の headers の「{name}」は Shiki が決めるため、指定できません。"
                ))
            );
        }
        // シークレットを付けるヘッダーと重ねない（大文字と小文字は区別しない）。
        let e = refused(
            &json!({ "authorization": "Bearer x" }),
            Some("Authorization"),
        );
        assert!(
            matches!(&e, HeaderRefused::Reserved(m) if m.starts_with(
                "Shiki.http.request の headers の「authorization」は、シークレットを付けるヘッダーと同じため送れません。"
            )),
            "{e:?}"
        );
        let e = refused(&json!({ "X-API-KEY": "x" }), Some("X-Api-Key"));
        assert!(matches!(e, HeaderRefused::Reserved(_)));
        assert_eq!(e.into_port_error().code, "invalid");
    }

    #[test]
    fn refuses_malformed_headers_with_a_reason() {
        // 形の誤りは Invalid（監査に残す Reserved ではない）。
        let msg = |raw: Value| match refused(&raw, None) {
            HeaderRefused::Invalid(m) => m,
            HeaderRefused::Reserved(m) => format!("Reserved になった: {m}"),
        };
        assert!(msg(json!(["X-A", "1"]))
            .starts_with("Shiki.http.request の headers は、ヘッダーの名前と値の組"));
        assert!(msg(json!("X-A: 1")).starts_with("Shiki.http.request の headers は、"));
        assert_eq!(
            msg(json!({ "X A": "1" })),
            "Shiki.http.request の headers の名前「X A」は使えません。英数字と - などの記号で、64 字までにしてください。"
        );
        assert!(msg(json!({ "": "1" })).contains("の名前「」は使えません"));
        assert!(
            msg(obj(&[("x".repeat(65), json!("1"))])).contains(&format!("「{}…」", "x".repeat(64)))
        );
        assert_eq!(
            msg(json!({ "X-A": 1 })),
            "Shiki.http.request の headers の「X-A」の値は文字で書いてください。"
        );
        // 改行を混ぜたヘッダーの差し込みは断る。
        assert_eq!(
            msg(json!({ "X-A": "1\r\nX-Injected: yes" })),
            "Shiki.http.request の headers の「X-A」の値に、改行などの制御文字は入れられません。"
        );
        assert_eq!(
            msg(json!({ "X-A": "a".repeat(MAX_VALUE_BYTES + 1) })),
            "Shiki.http.request の headers の「X-A」の値が長すぎます（4 KB まで）。"
        );
        assert!(author_headers(Some(&json!({ "X-A": "a".repeat(MAX_VALUE_BYTES) })), None).is_ok());
        let big: serde_json::Map<String, Value> = (0..3)
            .map(|i| (format!("X-{i}"), json!("a".repeat(3_000))))
            .collect();
        assert_eq!(
            msg(Value::Object(big)),
            "Shiki.http.request の headers が長すぎます（名前と値を合わせて 8 KB まで）。"
        );
        let many: serde_json::Map<String, Value> = (0..=MAX_HEADERS)
            .map(|i| (format!("X-{i}"), json!("1")))
            .collect();
        assert_eq!(
            msg(Value::Object(many)),
            "Shiki.http.request の headers は 20 個までにしてください。"
        );
        assert_eq!(
            msg(json!({ "X-A": "1", "x-a": "2" })),
            "Shiki.http.request の headers に「x-a」が 2 回あります（大文字と小文字は区別しません）。1 回にしてください。"
        );
    }

    #[test]
    fn checks_the_header_the_secret_is_attached_to() {
        assert!(check_secret_header("X-Api-Key").is_ok());
        assert!(matches!(
            check_secret_header("Host"),
            Err(HeaderRefused::Reserved(_))
        ));
        assert!(
            matches!(check_secret_header("X Api"), Err(HeaderRefused::Invalid(m)) if m.starts_with(
                "Shiki.http.request の secret.attach.header「X Api」は使えません。"
            ))
        );
    }
}
