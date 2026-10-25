//! 条件・値式の評価（**純関数**・ir.md §3・engine.md §4.3）。
//!
//! 値解決は [`ValueResolver`] 経由（`$from` の source→値写像は呼び出し側＝ワーカーが供給）。
//! **params の値の位置はすべて [`resolve_deep`] 1 か所で解決する**（リテラルの object / array の中に置いた
//! `$from` / `$template` も展開する・ir.md §3.1「任意の位置」）。解決できない参照は黙って null や空文字に
//! せず [`EXPR_RESOLVE_ERROR`] で step を失敗させる（ir.md §3.1）。
//! 条件木は閉じた op のみ。regex は `matches` でのみ使う（パターン長は保存時 V5 で上限済み）。

use serde_json::{Map, Value};

use crate::ir::expr::{
    template_pieces, CmpOp, Comparison, Condition, FromRef, TemplateExpr, TemplatePiece, ValueExpr,
};

/// 参照・テンプレートが解決できない（default も無い）。
pub const EXPR_RESOLVE_ERROR: &str = "expr_resolve_error";
/// 条件の比較で型が合わない（ir.md §3.3）。
pub const EXPR_TYPE_ERROR: &str = "expr_type_error";

/// `$from` 参照を実行時の値へ解決する（source→値の写像を持つ）。
pub trait ValueResolver {
    /// 参照を解決する（path を当てた値。default は評価器が当てる）。解決できなければ `None`。
    fn resolve_from(&self, from: &FromRef) -> Option<Value>;
}

/// 値式・条件の評価エラー（step を permanent に失敗させる・ir.md §3）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EvalError {
    /// [`EXPR_RESOLVE_ERROR`] / [`EXPR_TYPE_ERROR`]。
    pub code: &'static str,
    /// 利用者向けの説明（値そのものは載せない・run 履歴に残るため）。
    pub message: String,
}

impl EvalError {
    fn resolve(message: impl Into<String>) -> Self {
        EvalError {
            code: EXPR_RESOLVE_ERROR,
            message: message.into(),
        }
    }

    fn type_mismatch(message: impl Into<String>) -> Self {
        EvalError {
            code: EXPR_TYPE_ERROR,
            message: message.into(),
        }
    }
}

/// 値式を値へ解決する（リテラル / `$from` / `$template`）。
///
/// - `$from`: 値が無い（パス不在・null）なら default、それも無ければ [`EXPR_RESOLVE_ERROR`]。
///   値が null で default も無いときは null のまま返す（項目を空にする書込などで使う）。
/// - `$template`: `{name}` を vars で置き換える。vars に無い名前・解決できない var（`$from` の値が null で
///   default も無いものを含む・空文字を黙って埋め込まない）は [`EXPR_RESOLVE_ERROR`]。
/// - リテラル: object / array の中の `$from` / `$template` も再帰的に解決する。
///
/// params 全体へは掛けない。値式（`ValueExpr`）型のフィールドにだけ使う（URL・secret 名・skill 参照などの
/// String 型フィールドは参照を置けないこと自体が型防御・PIT-36）。
pub fn resolve_deep(expr: &ValueExpr, r: &dyn ValueResolver) -> Result<Value, EvalError> {
    match expr {
        ValueExpr::From(f) => lookup(f, r).ok_or_else(|| unresolved(f)),
        ValueExpr::Template(t) => resolve_template(t, r).map(Value::String),
        ValueExpr::Literal(v) => resolve_deep_json(v, r),
    }
}

