//! レダクト（解決した平文をログ・run 履歴・エラーからマスクする・engine.md §11.3）。
//!
//! **記録時実施**（表示時レダクトに頼らない・write-only 原則）。解決した平文の集合を
//! [`Redactor`] に登録し、文字列や JSON 値を通してからログ/履歴/output へ書く。

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

    /// 解決した平文を登録する（短すぎるものは登録しない）。
    pub fn add(&mut self, plaintext: &str) {
        if plaintext.len() >= MIN_LEN {
            self.secrets.push(plaintext.to_string());
        }
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

    /// JSON 値を再帰的に走査し、文字列に含まれる平文をマスクする。
    pub fn redact_json(&self, value: &Value) -> Value {
        match value {
            Value::String(s) => Value::String(self.redact_str(s)),
            Value::Array(a) => Value::Array(a.iter().map(|v| self.redact_json(v)).collect()),
            Value::Object(o) => Value::Object(
                o.iter()
                    .map(|(k, v)| (k.clone(), self.redact_json(v)))
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

#[cfg(test)]
mod tests {
    use super::*;

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
