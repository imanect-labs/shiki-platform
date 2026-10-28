//! ノード params の `$from`/`$template`/リテラル解決（engine.md §4.1・ir.md §3）。
//!
//! [`ParamResolver`] は [`ValueResolver`](crate::control::eval::ValueResolver) を実装し、
//! `$from` の源（`input` / `trigger` / `run` / `nodes.<id>.output`）を [`NodeContext`] から供給する。
//! executor は本モジュールのヘルパ（[`param`] / [`opt_param`] ＋型変換）で params の値式フィールドを
//! 1 つずつ解決する（params 全体へは掛けない・PIT-36）。解決は [`resolve_deep`] の 1 か所に集める。
//! `each`（map 領域の要素コンテキスト）は [`NodeContext::each`] から解決する（領域外は `None`）。

use serde_json::Value;

use crate::control::eval::{resolve_deep, EvalError, ValueResolver};
use crate::ir::expr::{FromRef, ValueExpr};
use crate::run::NodeContext;

use super::ports::PortError;

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
            // 実行情報（項目は vocab の単一定義・実行した日時は日本時間・D-97）。
            "run" => Some(crate::vocab::run_info(
                self.ctx.run_id,
                &self.ctx.tenant_id,
                self.ctx.run_started_at,
            )),
            // `nodes.<id>.output` → 先行成功 step の出力。
            _ if name.starts_with("nodes.") && name.ends_with(".output") => {
                let id = &name["nodes.".len()..name.len() - ".output".len()];
                self.ctx.node_outputs.get(id).cloned()
            }
            // `each` → map 領域の要素コンテキスト（`{item,index}`・path で `/item`・`/index`）。ir.md §3.1。
            "each" => self.ctx.each.clone(),
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

/// params の 1 フィールドを `ValueExpr` として解決する（未定義キーは `Ok(None)`）。
pub fn resolve_field(
    params: &Value,
    key: &str,
    r: &dyn ValueResolver,
) -> Result<Option<Value>, EvalError> {
    let Some(raw) = params.get(key) else {
        return Ok(None);
    };
    // フィールドは $from / $template / リテラルのいずれか（ValueExpr の手書き Deserialize は失敗しない）。
    let expr: ValueExpr =
        serde_json::from_value(raw.clone()).unwrap_or_else(|_| ValueExpr::Literal(raw.clone()));
    resolve_deep(&expr, r).map(Some)
}

/// 必須の値式フィールドを解決する。解決できなければ `expr_resolve_error`（`what` は「ノード: 項目」）。
///
/// 解決の失敗は permanent（同じ入力で再試行しても解決しない）。
pub fn param(e: &ValueExpr, r: &dyn ValueResolver, what: &str) -> Result<Value, PortError> {
    resolve_deep(e, r)
        .map_err(|err| PortError::new(err.code, format!("{what}: {}", err.message), false))
}

/// 省略できる値式フィールド。**項目が無いときだけ** `None` を返す。
///
/// 書いてある参照が解決できなければ失敗させ、黙って既定値（ルートフォルダ・run 入力全体・空文字など）へ
/// 落とさない（書き先の取り違えを防ぐ・ir.md §3.1）。
pub fn opt_param(
    e: Option<&ValueExpr>,
    r: &dyn ValueResolver,
    what: &str,
) -> Result<Option<Value>, PortError> {
    e.map(|e| param(e, r, what)).transpose()
}

