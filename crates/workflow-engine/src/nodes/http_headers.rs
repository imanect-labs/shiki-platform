//! 作成者が決めるヘッダーの決まり（script の `Shiki.http.request` の `headers`・BUG-179、「外部 API 呼び出し」ブロックの
//! 「ヘッダー」・GAP-19、シークレットを付けるヘッダーの名前・BUG-184）。2 つの経路は、この 1 か所の決まりを使う。
//!
//! 以前の script は `headers` を黙って捨て、シークレット・`Idempotency-Key`・`Content-Type` だけを付けていた。いまは
//! 作成者が書いたヘッダーを送る。ただし次のものは送らずに `invalid` で断る（黙って捨てない）。
//! - Shiki が決めるヘッダー（[`RESERVED`]）: 送り先（Host）・本文の長さと送り方（Content-Length・Transfer-Encoding）・
//!   接続の扱い（Connection ほか hop-by-hop）・応答の圧縮（Accept-Encoding・応答は圧縮を解かずに読むため）。Host を
//!   変えると、宛先の照合（URL のホスト部）と違う相手に届きうる。
//! - シークレットを付けるヘッダー（既定は Authorization）と同じ名前（平文を作成者の値で上書き・並記させない）。
//! - 名前が HTTP の token でない・値に制御文字（タブを除く）がある（改行を混ぜたヘッダーの差し込みを防ぐ）・
//!   同じ名前が 2 つある（大文字と小文字は区別しない）・数と長さの上限（[`MAX_HEADERS`]・[`MAX_NAME_BYTES`]・
//!   [`MAX_VALUE_BYTES`]・[`MAX_TOTAL_BYTES`]）を超える。
//!
//! シークレットを付けるヘッダーの名前（`secret.attach.header`）も、名前の形と Shiki が決めるヘッダーを同じ決まりで
//! 確かめる（[`check_secret_header`]）。ブロックは保存時（V1）と実行時に、script は実行時に確かめる。
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

/// 決まりを当てる経路（誤りの文の言い方が違う）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HeaderSurface {
    /// script の `Shiki.http.request`（`headers`・`secret.attach.header` の名前で言う）。
    Script,
    /// 「外部 API 呼び出し」ブロック（欄の見出しの「ヘッダー」・シークレットを付けるヘッダーで言う）。
    Block,
}

/// 確かめる時点での 1 つのヘッダーの値。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HeaderValue<'a> {
    /// 文字の値。
    Text(&'a str),
    /// 文字でない値（数・真偽・まとまりなど）。
    NotText,
    /// 実行のときに決まる値（保存時の差し込み・前の結果の参照）。保存時は値を確かめない。
    Later,
}

/// script の `Shiki.http.request` の `headers` を、送るヘッダーの並び（書いた名前のまま）にする。
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
        Some(_) => return Err(Issue::NotObject.refuse(HeaderSurface::Script)),
    };
    let pairs: Vec<(String, Value)> = map.iter().map(|(k, v)| (k.clone(), v.clone())).collect();
    sendable(HeaderSurface::Script, &pairs, secret_header)
}

/// 「外部 API 呼び出し」ブロックの「ヘッダー」（値を解決した後・名前の順）を、送るヘッダーの並びにする（GAP-19）。
pub fn block_headers(
    resolved: &[(String, Value)],
    secret_header: Option<&str>,
) -> Result<Vec<(String, String)>, HeaderRefused> {
    sendable(HeaderSurface::Block, resolved, secret_header)
}