/// JSON の中の `$from` / `$template` オブジェクトを値へ置き換える（値の形は変えない）。
///
/// data.query の比較値のように、値式型でなく data の型で受ける params の中の差し込みにも使う。
/// 形の崩れた値式（`$from` キーがあるのに読めない）は解決できないものとして失敗させる（保存時は V5 が弾く）。
pub fn resolve_deep_json(v: &Value, r: &dyn ValueResolver) -> Result<Value, EvalError> {
    match v {
        Value::Object(map) if map.contains_key("$from") || map.contains_key("$template") => {
            match serde_json::from_value::<ValueExpr>(v.clone()) {
                // 手書きの Deserialize は形の崩れた値式をリテラルとして読む（ここで再帰すると止まらない）。
                Ok(ValueExpr::Literal(_)) | Err(_) => Err(EvalError::resolve(
                    "値式（$from / $template）の形が正しくありません",
                )),
                Ok(expr) => resolve_deep(&expr, r),
            }
        }
        Value::Object(map) => {
            let mut out = Map::with_capacity(map.len());
            for (k, x) in map {
                out.insert(k.clone(), resolve_deep_json(x, r)?);
            }
            Ok(Value::Object(out))
        }
        Value::Array(items) => items
            .iter()
            .map(|x| resolve_deep_json(x, r))
            .collect::<Result<Vec<_>, _>>()
            .map(Value::Array),
        other => Ok(other.clone()),
    }
}

/// `$from` の値（値が無い・null のときは default・ir.md §3.1）。どちらも無ければ `None`。
fn lookup(f: &FromRef, r: &dyn ValueResolver) -> Option<Value> {
    match r.resolve_from(f) {
        None | Some(Value::Null) if f.default.is_some() => f.default.clone(),
        v => v,
    }
}

fn unresolved(f: &FromRef) -> EvalError {
    let path = f.path.as_deref().unwrap_or("");
    EvalError::resolve(format!(
        "$from {} {path} が解決できません（値が無く default もありません）",
        f.from
    ))
}

/// `$template` を組み立てる（字句は保存時検証と同じ [`template_pieces`]）。
fn resolve_template(t: &TemplateExpr, r: &dyn ValueResolver) -> Result<String, EvalError> {
    let mut out = String::with_capacity(t.template.len());
    for piece in template_pieces(&t.template) {
        match piece {
            TemplatePiece::Text(s) => out.push_str(s),
            TemplatePiece::Escaped(c) => out.push(c),
            TemplatePiece::Var(name) => {
                let Some(var) = t.vars.get(name) else {
                    return Err(EvalError::resolve(format!(
                        "$template の {{{name}}} が vars にありません"
                    )));
                };
                let v = match var {
                    // 値式全体の null（項目を空にする書込など）と違い、差し込みの null は空文字として
                    // 黙って埋まるだけなので、解決できないものとして扱う（ir.md §3.1）。
                    ValueExpr::From(f) => lookup(f, r)
                        .filter(|v| !v.is_null())
                        .ok_or_else(|| unresolved(f)),
                    other => resolve_deep(other, r),
                }
                .map_err(|e| EvalError {
                    code: e.code,
                    message: format!("$template の {{{name}}}: {}", e.message),
                })?;
                out.push_str(&value_to_string(&v));
            }
        }
    }
    Ok(out)
}

/// 値を文字列へ（テンプレート埋め込み・文字列比較用）。
fn value_to_string(v: &Value) -> String {
    match v {
        Value::String(s) => s.clone(),
        Value::Null => String::new(),
        other => other.to_string(),
    }
}

/// 条件木を評価する（`all`/`any`/`not`＋比較）。
///
/// 比較の値の `$from` が解決できないときは「値なし」として評価する（data の評価規則と同じ表・
/// `semantics_table.json`）。リテラルの中の参照（`in` の候補の配列など）・テンプレートは
/// [`resolve_deep`] と同じく解決できなければ失敗させる。大小比較の型不一致は [`EXPR_TYPE_ERROR`]。
pub fn eval_condition(cond: &Condition, r: &dyn ValueResolver) -> Result<bool, EvalError> {
    match cond {
        Condition::All(cs) => {
            for c in cs {
                if !eval_condition(c, r)? {
                    return Ok(false);
                }
            }
            Ok(true)
        }
        Condition::Any(cs) => {
            for c in cs {
                if eval_condition(c, r)? {
                    return Ok(true);
                }
            }
            Ok(false)
        }
        Condition::Not(c) => Ok(!eval_condition(c, r)?),
        Condition::Cmp(cmp) => eval_cmp(cmp, r),
    }
}

