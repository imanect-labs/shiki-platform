//! 制御ノード（branch/switch/join/map/wait・Task 10.5・engine.md §4）。
//!
//! - `eval`: 条件・値式の**純関数**評価（`$from`/`$template`/条件木）。
//! - `date`: 日付の計算（「日付を計算」・`transform.date` の本体・純関数・GAP-15）。
//! - branch/switch の出力ポート決定も純関数（[`branch_port`]/[`switch_port`]）。
//! - join の待ち合わせ・skip 伝播は [`run::readiness`](crate::run::readiness)（前進 TX が使う）。
//! - map（動的 fan-out）・wait（タイマ/イベント）の永続化は [`run::store`](crate::run::store)（checkpoint/
//!   scheduler 起床）で結線する。[`event_filter_matches`] はトリガ/wait のイベント filter 評価。

pub mod date;
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

/// 到来したイベントを指す `$from` の source か（`event` が正・`trigger` / `payload` は互換の別名）。
#[must_use]
pub fn is_event_source(from: &str) -> bool {
    matches!(from, "event" | "trigger" | "payload")
}

/// wait(event) の filter を停止時に束縛する（GAP-07 の T3）。
///
/// 到来したイベントを指す参照（[`is_event_source`]）はそのまま残し、それ以外（`input`・`nodes.*`・`run`・
/// `each`）は `r`（停止したステップの解決文脈）で解決してリテラルに置き換える。起床の判定はイベントの
/// ペイロードしか解決しないので、束縛しないと「前のブロックの結果と同じ行」のような filter は常に偽になる。
/// 解決できない参照は停止の失敗（[`eval::EXPR_RESOLVE_ERROR`]・黙って待ち続けない）。
pub fn bind_wait_filter(cond: &Condition, r: &dyn ValueResolver) -> Result<Condition, EvalError> {
    Ok(match cond {
        Condition::All(cs) => Condition::All(
            cs.iter()
                .map(|c| bind_wait_filter(c, r))
                .collect::<Result<_, _>>()?,
        ),
        Condition::Any(cs) => Condition::Any(
            cs.iter()
                .map(|c| bind_wait_filter(c, r))
                .collect::<Result<_, _>>()?,
        ),
        Condition::Not(c) => Condition::Not(Box::new(bind_wait_filter(c, r)?)),
        Condition::Cmp(cmp) => Condition::Cmp(crate::ir::expr::Comparison {
            left: bind_value(&cmp.left, r)?,
            op: cmp.op,
            right: cmp.right.as_ref().map(|v| bind_value(v, r)).transpose()?,
        }),
    })
}

/// 値式 1 つの束縛（[`bind_wait_filter`]）。
fn bind_value(v: &ValueExpr, r: &dyn ValueResolver) -> Result<ValueExpr, EvalError> {
    Ok(match v {
        ValueExpr::From(f) if is_event_source(&f.from) => v.clone(),
        ValueExpr::From(_) => ValueExpr::Literal(resolve_deep(v, r)?),
        // テンプレートは差し込みごとに束縛する（イベントの差し込みは起床時に解決する）。
        ValueExpr::Template(t) => ValueExpr::Template(crate::ir::expr::TemplateExpr {
            template: t.template.clone(),
            vars: t
                .vars
                .iter()
                .map(|(k, x)| Ok((k.clone(), bind_value(x, r)?)))
                .collect::<Result<_, EvalError>>()?,
        }),
        ValueExpr::Literal(json) => ValueExpr::Literal(bind_json(json, r)?),
    })
}

/// リテラルの JSON の中の値式（`$from` / `$template`）を束縛する。
fn bind_json(v: &Value, r: &dyn ValueResolver) -> Result<Value, EvalError> {
    match v {
        Value::Object(map) if map.contains_key("$from") || map.contains_key("$template") => {
            match serde_json::from_value::<ValueExpr>(v.clone()) {
                Ok(ValueExpr::Literal(_)) | Err(_) => Ok(v.clone()),
                Ok(expr) => {
                    let bound = bind_value(&expr, r)?;
                    Ok(match bound {
                        ValueExpr::Literal(x) => x,
                        other => serde_json::to_value(other).unwrap_or(Value::Null),
                    })
                }
            }
        }
        Value::Object(map) => Ok(Value::Object(
            map.iter()
                .map(|(k, x)| Ok((k.clone(), bind_json(x, r)?)))
                .collect::<Result<_, EvalError>>()?,
        )),
        Value::Array(items) => items
            .iter()
            .map(|x| bind_json(x, r))
            .collect::<Result<Vec<_>, _>>()
            .map(Value::Array),
        other => Ok(other.clone()),
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

    /// wait(event) の filter は、イベント以外の参照だけを停止時のリテラルに束縛する（T3）。
    #[test]
    #[allow(clippy::panic)]
    fn wait_filter_binds_everything_but_the_event() {
        let r = MapResolver(
            [
                ("input".to_string(), json!({ "id": "r-1" })),
                ("nodes.make.output".to_string(), json!({ "id": "r-2" })),
            ]
            .into_iter()
            .collect(),
        );
        let event = |path: &str| {
            ValueExpr::From(FromRef {
                from: "event".into(),
                path: Some(path.into()),
                default: None,
            })
        };
        let node = ValueExpr::From(FromRef {
            from: "nodes.make.output".into(),
            path: Some("/id".into()),
            default: None,
        });
        let cond = Condition::All(vec![
            Condition::Cmp(Comparison {
                left: event("/record_id"),
                op: CmpOp::Eq,
                right: Some(node),
            }),
            Condition::Cmp(Comparison {
                left: from("/id"),
                op: CmpOp::Eq,
                right: Some(event("/record_id")),
            }),
        ]);
        let bound = bind_wait_filter(&cond, &r).unwrap();
        let Condition::All(cs) = &bound else {
            panic!("{bound:?}")
        };
        let Condition::Cmp(a) = &cs[0] else { panic!() };
        assert!(matches!(&a.left, ValueExpr::From(f) if f.from == "event"));
        assert_eq!(a.right, Some(ValueExpr::Literal(json!("r-2"))));
        let Condition::Cmp(b) = &cs[1] else { panic!() };
        assert_eq!(b.left, ValueExpr::Literal(json!("r-1")));
        // 束縛した filter はイベントのペイロードだけで評価できる。
        assert_eq!(
            event_filter_matches(&bound, &json!({ "record_id": "r-2" })),
            Ok(false),
            "r-1 と r-2 の両方には一致しない"
        );
        // 解決できない参照は停止の失敗。
        let missing = Condition::Cmp(Comparison {
            left: event("/x"),
            op: CmpOp::Eq,
            right: Some(from("/nope")),
        });
        assert_eq!(
            bind_wait_filter(&missing, &r).unwrap_err().code,
            eval::EXPR_RESOLVE_ERROR
        );
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