/// 決まりを当て、文字の値のヘッダーの並びにする。
fn sendable(
    surface: HeaderSurface,
    pairs: &[(String, Value)],
    secret_header: Option<&str>,
) -> Result<Vec<(String, String)>, HeaderRefused> {
    let entries: Vec<(&str, HeaderValue<'_>)> = pairs
        .iter()
        .map(|(name, value)| {
            let value = match value {
                Value::String(s) => HeaderValue::Text(s),
                _ => HeaderValue::NotText,
            };
            (name.as_str(), value)
        })
        .collect();
    check_headers(surface, &entries, secret_header)?;
    Ok(pairs
        .iter()
        .filter_map(|(n, v)| v.as_str().map(|v| (n.clone(), v.to_string())))
        .collect())
}

/// ヘッダーの組に決まりを当てる（保存時は値が [`HeaderValue::Later`] のものの値を確かめない）。
pub fn check_headers(
    surface: HeaderSurface,
    entries: &[(&str, HeaderValue<'_>)],
    secret_header: Option<&str>,
) -> Result<(), HeaderRefused> {
    if entries.len() > MAX_HEADERS {
        return Err(Issue::TooMany.refuse(surface));
    }
    let mut total = 0usize;
    for (i, (name, value)) in entries.iter().enumerate() {
        let name = *name;
        if !is_token(name) {
            return Err(Issue::BadName(name).refuse(surface));
        }
        if RESERVED.contains(&name.to_ascii_lowercase().as_str()) {
            return Err(Issue::Reserved(name).refuse(surface));
        }
        if secret_header.is_some_and(|s| s.eq_ignore_ascii_case(name)) {
            return Err(Issue::SameAsSecret(name).refuse(surface));
        }
        if entries[..i]
            .iter()
            .any(|(n, _)| n.eq_ignore_ascii_case(name))
        {
            return Err(Issue::Duplicate(name).refuse(surface));
        }
        let text = match value {
            HeaderValue::Text(text) => *text,
            HeaderValue::NotText => return Err(Issue::NotText(name).refuse(surface)),
            HeaderValue::Later => "",
        };
        if text.chars().any(|c| c != '\t' && c.is_control()) {
            return Err(Issue::Control(name).refuse(surface));
        }
        if text.len() > MAX_VALUE_BYTES {
            return Err(Issue::ValueTooLong(name).refuse(surface));
        }
        total += name.len() + text.len();
        if total > MAX_TOTAL_BYTES {
            return Err(Issue::TooLong.refuse(surface));
        }
    }
    Ok(())
}

/// script の `secret.attach.header` の名前を確かめる（名前の形と、Shiki が決めるヘッダー）。
pub fn check_secret_header(name: &str) -> Result<(), HeaderRefused> {
    check_secret_header_on(HeaderSurface::Script, name)
}

/// 鍵を入れることの多いヘッダーの名前に含まれる語（小文字・[`looks_like_key_header`]）。
pub const KEY_HEADER_WORDS: &[&str] = &[
    "authorization",
    "api-key",
    "api_key",
    "apikey",
    "token",
    "secret",
    "password",
    "passwd",
    "credential",
    "cookie",
    "session",
    "x-auth",
];

/// API キー・トークンなどの鍵を入れることの多いヘッダーの名前か（名前に [`KEY_HEADER_WORDS`] のどれかを含む・
/// 大文字と小文字は区別しない）。
///
/// 「外部 API 呼び出し」の「ヘッダー」に、この名前で値をそのまま書くと、保存は止めずに注意を返す
/// （`ir.plain_key_header`・GAP-19）。書いた値は IR とバージョン履歴に残り、ワークフローを見られる人に見え、応答に映り
/// 返っても隠さない（隠すのはシークレットの値だけ）。
#[must_use]
pub fn looks_like_key_header(name: &str) -> bool {
    let name = name.to_ascii_lowercase();
    KEY_HEADER_WORDS.iter().any(|w| name.contains(w))
}

/// シークレットを付けるヘッダーの名前（`secret.attach.header`）を確かめる（名前の形と、Shiki が決めるヘッダー・BUG-184）。
pub fn check_secret_header_on(surface: HeaderSurface, name: &str) -> Result<(), HeaderRefused> {
    if !is_token(name) {
        return Err(Issue::SecretBadName(name).refuse(surface));
    }
    if RESERVED.contains(&name.to_ascii_lowercase().as_str()) {
        return Err(Issue::SecretReserved(name).refuse(surface));
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
        PortError::invalid(self.into_message())
    }

    /// 誤りの文（保存時の誤りの文にする）。
    #[must_use]
    pub fn into_message(self) -> String {
        match self {
            HeaderRefused::Invalid(m) | HeaderRefused::Reserved(m) => m,
        }
    }
}

/// 断る理由（経路ごとの文は [`Issue::text`]）。
enum Issue<'a> {
    NotObject,
    TooMany,
    BadName(&'a str),
    Reserved(&'a str),
    SameAsSecret(&'a str),
    Duplicate(&'a str),
    NotText(&'a str),
    Control(&'a str),
    ValueTooLong(&'a str),
    TooLong,
    SecretBadName(&'a str),
    SecretReserved(&'a str),
}

/// script の誤りの文の前置き（どの呼び出しの誤りかを言う）。
const SCRIPT: &str = "Shiki.http.request の headers";
/// script のシークレットを付けるヘッダーの名前の前置き。
const SCRIPT_SECRET: &str = "Shiki.http.request の secret.attach.header";

impl Issue<'_> {
    fn refuse(self, surface: HeaderSurface) -> HeaderRefused {
        let reserved = matches!(
            self,
            Issue::Reserved(_) | Issue::SameAsSecret(_) | Issue::SecretReserved(_)
        );
        let text = self.text(surface);
        if reserved {
            HeaderRefused::Reserved(text)
        } else {
            HeaderRefused::Invalid(text)
        }
    }

    fn text(self, surface: HeaderSurface) -> String {
        let kb = |b: usize| b / 1024;
        let script = surface == HeaderSurface::Script;
        match self {
            Issue::NotObject => format!(
                "{SCRIPT} は、ヘッダーの名前と値の組（{{ \"X-Request-Id\": \"…\" }} の形）で書いてください。"
            ),
            Issue::TooMany if script => format!("{SCRIPT} は {MAX_HEADERS} 個までにしてください。"),
            Issue::TooMany => format!("ヘッダーは {MAX_HEADERS} 個までにしてください。"),
            Issue::BadName(n) if script => format!(
                "{SCRIPT} の名前「{}」は使えません。英数字と - などの記号で、{MAX_NAME_BYTES} 字までにしてください。",
                shown(n)
            ),
            Issue::BadName(n) => format!(
                "ヘッダーの名前「{}」は使えません。英数字と - などの記号で、{MAX_NAME_BYTES} 字までにしてください。",
                shown(n)
            ),
            Issue::Reserved(n) if script => {
                format!("{SCRIPT} の「{}」は Shiki が決めるため、指定できません。", shown(n))
            }
            Issue::Reserved(n) => {
                format!("ヘッダー「{}」は Shiki が決めるため、指定できません。", shown(n))
            }
            Issue::SameAsSecret(n) if script => format!(
                "{SCRIPT} の「{}」は、シークレットを付けるヘッダーと同じため送れません。secret.attach か headers の\
                 どちらかを変えてください。",
                shown(n)
            ),
            Issue::SameAsSecret(n) => format!(
                "ヘッダー「{}」は、シークレットを付けるヘッダーと同じため送れません。シークレットの付け方か、\
                 ヘッダーの名前を変えてください。",
                shown(n)
            ),
            Issue::Duplicate(n) if script => format!(
                "{SCRIPT} に「{}」が 2 回あります（大文字と小文字は区別しません）。1 回にしてください。",
                shown(n)
            ),
            Issue::Duplicate(n) => format!(
                "ヘッダー「{}」が 2 回あります（大文字と小文字は区別しません）。1 回にしてください。",
                shown(n)
            ),
            Issue::NotText(n) if script => {
                format!("{SCRIPT} の「{}」の値は文字で書いてください。", shown(n))
            }
            Issue::NotText(n) => format!("ヘッダー「{}」の値は文字にしてください。", shown(n)),
            Issue::Control(n) if script => format!(
                "{SCRIPT} の「{}」の値に、改行などの制御文字は入れられません。",
                shown(n)
            ),
            Issue::Control(n) => format!(
                "ヘッダー「{}」の値に、改行などの制御文字は入れられません。",
                shown(n)
            ),
            Issue::ValueTooLong(n) if script => format!(
                "{SCRIPT} の「{}」の値が長すぎます（{} KB まで）。",
                shown(n),
                kb(MAX_VALUE_BYTES)
            ),
            Issue::ValueTooLong(n) => format!(
                "ヘッダー「{}」の値が長すぎます（{} KB まで）。",
                shown(n),
                kb(MAX_VALUE_BYTES)
            ),
            Issue::TooLong if script => format!(
                "{SCRIPT} が長すぎます（名前と値を合わせて {} KB まで）。",
                kb(MAX_TOTAL_BYTES)
            ),
            Issue::TooLong => format!(
                "ヘッダーが長すぎます（名前と値を合わせて {} KB まで）。",
                kb(MAX_TOTAL_BYTES)
            ),
            Issue::SecretBadName(n) if script => format!(
                "{SCRIPT_SECRET}「{}」は使えません。英数字と - などの記号で、{MAX_NAME_BYTES} 字までにしてください。",
                shown(n)
            ),
            Issue::SecretBadName(n) => format!(
                "シークレットを付けるヘッダーの名前「{}」は使えません。英数字と - などの記号で、{MAX_NAME_BYTES} 字まで\
                 にしてください。",
                shown(n)
            ),
            Issue::SecretReserved(n) if script => format!(
                "{SCRIPT_SECRET}「{}」は Shiki が決めるため、指定できません。",
                shown(n)
            ),
            Issue::SecretReserved(n) => format!(
                "シークレットを付けるヘッダー「{}」は Shiki が決めるため、指定できません。",
                shown(n)
            ),
        }
    }
}

/// 名前が HTTP の token（RFC 9110 の tchar）で、長さの上限の中か。
fn is_token(name: &str) -> bool {
    let tchar = |b: u8| b.is_ascii_alphanumeric() || b"!#$%&'*+-.^_`|~".contains(&b);
    !name.is_empty() && name.len() <= MAX_NAME_BYTES && name.bytes().all(tchar)
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

    /// ブロックの「ヘッダー」は script と同じ決まりで、欄の見出しで言う（GAP-19・BUG-184）。
    #[test]
    fn block_headers_follow_the_same_rules_in_the_words_of_the_field() {
        let pairs = |v: &[(&str, Value)]| -> Vec<(String, Value)> {
            v.iter()
                .map(|(n, v)| ((*n).to_string(), v.clone()))
                .collect()
        };
        assert_eq!(
            block_headers(
                &pairs(&[("Accept", json!("application/json"))]),
                Some("Authorization")
            )
            .unwrap(),
            vec![("Accept".to_string(), "application/json".to_string())]
        );
        let refused = |v: &[(&str, Value)], secret: Option<&str>| {
            block_headers(&pairs(v), secret).unwrap_err()
        };
        assert_eq!(
            refused(&[("Host", json!("evil.example"))], None),
            HeaderRefused::Reserved(
                "ヘッダー「Host」は Shiki が決めるため、指定できません。".into()
            )
        );
        assert_eq!(
            refused(&[("x-api-key", json!("v"))], Some("X-Api-Key")),
            HeaderRefused::Reserved(
                "ヘッダー「x-api-key」は、シークレットを付けるヘッダーと同じため送れません。シークレットの付け方か、\
                 ヘッダーの名前を変えてください。"
                    .into()
            )
        );
        assert_eq!(
            refused(&[("X-A", json!(1))], None).into_message(),
            "ヘッダー「X-A」の値は文字にしてください。"
        );
        assert_eq!(
            refused(&[("X-A", json!("1\nX-B: 2"))], None).into_message(),
            "ヘッダー「X-A」の値に、改行などの制御文字は入れられません。"
        );
        assert_eq!(
            refused(&[("X A", json!("1"))], None).into_message(),
            "ヘッダーの名前「X A」は使えません。英数字と - などの記号で、64 字までにしてください。"
        );
        assert_eq!(
            refused(&[("X-A", json!("1")), ("x-a", json!("2"))], None).into_message(),
            "ヘッダー「x-a」が 2 回あります（大文字と小文字は区別しません）。1 回にしてください。"
        );
        let many: Vec<(String, Value)> = (0..=MAX_HEADERS)
            .map(|i| (format!("X-{i:02}"), json!("1")))
            .collect();
        assert_eq!(
            block_headers(&many, None).unwrap_err().into_message(),
            "ヘッダーは 20 個までにしてください。"
        );
        let long: Vec<(String, Value)> = (0..3)
            .map(|i| (format!("X-{i}"), json!("a".repeat(3_000))))
            .collect();
        assert_eq!(
            block_headers(&long, None).unwrap_err().into_message(),
            "ヘッダーが長すぎます（名前と値を合わせて 8 KB まで）。"
        );
        assert_eq!(
            refused(&[("X-A", json!("a".repeat(MAX_VALUE_BYTES + 1)))], None).into_message(),
            "ヘッダー「X-A」の値が長すぎます（4 KB まで）。"
        );
    }

    /// 保存時は、実行のときに決まる値（差し込み・参照）の値を確かめず、名前と数だけを確かめる。
    #[test]
    fn saved_headers_skip_values_decided_at_run_time() {
        let later = [
            ("X-Request-Id", HeaderValue::Later),
            ("Accept", HeaderValue::Text("text/plain")),
        ];
        assert_eq!(check_headers(HeaderSurface::Block, &later, None), Ok(()));
        let reserved = [("Content-Length", HeaderValue::Later)];
        assert!(matches!(
            check_headers(HeaderSurface::Block, &reserved, None),
            Err(HeaderRefused::Reserved(_))
        ));
        let not_text = [("X-Count", HeaderValue::NotText)];
        assert_eq!(
            check_headers(HeaderSurface::Block, &not_text, None)
                .unwrap_err()
                .into_message(),
            "ヘッダー「X-Count」の値は文字にしてください。"
        );
    }

    /// シークレットを付けるヘッダーの名前は、ブロックではその欄の言い方で断る（BUG-184）。
    #[test]
    fn block_secret_header_names_are_checked() {
        assert_eq!(
            check_secret_header_on(HeaderSurface::Block, "X-Api-Key"),
            Ok(())
        );
        assert_eq!(
            check_secret_header_on(HeaderSurface::Block, "Host"),
            Err(HeaderRefused::Reserved(
                "シークレットを付けるヘッダー「Host」は Shiki が決めるため、指定できません。"
                    .into()
            ))
        );
        for bad in ["", "X Api", "X-Api\r\nHost"] {
            assert!(
                matches!(
                    check_secret_header_on(HeaderSurface::Block, bad),
                    Err(HeaderRefused::Invalid(m)) if m.starts_with("シークレットを付けるヘッダーの名前「")
                ),
                "{bad:?}"
            );
        }
    }
}
