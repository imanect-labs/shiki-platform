//! データフロー模型（式言語なし・ir.md §3）。
//!
//! パラメータ値は 3 種のみ: リテラル・`$from`（参照）・`$template`（文字列組み立て）。
//! 条件は閉じた op の条件木（`control.branch` / イベント filter / `control.wait` で共用）。

use serde::{Deserialize, Serialize};
use ts_rs::TS;

/// 条件木のネスト最大深さ（V7）。
pub const MAX_CONDITION_DEPTH: usize = 5;
/// `$template` の vars 上限（V7）。
pub const MAX_TEMPLATE_VARS: usize = 50;
/// regex パターン長上限（V5）。
pub const MAX_REGEX_LEN: usize = 256;

/// `$from` 参照（`{ "$from": "...", "path": "<JSON Pointer>", "default": ... }`・ir.md §3.1）。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS)]
#[serde(deny_unknown_fields)]
#[ts(export)]
pub struct FromRef {
    /// source: `input` / `trigger` / `nodes.<id>.output` / `run` / `each`（`each.item`/`each.index`）。
    #[serde(rename = "$from")]
    pub from: String,
    /// JSON Pointer（省略時は source 全体）。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub path: Option<String>,
    /// 解決失敗時の既定値（無ければ解決失敗で step 失敗）。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(type = "unknown")]
    pub default: Option<serde_json::Value>,
}

/// `$template` 文字列組み立て（`{name}` は vars のキーのみ・ir.md §3.2）。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS)]
#[serde(deny_unknown_fields)]
#[ts(export)]
pub struct TemplateExpr {
    #[serde(rename = "$template")]
    pub template: String,
    #[serde(default)]
    pub vars: std::collections::BTreeMap<String, ValueExpr>,
}

/// パラメータ値（リテラル / `$from` / `$template`）。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS)]
#[serde(untagged)]
#[ts(export)]
pub enum ValueExpr {
    /// `$from` 参照。
    From(FromRef),
    /// `$template` 文字列組み立て。
    Template(TemplateExpr),
    /// リテラル JSON（object/array/string/number/bool/null）。
    Literal(#[ts(type = "unknown")] serde_json::Value),
}

impl ValueExpr {
    /// `$from` 参照ならその source を返す。
    pub fn as_from(&self) -> Option<&FromRef> {
        match self {
            ValueExpr::From(f) => Some(f),
            _ => None,
        }
    }
}

/// 条件木（`all`/`any`/`not` 合成＋閉じた op・ir.md §3.3）。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS)]
#[serde(rename_all = "snake_case")]
#[ts(export)]
pub enum Condition {
    /// 全て真。
    All(Vec<Condition>),
    /// いずれか真。
    Any(Vec<Condition>),
    /// 否定。
    Not(Box<Condition>),
    /// 比較（左辺は値式・右辺は演算子ごとの引数）。
    Cmp(Comparison),
}

/// 比較演算子（閉じた集合・ir.md §3.3）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(rename_all = "snake_case")]
#[ts(export)]
pub enum CmpOp {
    Eq,
    Neq,
    Lt,
    Lte,
    Gt,
    Gte,
    In,
    Contains,
    StartsWith,
    EndsWith,
    Exists,
    IsNull,
    Matches,
}

/// 単項比較。`left` を評価し `op` と `right`（省略可: exists/is_null）で判定する。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS)]
#[serde(deny_unknown_fields)]
#[ts(export)]
pub struct Comparison {
    pub left: ValueExpr,
    pub op: CmpOp,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub right: Option<ValueExpr>,
}

impl Condition {
    /// 条件木のネスト深さ（V7 検証用）。
    pub fn depth(&self) -> usize {
        match self {
            Condition::All(cs) | Condition::Any(cs) => {
                1 + cs.iter().map(Condition::depth).max().unwrap_or(0)
            }
            Condition::Not(c) => 1 + c.depth(),
            Condition::Cmp(_) => 1,
        }
    }

    /// 木を走査して各比較へ関数を適用する（regex コンパイル検証・祖先性検証で使う）。
    pub fn for_each_cmp(&self, f: &mut impl FnMut(&Comparison)) {
        match self {
            Condition::All(cs) | Condition::Any(cs) => {
                for c in cs {
                    c.for_each_cmp(f);
                }
            }
            Condition::Not(c) => c.for_each_cmp(f),
            Condition::Cmp(cmp) => f(cmp),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn from_ref_deny_unknown() {
        let ok: Result<FromRef, _> =
            serde_json::from_value(json!({"$from": "input", "path": "/a"}));
        assert!(ok.is_ok());
        let bad: Result<FromRef, _> = serde_json::from_value(json!({"$from": "input", "typo": 1}));
        assert!(bad.is_err(), "未知フィールドは拒否");
    }

    #[test]
    fn value_expr_untagged() {
        let f: ValueExpr = serde_json::from_value(json!({"$from": "input"})).unwrap();
        assert!(matches!(f, ValueExpr::From(_)));
        let t: ValueExpr =
            serde_json::from_value(json!({"$template": "hi {n}", "vars": {}})).unwrap();
        assert!(matches!(t, ValueExpr::Template(_)));
        let l: ValueExpr = serde_json::from_value(json!(42)).unwrap();
        assert!(matches!(l, ValueExpr::Literal(_)));
    }

    #[test]
    fn condition_depth() {
        let c = Condition::All(vec![
            Condition::Cmp(Comparison {
                left: ValueExpr::Literal(json!(1)),
                op: CmpOp::Eq,
                right: Some(ValueExpr::Literal(json!(1))),
            }),
            Condition::Not(Box::new(Condition::Cmp(Comparison {
                left: ValueExpr::Literal(json!(2)),
                op: CmpOp::Gt,
                right: Some(ValueExpr::Literal(json!(1))),
            }))),
        ]);
        assert_eq!(c.depth(), 3); // all -> not -> cmp
    }
}
