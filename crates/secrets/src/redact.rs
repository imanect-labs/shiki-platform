//! レダクト（解決した平文をログ・run 履歴・エラーからマスクする・engine.md §11.3）。
//!
//! **記録時実施**（表示時レダクトに頼らない・write-only 原則）。解決した平文の集合を
//! [`Redactor`] に登録し、文字列や JSON 値を通してからログ/履歴/output へ書く。
//!
//! マスクは**完全一致**で、登録した平文のほかに、外部の応答に反射しやすい変形だけを足す: 前後の空白を
//! 落とした形・JSON の文字列としてエスケープした形（標準・`\/`・`\uXXXX` の大小）・%エンコードした形
//! （RFC 3986・`encodeURIComponent`・フォーム・16 進の大小）。ほかの符号化（base64・HTML の文字参照・
//! 部分一致・4 文字未満）はマスクできない。

use serde_json::Value;

/// マスク後の置換文字列。
const MASK: &str = "[REDACTED]";
/// これ未満の長さの平文はマスク対象にしない（"a" 等が全文一致で誤爆するのを防ぐ）。
pub(crate) const MIN_LEN: usize = 4;

/// 解決済み平文を保持し、任意の文字列/JSON からマスクする。
#[derive(Debug, Default, Clone)]
pub struct Redactor {
    secrets: Vec<String>,
}

impl Redactor {
    pub fn new() -> Self {
        Redactor {
            secrets: Vec::new(),
        }
    }

    /// 解決した平文を登録する（短すぎるものは登録しない）。反射しやすい変形（モジュールの説明）も一緒に登録する。
    pub fn add(&mut self, plaintext: &str) {
        let trimmed = plaintext.trim();
        for base in [plaintext, trimmed] {
            if base.len() < MIN_LEN {
                continue;
            }
            for form in std::iter::once(base.to_string()).chain(encoded_forms(base)) {
                if !self.secrets.contains(&form) {
                    self.secrets.push(form);
                }
            }
        }
        // 長い形から置き換える（短い形が長い形の一部を先に置き換えて、残りが平文のまま残らないように）。
        self.secrets.sort_by_key(|s| std::cmp::Reverse(s.len()));
    }

    /// 文字列中の登録済み平文を全てマスクする。
    pub fn redact_str(&self, input: &str) -> String {
        let mut out = input.to_string();
        for s in &self.secrets {
            if out.contains(s.as_str()) {
                out = out.replace(s.as_str(), MASK);
            }
        }
        out
    }

    /// JSON 値を再帰的に走査し、文字列（オブジェクトのキーを含む）に含まれる平文をマスクする。
    pub fn redact_json(&self, value: &Value) -> Value {
        match value {
            Value::String(s) => Value::String(self.redact_str(s)),
            Value::Array(a) => Value::Array(a.iter().map(|v| self.redact_json(v)).collect()),
            Value::Object(o) => Value::Object(
                o.iter()
                    .map(|(k, v)| (self.redact_str(k), self.redact_json(v)))
                    .collect(),
            ),
            other => other.clone(),
        }
    }

    /// 登録済み平文があるか（空なら redact をスキップできる）。
    pub fn is_empty(&self) -> bool {
        self.secrets.is_empty()
    }
}

/// 平文の反射しやすい変形（JSON の文字列のエスケープ・%エンコード）。元と同じ形も含みうる（呼び出し側で重複を除く）。
fn encoded_forms(p: &str) -> Vec<String> {
    let mut out = Vec::new();
    for json in [
        JsonEscape::STANDARD,
        JsonEscape {
            slash: true,
            ..JsonEscape::STANDARD
        },
        JsonEscape {
            non_ascii: Some(Hex::Lower),
            ..JsonEscape::STANDARD
        },
        JsonEscape {
            non_ascii: Some(Hex::Upper),
            html: true,
            ..JsonEscape::STANDARD
        },
    ] {
        out.push(json.escape(p));
    }
    // RFC 3986 の非予約文字・encodeURIComponent・フォーム（空白は +）。
    let rfc3986 = |c: u8| c.is_ascii_alphanumeric() || b"-._~".contains(&c);
    let uri_component = |c: u8| c.is_ascii_alphanumeric() || b"-_.!~*'()".contains(&c);
    let form = |c: u8| c.is_ascii_alphanumeric() || b"*-._".contains(&c);
    for hex in [Hex::Upper, Hex::Lower] {
        out.push(percent_encode(p, rfc3986, hex, false));
        out.push(percent_encode(p, uri_component, hex, false));
        out.push(percent_encode(p, form, hex, true));
    }
    out
}

#[derive(Clone, Copy)]
enum Hex {
    Upper,
    Lower,
}

impl Hex {
    fn fmt(self, v: u32, width: usize) -> String {
        match self {
            Hex::Upper => format!("{v:0width$X}"),
            Hex::Lower => format!("{v:0width$x}"),
        }
    }
}

/// JSON の文字列のエスケープの流儀（serde_json の標準・PHP の `\/`・Python の ensure_ascii・.NET の既定）。
#[derive(Clone, Copy)]
struct JsonEscape {
    /// `/` を `\/` にする。
    slash: bool,
    /// ASCII でない文字を `\uXXXX`（UTF-16）にする。
    non_ascii: Option<Hex>,
    /// HTML で意味を持つ文字（`"` `<` `>` `&` `'` `+` `` ` ``）を `\uXXXX` にする。
    html: bool,
}

