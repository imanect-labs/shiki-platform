//! 数式の閉じた AST（data-platform §7.3）。
//!
//! 自由文字列を SQL にしない（LLM・開発者由来の任意式が SQL になる経路を作らない）。式は
//! この列挙の木としてだけ保存・評価し、Rust のインタプリタ（[`super::eval`]）で計算する。
//! フィールドは参照名（`key`）で指す（外部契約は key・data-platform §4）。

use serde::{Deserialize, Serialize};
use serde_json::Value;
use utoipa::ToSchema;

use crate::derived::types::RollupOp;
use crate::DataError;

/// 式の深さの上限（スキーマ保存時に 422）。
pub const MAX_FORMULA_DEPTH: usize = 16;
/// 式のノード数の上限。
pub(crate) const MAX_FORMULA_NODES: usize = 256;
/// 文字列リテラルの長さの上限（バイト）。
const MAX_LITERAL_BYTES: usize = 1_000;
/// `round` の小数桁の上限。
pub(crate) const MAX_ROUND_SCALE: u8 = 9;

/// 比較演算子。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum FormulaCmpOp {
    Eq,
    Ne,
    Lt,
    Lte,
    Gt,
    Gte,
}

/// `switch` の分岐。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SwitchCase {
    pub when: FormulaExpr,
    pub then: FormulaExpr,
}

/// 数式（閉じた AST）。
///
/// 値の型は number（`numeric` 精度の 10 進）・text・bool・date・date_time・null。演算の被演算子に
/// null や型の合わない値が来たら結果は null（例外にしない）。0 除算も null。
///
/// OpenAPI のスキーマは再帰の扱いのため [`super::schema`] で組み立てる（serde の形と同じ）。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FormulaExpr {
    /// 自テーブルのフィールド（参照名）。
    Field(String),
    /// リテラル（number / string / bool / null）。
    Lit(Value),
    Add(Box<FormulaExpr>, Box<FormulaExpr>),
    Sub(Box<FormulaExpr>, Box<FormulaExpr>),
    Mul(Box<FormulaExpr>, Box<FormulaExpr>),
    /// 除算（被演算子の小数桁の大きい方 + 6 桁へ偶数丸め）。
    Div(Box<FormulaExpr>, Box<FormulaExpr>),
    /// 小数 `n` 桁へ四捨五入（0 から遠い方へ・0〜9 桁）。
    Round(Box<FormulaExpr>, u8),
    Concat(Vec<FormulaExpr>),
    Upper(Box<FormulaExpr>),
    Lower(Box<FormulaExpr>),
    /// 日付（date / date_time）に日数（整数）を足す。
    DateAdd(Box<FormulaExpr>, Box<FormulaExpr>),
    /// 日数の差（`to - from`・UTC の日付で数える）。
    DateDiff(Box<FormulaExpr>, Box<FormulaExpr>),
    /// 現在時刻（UTC）。**含む式は `volatile`**（材料化しない・§7.4）。
    Now,
    /// `if(条件, 真, 偽)`（条件が true のときだけ真の枝）。
    If(Box<FormulaExpr>, Box<FormulaExpr>, Box<FormulaExpr>),
    Switch {
        value: Box<FormulaExpr>,
        cases: Vec<SwitchCase>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        default: Option<Box<FormulaExpr>>,
    },
    And(Vec<FormulaExpr>),
    Or(Vec<FormulaExpr>),
    Not(Box<FormulaExpr>),
    Cmp(Box<FormulaExpr>, FormulaCmpOp, Box<FormulaExpr>),
    /// リンク列 `link` を辿った参照先 `target` の集計（ロールアップ列と同じ規則・閲覧者の権限で評価）。
    Rollup {
        link: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        target: Option<String>,
        op: RollupOp,
    },
}

impl FormulaExpr {
    /// 子ノード（評価順）。
    pub(crate) fn children(&self) -> Vec<&FormulaExpr> {
        use FormulaExpr as E;
        match self {
            E::Field(_) | E::Lit(_) | E::Now | E::Rollup { .. } => Vec::new(),
            E::Add(a, b)
            | E::Sub(a, b)
            | E::Mul(a, b)
            | E::Div(a, b)
            | E::DateAdd(a, b)
            | E::DateDiff(a, b)
            | E::Cmp(a, _, b) => vec![a, b],
            E::Round(a, _) | E::Upper(a) | E::Lower(a) | E::Not(a) => vec![a],
            E::Concat(v) | E::And(v) | E::Or(v) => v.iter().collect(),
            E::If(c, a, b) => vec![c, a, b],
            E::Switch {
                value,
                cases,
                default,
            } => {
                let mut out: Vec<&FormulaExpr> = vec![value];
                for c in cases {
                    out.push(&c.when);
                    out.push(&c.then);
                }
                if let Some(d) = default {
                    out.push(d);
                }
                out
            }
        }
    }

    /// 前順（DFS）でノードを訪れる。
    pub(crate) fn walk<'a>(&'a self, f: &mut impl FnMut(&'a FormulaExpr)) {
        f(self);
        for c in self.children() {
            c.walk(f);
        }
    }

