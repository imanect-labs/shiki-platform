//! ノード params の `$from`/`$template`/リテラル解決（engine.md §4.1・ir.md §3）。
//!
//! [`ParamResolver`] は [`ValueResolver`](crate::control::eval::ValueResolver) を実装し、
//! `$from` の源（`input` / `trigger` / `run` / `nodes.<id>.output`）を [`NodeContext`] から供給する。
//! executor は本モジュールのヘルパ（[`resolve_field`] ＋型変換）で params の各フィールドを解決する。
//! `each`（map 領域）は Stage A 未対応のため `None` を返す。

use serde_json::Value;

use crate::control::eval::{resolve_value, ValueResolver};
use crate::ir::expr::{FromRef, ValueExpr};
use crate::run::NodeContext;

/// `$from` の源を `NodeContext` から解決するリゾルバ。
pub struct ParamResolver<'a> {
    ctx: &'a NodeContext,
}

impl<'a> ParamResolver<'a> {
    #[must_use]
    pub fn new(ctx: &'a NodeContext) -> Self {
        ParamResolver { ctx }
    }

    /// `$from` の source 名から基底 Value を得る（path 適用前）。
    fn source(&self, name: &str) -> Option<Value> {
        match name {
            "input" => Some(self.ctx.input.clone()),
            "trigger" => Some(self.ctx.trigger.clone()),
            "run" => Some(serde_json::json!({
                "run_id": self.ctx.run_id.to_string(),
                "tenant_id": self.ctx.tenant_id,
            })),
            // `nodes.<id>.output` → 先行成功 step の出力。
            _ if name.starts_with("nodes.") && name.ends_with(".output") => {
                let id = &name["nodes.".len()..name.len() - ".output".len()];
                self.ctx.node_outputs.get(id).cloned()
            }
            // `each` / `each.item` / `each.index` は map 領域（Stage A 未対応）。
            _ => None,
        }
    }
}

impl ValueResolver for ParamResolver<'_> {
    fn resolve_from(&self, from: &FromRef) -> Option<Value> {
        let base = self.source(&from.from)?;
        match &from.path {
            Some(p) => base.pointer(p).cloned(),
            None => Some(base),
        }
    }
}

/// params の 1 フィールドを `ValueExpr` として解決する（未定義キーは `None`）。
#[must_use]
pub fn resolve_field(params: &Value, key: &str, r: &dyn ValueResolver) -> Option<Value> {
    let raw = params.get(key)?;
    // フィールドは $from / $template / リテラルのいずれか（untagged ValueExpr）。
    let expr: ValueExpr = serde_json::from_value(raw.clone()).ok()?;
    resolve_value(&expr, r)
}

/// 解決結果を文字列として取り出す（String のみ・型不一致は `None`）。
#[must_use]
pub fn as_string(v: &Value) -> Option<String> {
    v.as_str().map(ToString::to_string)
}

/// 解決結果を UUID として取り出す（UUID 文字列のみ）。
#[must_use]
pub fn as_uuid(v: &Value) -> Option<uuid::Uuid> {
    v.as_str().and_then(|s| uuid::Uuid::parse_str(s).ok())
}

/// 解決結果を u32 として取り出す。
#[must_use]
pub fn as_u32(v: &Value) -> Option<u32> {
    v.as_u64().and_then(|n| u32::try_from(n).ok())
}

/// 解決結果をバイト列として取り出す（`content` 等）。
///
/// - 文字列 → UTF-8 バイト（Stage A のテキスト書込の既定）。
/// - `{ "base64": "..." }` → base64 デコード（バイナリ）。
/// - その他 → JSON 直列化バイト（構造化データの保存）。
#[must_use]
pub fn as_bytes(v: &Value) -> Vec<u8> {
    use base64::Engine as _;
    match v {
        Value::String(s) => s.clone().into_bytes(),
        Value::Object(map) => {
            if let Some(Value::String(b64)) = map.get("base64") {
                if let Ok(bytes) = base64::engine::general_purpose::STANDARD.decode(b64) {
                    return bytes;
                }
            }
            serde_json::to_vec(v).unwrap_or_default()
        }
        other => serde_json::to_vec(other).unwrap_or_default(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use uuid::Uuid;

    fn ctx_with(input: Value, node_outputs: Value) -> NodeContext {
        NodeContext {
            tenant_id: "t1".into(),
            org: "acme".into(),
            run_id: Uuid::nil(),
            step_path: "n1".into(),
            idempotency_key: "wf:t1:0:n1".into(),
            attempt: 1,
            principal: "wf".into(),
            input,
            trigger: json!({}),
            node_outputs,
            trace_id: None,
            scope_ceiling: vec![],
        }
    }

    #[test]
    fn resolves_from_input_path() {
        let ctx = ctx_with(json!({ "file_id": "abc" }), Value::Null);
        let r = ParamResolver::new(&ctx);
        let params = json!({ "id": { "$from": "input", "path": "/file_id" } });
        assert_eq!(resolve_field(&params, "id", &r), Some(json!("abc")));
    }

    #[test]
    fn resolves_from_node_output() {
        let ctx = ctx_with(json!({}), json!({ "read_file": { "text": "hello" } }));
        let r = ParamResolver::new(&ctx);
        let params = json!({ "content": { "$from": "nodes.read_file.output", "path": "/text" } });
        assert_eq!(resolve_field(&params, "content", &r), Some(json!("hello")));
    }

    #[test]
    fn literal_and_default() {
        let ctx = ctx_with(json!({}), Value::Null);
        let r = ParamResolver::new(&ctx);
        let params = json!({
            "method": "POST",
            "topk": { "$from": "input", "path": "/missing", "default": 5 }
        });
        assert_eq!(resolve_field(&params, "method", &r), Some(json!("POST")));
        assert_eq!(resolve_field(&params, "topk", &r), Some(json!(5)));
        assert_eq!(resolve_field(&params, "absent", &r), None);
    }

    #[test]
    fn bytes_from_string_and_base64() {
        assert_eq!(as_bytes(&json!("hi")), b"hi".to_vec());
        assert_eq!(as_bytes(&json!({ "base64": "aGk=" })), b"hi".to_vec());
    }

    #[test]
    fn each_source_unsupported_in_stage_a() {
        let ctx = ctx_with(json!({}), Value::Null);
        let r = ParamResolver::new(&ctx);
        let params = json!({ "x": { "$from": "each", "path": "/item" } });
        assert_eq!(resolve_field(&params, "x", &r), None);
    }
}