impl JsonEscape {
    const STANDARD: JsonEscape = JsonEscape {
        slash: false,
        non_ascii: None,
        html: false,
    };

    fn escape(self, p: &str) -> String {
        let hex = self.non_ascii.unwrap_or(Hex::Lower);
        let mut out = String::with_capacity(p.len());
        for c in p.chars() {
            match c {
                '"' | '<' | '>' | '&' | '\'' | '+' | '`' if self.html => {
                    out.push_str("\\u");
                    out.push_str(&hex.fmt(u32::from(c), 4));
                }
                '"' => out.push_str("\\\""),
                '\\' => out.push_str("\\\\"),
                '/' if self.slash => out.push_str("\\/"),
                '\n' => out.push_str("\\n"),
                '\r' => out.push_str("\\r"),
                '\t' => out.push_str("\\t"),
                '\u{8}' => out.push_str("\\b"),
                '\u{c}' => out.push_str("\\f"),
                c if u32::from(c) < 0x20 => {
                    out.push_str("\\u");
                    out.push_str(&Hex::Lower.fmt(u32::from(c), 4));
                }
                c if !c.is_ascii() && self.non_ascii.is_some() => {
                    let mut units = [0u16; 2];
                    for u in c.encode_utf16(&mut units) {
                        out.push_str("\\u");
                        out.push_str(&hex.fmt(u32::from(*u), 4));
                    }
                }
                c => out.push(c),
            }
        }
        out
    }
}

/// %エンコード（UTF-8 のバイトごと・`keep` の文字はそのまま・`space_plus` なら空白を `+`）。
fn percent_encode(p: &str, keep: impl Fn(u8) -> bool, hex: Hex, space_plus: bool) -> String {
    let mut out = String::with_capacity(p.len());
    for b in p.bytes() {
        if keep(b) {
            out.push(char::from(b));
        } else if space_plus && b == b' ' {
            out.push('+');
        } else {
            out.push('%');
            out.push_str(&hex.fmt(u32::from(b), 2));
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 反射しやすい変形もマスクする（前後の空白を落とした形・JSON のエスケープ・%エンコード）。
    #[test]
    fn masks_reflected_forms() {
        let mut r = Redactor::new();
        r.add("abc/def+ghi=");
        for reflected in [
            "abc/def+ghi=",
            "abc\\/def+ghi=",
            "abc/def\\u002Bghi=",
            "abc%2Fdef%2Bghi%3D",
            "abc%2fdef%2bghi%3d",
        ] {
            let out = r.redact_str(&format!("next=https://api.example.com/x?t={reflected}&p=2"));
            assert_eq!(
                out, "next=https://api.example.com/x?t=[REDACTED]&p=2",
                "{reflected}"
            );
        }
        let mut r = Redactor::new();
        r.add(" tok-12345 \n");
        assert_eq!(
            r.redact_str("Bearer tok-12345 echoed"),
            "Bearer [REDACTED] echoed"
        );
        let mut r = Redactor::new();
        r.add("pa\"ss\\wörd 1");
        for reflected in [
            "pa\\\"ss\\\\wörd 1",
            "pa\\\"ss\\\\w\\u00f6rd 1",
            "pa\\u0022ss\\\\w\\u00F6rd 1",
            "pa%22ss%5Cw%C3%B6rd%201",
            "pa%22ss%5Cw%C3%B6rd+1",
        ] {
            assert_eq!(
                r.redact_str(&format!("[{reflected}]")),
                "[[REDACTED]]",
                "{reflected}"
            );
        }
    }

    /// JSON のオブジェクトのキーもマスクする。
    #[test]
    fn masks_object_keys() {
        let mut r = Redactor::new();
        r.add("s3cr3t-value-long");
        let red = r
            .redact_json(&serde_json::json!({ "s3cr3t-value-long": { "x-s3cr3t-value-long": 1 } }));
        assert!(!red.to_string().contains("s3cr3t-value-long"), "{red}");
        assert_eq!(red["[REDACTED]"]["x-[REDACTED]"], serde_json::json!(1));
    }

    #[test]
    fn masks_in_string() {
        let mut r = Redactor::new();
        r.add("xoxb-super-secret-token");
        let out = r.redact_str("Authorization: Bearer xoxb-super-secret-token done");
        assert!(!out.contains("xoxb-super-secret-token"));
        assert!(out.contains(MASK));
    }

    #[test]
    fn masks_in_nested_json() {
        let mut r = Redactor::new();
        r.add("s3cr3t-value-long");
        let v = serde_json::json!({
            "headers": { "auth": "Bearer s3cr3t-value-long" },
            "list": ["prefix s3cr3t-value-long suffix", 42],
        });
        let red = r.redact_json(&v);
        let s = red.to_string();
        assert!(!s.contains("s3cr3t-value-long"));
        assert!(s.contains("REDACTED"));
        // 非文字列（数値）はそのまま。
        assert_eq!(red["list"][1], serde_json::json!(42));
    }

    #[test]
    fn short_secrets_not_registered() {
        let mut r = Redactor::new();
        r.add("ab"); // MIN_LEN 未満
        assert!(r.is_empty());
        assert_eq!(r.redact_str("ab cd"), "ab cd");
    }

    #[test]
    fn empty_redactor_is_identity() {
        let r = Redactor::new();
        assert_eq!(r.redact_str("nothing to mask"), "nothing to mask");
    }
}