    /// 参照する自テーブルのフィールド（`field` と式内ロールアップのリンク列）。
    pub(crate) fn local_refs(&self) -> Vec<&str> {
        let mut out = Vec::new();
        self.walk(&mut |e| match e {
            FormulaExpr::Field(k) => out.push(k.as_str()),
            FormulaExpr::Rollup { link, .. } => out.push(link.as_str()),
            _ => {}
        });
        out
    }

    /// 式内のロールアップ（DFS 順）。
    pub(crate) fn rollups(&self) -> Vec<&FormulaExpr> {
        let mut out = Vec::new();
        self.walk(&mut |e| {
            if matches!(e, FormulaExpr::Rollup { .. }) {
                out.push(e);
            }
        });
        out
    }

    /// `now` を含むか（含めば `volatile`）。
    pub(crate) fn uses_now(&self) -> bool {
        let mut found = false;
        self.walk(&mut |e| found |= matches!(e, FormulaExpr::Now));
        found
    }

    fn depth(&self) -> usize {
        1 + self.children().iter().map(|c| c.depth()).max().unwrap_or(0)
    }

    fn count(&self) -> usize {
        let mut n = 0;
        self.walk(&mut |_| n += 1);
        n
    }
}

/// 構造の検証（深さ・ノード数・リテラル）。参照の解決と型は [`crate::derived::graph`] が見る。
pub(crate) fn validate_shape(name: &str, e: &FormulaExpr) -> Result<(), DataError> {
    if e.depth() > MAX_FORMULA_DEPTH {
        return Err(crate::schema_invalid(format!(
            "'{name}': 数式が深すぎます（最大 {MAX_FORMULA_DEPTH}）"
        )));
    }
    if e.count() > MAX_FORMULA_NODES {
        return Err(crate::schema_invalid(format!(
            "'{name}': 数式が大きすぎます（最大 {MAX_FORMULA_NODES} ノード）"
        )));
    }
    let mut err: Option<DataError> = None;
    e.walk(&mut |n| {
        if err.is_some() {
            return;
        }
        match n {
            FormulaExpr::Lit(v) => match v {
                Value::String(s) if s.len() > MAX_LITERAL_BYTES => {
                    err = Some(crate::schema_invalid(format!(
                        "'{name}': 数式の文字列リテラルが長すぎます"
                    )));
                }
                Value::Array(_) | Value::Object(_) => {
                    err = Some(crate::schema_invalid(format!(
                        "'{name}': 数式のリテラルは number / string / bool / null のみです"
                    )));
                }
                Value::Number(num) if super::decimal::Decimal::from_number(num).is_none() => {
                    err = Some(crate::schema_invalid(format!(
                        "'{name}': 数式の数値リテラルを正確に表せません"
                    )));
                }
                _ => {}
            },
            FormulaExpr::Round(_, s) if *s > MAX_ROUND_SCALE => {
                err = Some(crate::schema_invalid(format!(
                    "'{name}': round の桁数は 0〜{MAX_ROUND_SCALE} です"
                )));
            }
            FormulaExpr::Concat(v) | FormulaExpr::And(v) | FormulaExpr::Or(v) if v.is_empty() => {
                err = Some(crate::schema_invalid(format!(
                    "'{name}': concat / and / or の引数が空です"
                )));
            }
            FormulaExpr::Switch { cases, .. } if cases.is_empty() => {
                err = Some(crate::schema_invalid(format!(
                    "'{name}': switch の分岐が空です"
                )));
            }
            _ => {}
        }
    });
    err.map_or(Ok(()), Err)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn serde_is_closed_and_snake_case() {
        let e: FormulaExpr = serde_json::from_value(json!({
            "mul": [{"field": "price"}, {"lit": 3}]
        }))
        .unwrap();
        assert_eq!(
            e,
            FormulaExpr::Mul(
                Box::new(FormulaExpr::Field("price".into())),
                Box::new(FormulaExpr::Lit(json!(3)))
            )
        );
        let now: FormulaExpr = serde_json::from_value(json!("now")).unwrap();
        assert!(now.uses_now());
        // 閉じた集合の外（生 SQL など）はデシリアライズ失敗。
        assert!(serde_json::from_value::<FormulaExpr>(json!({"sql": "1"})).is_err());
    }

    #[test]
    fn shape_limits_are_enforced() {
        let mut e = FormulaExpr::Lit(json!(1));
        for _ in 0..MAX_FORMULA_DEPTH {
            e = FormulaExpr::Not(Box::new(e));
        }
        assert!(validate_shape("f", &e).is_err());
        let ok = FormulaExpr::Add(
            Box::new(FormulaExpr::Field("a".into())),
            Box::new(FormulaExpr::Lit(json!(0.1))),
        );
        assert!(validate_shape("f", &ok).is_ok());
        assert!(validate_shape("f", &FormulaExpr::Lit(json!([1]))).is_err());
        assert!(validate_shape("f", &FormulaExpr::Round(Box::new(ok), 10)).is_err());
    }

    #[test]
    fn refs_and_rollups_are_collected_in_order() {
        let e = FormulaExpr::Add(
            Box::new(FormulaExpr::Field("a".into())),
            Box::new(FormulaExpr::Rollup {
                link: "items".into(),
                target: Some("amount".into()),
                op: RollupOp::Sum,
            }),
        );
        assert_eq!(e.local_refs(), vec!["a", "items"]);
        assert_eq!(e.rollups().len(), 1);
        assert!(!e.uses_now());
    }
}