/// 比較の値を解決する（最上位の `$from` は解決できなければ値なし＝`None`）。
fn resolve_operand(e: &ValueExpr, r: &dyn ValueResolver) -> Result<Option<Value>, EvalError> {
    match e {
        ValueExpr::From(f) => Ok(lookup(f, r)),
        other => resolve_deep(other, r).map(Some),
    }
}

/// 左辺を「値の集合」として見る（data の多値列と同じ模型・query-optimizer.md §3.1）。
///
/// 値なし（未解決・`null`）と空配列は空集合、配列は要素の集合（`null` の要素は値なし）、
/// それ以外は要素 1 個の集合。
fn value_set(v: Option<&Value>) -> Vec<&Value> {
    match v {
        None | Some(Value::Null) => Vec::new(),
        Some(Value::Array(items)) => items.iter().filter(|x| !x.is_null()).collect(),
        Some(other) => vec![other],
    }
}

/// 等値（数値は数値として比較する: `1 = 1.0`。整数同士は精度を落とさない）。
fn same(lhs: &Value, rhs: &Value) -> bool {
    match (lhs, rhs) {
        (Value::Number(l), Value::Number(r)) => {
            if let (Some(li), Some(ri)) = (l.as_i64(), r.as_i64()) {
                li == ri
            } else if let (Some(lu), Some(ru)) = (l.as_u64(), r.as_u64()) {
                lu == ru
            } else {
                // 浮動小数の等値は JSON の数値の値としての一致（誤差を許さない・data の 10 進比較と同じ結果）。
                l.as_f64().zip(r.as_f64()).is_some_and(|(lf, rf)| {
                    lf.partial_cmp(&rf).is_some_and(std::cmp::Ordering::is_eq)
                })
            }
        }
        _ => lhs == rhs,
    }
}

/// 比較 1 つを評価する（2 値論理・data の評価規則と同じ表・query-optimizer.md §3.1）。
///
/// - `eq c`: 値のいずれかが c に等しい（単値は値 = c・多値は「c を含む」・値なしは偽）
/// - `neq c`: `not(eq c)`（値なしは**真**・多値は「c を含まない」）
/// - `in [..]`: 値のいずれかが候補のいずれかに等しい（値なしは偽）
/// - `contains` / `starts_with` / `ends_with`: いずれかの要素（文字列）が満たす
/// - `exists`: 値が 1 個以上（空配列は偽）・`is_null`: `not(exists)`
/// - `lt` / `lte` / `gt` / `gte`: 単値だけ（多値は data では使えない・ここでは偽）。値なしは偽、
///   数値と文字列のように型が合わなければ [`EXPR_TYPE_ERROR`]（表は型の合う組だけを定める）
///
/// 2026-10 に data 側へ揃えた（それ以前は配列全体との一致・`in` は左辺が配列だと常に偽・
/// 空配列の `exists` は真・`1 ≠ 1.0`）。保存済み定義の影響は `ir::validate::semantics` が検出する。
fn eval_cmp(cmp: &Comparison, r: &dyn ValueResolver) -> Result<bool, EvalError> {
    let left = resolve_operand(&cmp.left, r)?;
    let right = match &cmp.right {
        Some(e) => resolve_operand(e, r)?,
        None => None,
    };
    let vals = value_set(left.as_ref());
    let out = match cmp.op {
        CmpOp::Exists => !vals.is_empty(),
        CmpOp::IsNull => vals.is_empty(),
        CmpOp::Eq => right
            .as_ref()
            .is_some_and(|c| vals.iter().any(|v| same(v, c))),
        CmpOp::Neq => !right
            .as_ref()
            .is_some_and(|c| vals.iter().any(|v| same(v, c))),
        CmpOp::Lt | CmpOp::Lte | CmpOp::Gt | CmpOp::Gte => match left.as_ref() {
            Some(Value::Array(_)) => false,
            l => compare_ordered(l, right.as_ref(), cmp.op)?,
        },
        CmpOp::In => match right {
            Some(Value::Array(cands)) => vals.iter().any(|v| cands.iter().any(|c| same(v, c))),
            _ => false,
        },
        CmpOp::Contains => any_str(&vals, right.as_ref(), |s, p| s.contains(p)),
        CmpOp::StartsWith => any_str(&vals, right.as_ref(), |s, p| s.starts_with(p)),
        CmpOp::EndsWith => any_str(&vals, right.as_ref(), |s, p| s.ends_with(p)),
        CmpOp::Matches => match (left, right) {
            (Some(Value::String(s)), Some(Value::String(pat))) => {
                regex::Regex::new(&pat).is_ok_and(|re| re.is_match(&s))
            }
            _ => false,
        },
    };
    Ok(out)
}

