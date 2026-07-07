//! 制御ノード（branch/switch/join/map/wait・Task 10.5・engine.md §4）。
//!
//! - `eval`: 条件・値式の**純関数**評価（`$from`/`$template`/条件木）。
//! - branch/switch の出力ポート決定も純関数（[`branch_port`]/[`switch_port`]）。
//! - join の待ち合わせ・skip 伝播は [`run::readiness`](crate::run::readiness)（前進 TX が使う）。
//! - map（動的 fan-out）・wait（タイマ/イベント）は実行時の永続化を伴う（最終 PR で結線）。

pub mod eval;

use serde_json::Value;

use crate::ir::expr::{Condition, ValueExpr};
use eval::{eval_condition, resolve_value, ValueResolver};

/// branch の出力ポートを決める（条件成立で `"true"`、不成立で `"false"`）。
pub fn branch_port(condition: &Condition, r: &dyn ValueResolver) -> &'static str {
    if eval_condition(condition, r) {
        "true"
    } else {
        "false"
    }
}

/// switch の出力ポートを決める（`value` を各 case とリテラル一致で照合・無ければ `default`）。
///
/// `cases` は `(port_name, 一致リテラル)` の順序付きリスト。最初に一致した port を返す。
pub fn switch_port(
    value_expr: &ValueExpr,
    cases: &[(String, Value)],
    r: &dyn ValueResolver,
) -> String {
    let value = resolve_value(value_expr, r);
    for (port, expected) in cases {
        if value.as_ref() == Some(expected) {
            return port.clone();
        }
    }
    "default".to_string()
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
        assert_eq!(branch_port(&cond, &r), "true");
        let cond2 = Condition::Cmp(Comparison {
            left: from("/n"),
            op: CmpOp::Gt,
            right: Some(ValueExpr::Literal(json!(10))),
        });
        assert_eq!(branch_port(&cond2, &r), "false");
    }

    #[test]
    fn switch_matches_case_or_default() {
        let r = resolver(json!({ "kind": "pdf" }));
        let cases = vec![
            ("img".to_string(), json!("png")),
            ("doc".to_string(), json!("pdf")),
        ];
        assert_eq!(switch_port(&from("/kind"), &cases, &r), "doc");

        let r2 = resolver(json!({ "kind": "zip" }));
        assert_eq!(switch_port(&from("/kind"), &cases, &r2), "default");
    }
}
