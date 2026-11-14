//! 保存を止めない警告: 「外部 API 呼び出し」の「ヘッダー」に、鍵を入れることの多い名前（Authorization など）で値を
//! そのまま書いた（GAP-19）。
//!
//! 書いた値は IR（バージョン履歴を含む）に残り、ワークフローを見られる人に見える。送り先の応答に映り返っても隠さない
//! （隠すのはシークレットの値だけ）。鍵はシークレットに入れて「使用するシークレット」で付けるよう知らせる。保存は
//! 止めない（鍵でない値のこともあり、保存済みの定義もある）。注意の文に値を映し返さない。
//!
//! 名前の決まりは `nodes::http_headers::looks_like_key_header` の 1 か所。値は、リテラルの文字と、`$template` の差し込み
//! 以外の文字と、差し込みの `vars` にリテラルで書いた文字を見る。認証の方式の語（Bearer など）だけで鍵の無い値
//! （`Bearer {token}` の差し込みで、token が参照のものなど）は言わない。

use serde_json::Value;

use super::ValidationError;
use crate::ir::expr::{template_pieces, TemplatePiece, ValueExpr};
use crate::ir::WorkflowIr;
use crate::nodes::http_headers::looks_like_key_header;
use crate::vocab::NodeType;

/// 警告のコード: 鍵を入れることの多い名前のヘッダーに、値をそのまま書いた。
pub const PLAIN_KEY_HEADER: &str = "ir.plain_key_header";

/// 認証の方式の語（値の前に置くだけで、鍵ではない）。
const AUTH_SCHEMES: &[&str] = &["bearer", "basic", "token", "bot", "digest"];

/// 鍵を入れることの多い名前のヘッダーに値をそのまま書いた「外部 API 呼び出し」（ブロックごとに 1 つ）。
pub(super) fn warnings(ir: &WorkflowIr) -> Vec<ValidationError> {
    let mut out = Vec::new();
    for node in ir
        .nodes
        .iter()
        .filter(|n| n.node_type == NodeType::HttpRequest.as_str())
    {
        let Some(headers) = node.params.get("headers").and_then(Value::as_object) else {
            continue;
        };
        let names: Vec<String> = headers
            .iter()
            .filter(|(name, value)| looks_like_key_header(name) && has_written_value(value))
            .map(|(name, _)| format!("「{name}」"))
            .collect();
        if names.is_empty() {
            continue;
        }
        out.push(
            ValidationError::new(
                PLAIN_KEY_HEADER,
                format!(
                    "ヘッダー{}に、値をそのまま書いています。この値は、ワークフローを見られる人に見え、バージョン履歴\
                     にも残ります。API キーなどの鍵は、シークレットに入れて「使用するシークレット」で付けてください。",
                    names.join("・")
                ),
            )
            .at_node(&node.id)
            .at_path("/params/headers"),
        );
    }
    out
}

/// 値に、IR に書いたままの文字（認証の方式の語を除く）があるか。参照の中身は実行のときに決まるので見ない。
fn has_written_value(value: &Value) -> bool {
    let Ok(expr) = serde_json::from_value::<ValueExpr>(value.clone()) else {
        return false;
    };
    written_text(&expr)
        .split_whitespace()
        .any(|word| !AUTH_SCHEMES.contains(&word.to_ascii_lowercase().as_str()))
}