/// いずれかの要素（文字列）が右辺の文字列について `f` を満たすか。
fn any_str(vals: &[&Value], right: Option<&Value>, f: impl Fn(&str, &str) -> bool) -> bool {
    let Some(Value::String(p)) = right else {
        return false;
    };
    vals.iter().any(|v| v.as_str().is_some_and(|s| f(s, p)))
}

/// 数値どうしは数値、文字列どうしは文字列で比較する（値なし・null は偽）。
///
/// それ以外の組（数値と文字列・真偽値・オブジェクトなど）は黙って偽にせず [`EXPR_TYPE_ERROR`]
/// （固定値の「100」と数値の 100 を比べて分岐が黙って逆に流れるのを防ぐ・ir.md §3.3）。
fn compare_ordered(
    left: Option<&Value>,
    right: Option<&Value>,
    op: CmpOp,
) -> Result<bool, EvalError> {
    let (Some(l), Some(r)) = (left, right) else {
        return Ok(false);
    };
    let ord = match (l, r) {
        (Value::Null, _) | (_, Value::Null) => return Ok(false),
        (Value::Number(a), Value::Number(b)) => {
            if let (Some(a), Some(b)) = (a.as_i64(), b.as_i64()) {
                // 整数同士は f64 変換で精度を落とさず i64 で比較する（2^53 超の ID/カウンタを正しく扱う）。
                Some(a.cmp(&b))
            } else if let (Some(a), Some(b)) = (a.as_u64(), b.as_u64()) {
                Some(a.cmp(&b))
            } else {
                a.as_f64()
                    .zip(b.as_f64())
                    .and_then(|(a, b)| a.partial_cmp(&b))
            }
        }
        (Value::String(a), Value::String(b)) => Some(a.cmp(b)),
        _ => {
            let op = serde_json::to_value(op)
                .ok()
                .and_then(|v| v.as_str().map(str::to_string))
                .unwrap_or_default();
            return Err(EvalError::type_mismatch(format!(
                "{op} で比べられない組み合わせです（左: {} / 右: {}）。大小比較は数値どうし・文字列どうしでだけできます",
                type_name(l),
                type_name(r)
            )));
        }
    };
    Ok(match (ord, op) {
        (Some(o), CmpOp::Lt) => o.is_lt(),
        (Some(o), CmpOp::Lte) => o.is_le(),
        (Some(o), CmpOp::Gt) => o.is_gt(),
        (Some(o), CmpOp::Gte) => o.is_ge(),
        _ => false,
    })
}

/// エラー文言用の型名（値そのものは出さない）。
fn type_name(v: &Value) -> &'static str {
    match v {
        Value::Null => "null",
        Value::Bool(_) => "真偽値",
        Value::Number(_) => "数値",
        Value::String(_) => "文字列",
        Value::Array(_) => "配列",
        Value::Object(_) => "オブジェクト",
    }
}

#[cfg(test)]
#[path = "eval_tests.rs"]
mod tests;