/// 解決した値の型を確かめる（違えば `invalid`・`kind` は「文字列」「UUID」など）。
pub fn typed<T>(
    v: &Value,
    what: &str,
    kind: &str,
    conv: impl FnOnce(&Value) -> Option<T>,
) -> Result<T, PortError> {
    conv(v).ok_or_else(|| {
        // 「UUID」のような英字の型名は前後を空ける（「文字列」は詰める）。
        let kind = if kind.is_ascii() {
            format!(" {kind} ")
        } else {
            kind.to_string()
        };
        PortError::invalid(format!("{what} が{kind}ではありません"))
    })
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
            run_started_at: chrono::Utc::now(),
            step_path: "n1".into(),
            idempotency_key: "wf:t1:0:n1".into(),
            attempt: 1,
            principal: "wf".into(),
            principal_kind: "workflow".into(),
            input,
            trigger: json!({}),
            node_outputs,
            each: None,
            trace_id: None,
            scope_ceiling: vec![],
            cancel: crate::run::CancelSignal::default(),
            workflow_id: Uuid::nil(),
            cache: None,
        }
    }

    fn ctx_with_each(each: Value) -> NodeContext {
        NodeContext {
            each: Some(each),
            ..ctx_with(json!({}), Value::Null)
        }
    }

    #[test]
    fn resolves_from_input_path() {
        let ctx = ctx_with(json!({ "file_id": "abc" }), Value::Null);
        let r = ParamResolver::new(&ctx);
        let params = json!({ "id": { "$from": "input", "path": "/file_id" } });
        assert_eq!(resolve_field(&params, "id", &r), Ok(Some(json!("abc"))));
    }

    #[test]
    fn resolves_from_node_output() {
        let ctx = ctx_with(json!({}), json!({ "read_file": { "text": "hello" } }));
        let r = ParamResolver::new(&ctx);
        let params = json!({ "content": { "$from": "nodes.read_file.output", "path": "/text" } });
        assert_eq!(
            resolve_field(&params, "content", &r),
            Ok(Some(json!("hello")))
        );
    }

    #[test]
    fn literal_and_default() {
        let ctx = ctx_with(json!({}), Value::Null);
        let r = ParamResolver::new(&ctx);
        let params = json!({
            "method": "POST",
            "topk": { "$from": "input", "path": "/missing", "default": 5 }
        });
        assert_eq!(
            resolve_field(&params, "method", &r),
            Ok(Some(json!("POST")))
        );
        assert_eq!(resolve_field(&params, "topk", &r), Ok(Some(json!(5))));
        assert_eq!(resolve_field(&params, "absent", &r), Ok(None));
    }

    #[test]
    fn bytes_from_string_and_base64() {
        assert_eq!(as_bytes(&json!("hi")), b"hi".to_vec());
        assert_eq!(as_bytes(&json!({ "base64": "aGk=" })), b"hi".to_vec());
    }

    #[test]
    fn each_source_resolves_item_and_index() {
        let ctx = ctx_with_each(json!({ "item": { "name": "a.txt" }, "index": 3 }));
        let r = ParamResolver::new(&ctx);
        let item = json!({ "x": { "$from": "each", "path": "/item/name" } });
        assert_eq!(resolve_field(&item, "x", &r), Ok(Some(json!("a.txt"))));
        let idx = json!({ "x": { "$from": "each", "path": "/index" } });
        assert_eq!(resolve_field(&idx, "x", &r), Ok(Some(json!(3))));
    }

    #[test]
    fn each_source_none_outside_region() {
        let ctx = ctx_with(json!({}), Value::Null);
        let r = ParamResolver::new(&ctx);
        let params = json!({ "x": { "$from": "each", "path": "/item" } });
        assert_eq!(
            resolve_field(&params, "x", &r).unwrap_err().code,
            crate::control::eval::EXPR_RESOLVE_ERROR
        );
    }

    #[test]
    fn opt_param_falls_back_only_when_absent() {
        let ctx = ctx_with(json!({ "folder": "not-a-uuid" }), Value::Null);
        let r = ParamResolver::new(&ctx);
        assert_eq!(opt_param(None, &r, "storage.write: folder").unwrap(), None);
        // 書いてある参照が解決できなければ既定値（ルート）へ落とさず失敗。
        let missing: ValueExpr =
            serde_json::from_value(json!({ "$from": "input", "path": "/nope" })).unwrap();
        let err = opt_param(Some(&missing), &r, "storage.write: folder").unwrap_err();
        assert_eq!(err.code, "expr_resolve_error");
        assert!(
            err.message.starts_with("storage.write: folder: "),
            "{}",
            err.message
        );
        // 解決できても型が違えば invalid（黙って None にしない）。
        let present: ValueExpr =
            serde_json::from_value(json!({ "$from": "input", "path": "/folder" })).unwrap();
        let v = opt_param(Some(&present), &r, "storage.write: folder")
            .unwrap()
            .unwrap();
        let err = typed(&v, "storage.write: folder", "UUID", as_uuid).unwrap_err();
        assert_eq!(err.code, "invalid");
    }
}