/// 値の式のうち、IR に書いたままの文字（参照は区切りの空白にする）。
///
/// 差し込みの `vars` にリテラルで書いた値も、書いたままの文字として数える（`{"$template":"Bearer {t}","vars":{"t":
/// "sk-…"}}` の鍵も IR とバージョン履歴に残る）。差し込みは前後を区切りにする（`{a}{b}` や `k-{x}` の値どうしを 1 語に
/// しない）。
fn written_text(expr: &ValueExpr) -> String {
    match expr {
        ValueExpr::Literal(Value::String(s)) => s.clone(),
        ValueExpr::Literal(Value::Number(n)) => n.to_string(),
        ValueExpr::Template(t) => template_pieces(&t.template)
            .into_iter()
            .map(|piece| match piece {
                TemplatePiece::Text(s) => s.to_string(),
                TemplatePiece::Escaped(c) => c.to_string(),
                TemplatePiece::Var(name) => t
                    .vars
                    .get(name)
                    .map_or_else(|| " ".to_string(), |v| format!(" {} ", written_text(v))),
            })
            .collect(),
        _ => String::new(),
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::needless_pass_by_value)]
    use serde_json::json;

    use super::*;

    fn ir(headers: Value) -> WorkflowIr {
        WorkflowIr::from_json(&json!({
            "ir_version": 1, "name": "wf", "triggers": [{ "kind": "interactive" }],
            "nodes": [{ "id": "call", "type": "http.request", "params": {
                "url": "https://api.example.com", "headers": headers } }],
            "edges": []
        }))
        .unwrap()
    }

    /// 鍵の名前のヘッダーに書いた値は、名前だけを挙げて知らせる（値は映し返さない）。
    #[test]
    fn written_values_in_key_headers_are_reported_without_the_value() {
        let w = warnings(&ir(json!({
            "Authorization": "Bearer sk-live-0123456789",
            "X-Api-Key": { "$template": "k-{suffix}", "vars": { "suffix": "x" } },
            "X-Request-Id": "r-1"
        })));
        assert_eq!(w.len(), 1);
        assert_eq!(w[0].code, PLAIN_KEY_HEADER);
        assert_eq!(w[0].node_id.as_deref(), Some("call"));
        assert_eq!(w[0].path.as_deref(), Some("/params/headers"));
        assert_eq!(
            w[0].message,
            "ヘッダー「Authorization」・「X-Api-Key」に、値をそのまま書いています。この値は、ワークフローを見られる人に\
             見え、バージョン履歴にも残ります。API キーなどの鍵は、シークレットに入れて「使用するシークレット」で付けて\
             ください。"
        );
        assert!(!w[0].message.contains("sk-live"));
    }

    /// 差し込みの vars にリテラルで書いた鍵も、書いた値として知らせる（認証の方式の語だけなら言わない）。
    #[test]
    fn literal_vars_of_a_template_are_written_values() {
        for headers in [
            json!({ "Authorization": { "$template": "Bearer {t}", "vars": { "t": "sk-live-0123" } } }),
            json!({ "Authorization": { "$template": "{s} {t}", "vars": {
                "s": "Bearer", "t": { "$template": "{k}", "vars": { "k": "sk-live-0123" } } } } }),
            json!({ "X-Api-Key": { "$template": "{n}", "vars": { "n": 1_234_567 } } }),
        ] {
            let w = warnings(&ir(headers.clone()));
            assert_eq!(w.len(), 1, "{headers}");
            assert!(!w[0].message.contains("sk-live"), "{}", w[0].message);
        }
        assert!(warnings(&ir(
            json!({ "Authorization": { "$template": "{s} {t}", "vars": {
            "s": "Bearer", "t": { "$from": "input", "path": "/token" } } } })
        ))
        .is_empty());
    }

    /// 参照・差し込みだけの値、認証の方式の語だけの値、空の値、鍵でない名前のヘッダーは言わない。
    #[test]
    fn references_and_other_headers_are_not_reported() {
        for headers in [
            json!({ "Authorization": { "$from": "input", "path": "/token" } }),
            json!({ "Authorization": { "$template": "Bearer {t}", "vars": {
                "t": { "$from": "input", "path": "/token" } } } }),
            json!({ "Authorization": "Bearer " }),
            json!({ "Authorization": "" }),
            json!({ "X-Request-Id": "r-1", "Idempotency-Key": "k-1", "Content-Type": "text/plain" }),
        ] {
            assert!(warnings(&ir(headers.clone())).is_empty(), "{headers}");
        }
    }

    /// 鍵を入れることの多い名前は、大文字と小文字によらず語を含むかで見る。
    #[test]
    fn key_header_names_are_matched_by_words() {
        for name in [
            "Authorization",
            "x-api-key",
            "Api_Key",
            "X-Auth-Token",
            "X-Access-Token",
            "Cookie",
            "X-Client-Secret",
            "X-Session-Id",
        ] {
            assert!(looks_like_key_header(name), "{name}");
        }
        for name in ["Accept", "X-Request-Id", "Idempotency-Key", "Content-Type"] {
            assert!(!looks_like_key_header(name), "{name}");
        }
    }
}
