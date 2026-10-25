//! 制御ノード（branch/switch/join/map/wait・Task 10.5・engine.md §4）。
//!
//! - `eval`: 条件・値式の**純関数**評価（`$from`/`$template`/条件木）。
//! - branch/switch の出力ポート決定も純関数（[`branch_port`]/[`switch_port`]）。
//! - join の待ち合わせ・skip 伝播は [`run::readiness`](crate::run::readiness)（前進 TX が使う）。
//! - map（動的 fan-out）・wait（タイマ/イベント）の永続化は [`run::store`](crate::run::store)（checkpoint/
//!   scheduler 起床）で結線する。[`event_filter_matches`] はトリガ/wait のイベント filter 評価。

pub mod eval;
#[cfg(test)]
mod eval_semantics_tests;

use serde_json::Value;

use crate::ir::expr::{Condition, FromRef, ValueExpr};
use eval::{eval_condition, resolve_deep, EvalError, ValueResolver};

/// イベントペイロードに対する `$from` リゾルバ（トリガ/wait の filter 評価用）。
///
/// filter はイベントペイロードに対して評価する（engine.md §5.4）。`$from` の source は
/// `trigger` / `event` / `payload` がペイロード全体を指す（`path` で JSON Pointer 適用）。
pub struct PayloadResolver<'a> {
    pub payload: &'a Value,
}

impl ValueResolver for PayloadResolver<'_> {
    fn resolve_from(&self, from: &FromRef) -> Option<Value> {
        match from.from.as_str() {
            "trigger" | "event" | "payload" => match &from.path {
                Some(p) => self.payload.pointer(p).cloned(),
                None => Some(self.payload.clone()),
            },
            _ => None,
        }
    }
}

/// イベント filter がペイロードに一致するか。
///
/// 評価エラー（型不一致など）は `Err`。呼び出し側は発火・起床させない（fail-closed・engine.md §5.6）。
pub fn event_filter_matches(filter: &Condition, payload: &Value) -> Result<bool, EvalError> {
    eval_condition(filter, &PayloadResolver { payload })
}

/// branch の出力ポートを決める（条件成立で `"true"`、不成立で `"false"`）。評価エラーは step の失敗。
pub fn branch_port(
    condition: &Condition,
    r: &dyn ValueResolver,
) -> Result<&'static str, EvalError> {
    Ok(if eval_condition(condition, r)? {
        "true"
    } else {
        "false"
    })
}

/// switch の出力ポートを決める（`value` を各 case とリテラル一致で照合・無ければ `default`）。
///
/// `cases` は `(port_name, 一致リテラル)` の順序付きリスト。最初に一致した port を返す。
/// `value` が解決できない（default も無い）ときは `default` へ流さず失敗させる（ir.md §3.1）。
pub fn switch_port(
    value_expr: &ValueExpr,
    cases: &[(String, Value)],
    r: &dyn ValueResolver,
) -> Result<String, EvalError> {
    let value = resolve_deep(value_expr, r)?;
    for (port, expected) in cases {
        if &value == expected {
            return Ok(port.clone());
        }
    }
    Ok("default".to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ir::expr::{CmpOp, Comparison, FromRef};
    use serde_json::json;
    use std::collections::HashMap;

    struct MapResolver(HashMap<String, Value>);
    impl ValueResolver for MapResolver {
        fn resolve_from(&self, from: &FromRef) -> Option<Value> {
            let base = self.0.get(&from.from)?;
            match &from.path {
                Some(p) => base.pointer(p).cloned(),
                None => Some(base.clone()),
            }
        }
    }
    fn resolver(src: Value) -> MapResolver {
        MapResolver([("input".to_string(), src)].into_iter().collect())
    }
    fn from(path: &str) -> ValueExpr {
        ValueExpr::From(FromRef {
            from: "input".into(),
            path: Some(path.into()),
            default: None,
        })
    }

    #[test]
    fn branch_selects_true_false() {
        let r = resolver(json!({ "n": 5 }));
        let cond = Condition::Cmp(Comparison {
            left: from("/n"),
            op: CmpOp::Gt,
            right: Some(ValueExpr::Literal(json!(3))),
        });
        assert_eq!(branch_port(&cond, &r), Ok("true"));
        let cond2 = Condition::Cmp(Comparison {
            left: from("/n"),
            op: CmpOp::Gt,
            right: Some(ValueExpr::Literal(json!(10))),
        });
        assert_eq!(branch_port(&cond2, &r), Ok("false"));
    }

    #[test]
    fn switch_matches_case_or_default() {
        let r = resolver(json!({ "kind": "pdf" }));
        let cases = vec![
            ("img".to_string(), json!("png")),
            ("doc".to_string(), json!("pdf")),
        ];
        assert_eq!(switch_port(&from("/kind"), &cases, &r).unwrap(), "doc");

        let r2 = resolver(json!({ "kind": "zip" }));
        assert_eq!(switch_port(&from("/kind"), &cases, &r2).unwrap(), "default");

        // 値が解決できないときは default へ黙って流さない（expr_resolve_error）。
        let r3 = resolver(json!({}));
        assert_eq!(
            switch_port(&from("/kind"), &cases, &r3).unwrap_err().code,
            eval::EXPR_RESOLVE_ERROR
        );
    }
}
