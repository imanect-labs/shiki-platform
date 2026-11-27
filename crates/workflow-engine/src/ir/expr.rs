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
/// regex パターン長上限（V5・文字数で数える・BUG-228。入力項目と出力の形の pattern の長さも同じ数）。
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
    // 書いた `"default": null` は null の既定値として残す（`Option` の既定の読み方では null が「書いていない」になり、
    // 無い値を null で埋められなかった・保存し直すと消えた）。項目が無いときだけ `None`。
    #[serde(
        default,
        deserialize_with = "written_default",
        skip_serializing_if = "Option::is_none"
    )]
    #[ts(type = "unknown")]
    pub default: Option<serde_json::Value>,
}

/// `default` の項目を書いてあれば、null も値として受ける。
fn written_default<'de, D: serde::Deserializer<'de>>(
    d: D,
) -> Result<Option<serde_json::Value>, D::Error> {
    serde_json::Value::deserialize(d).map(Some)
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
///
/// 直列化は untagged。**読み込みは手書き**（[`Deserialize`] の実装を参照）: serde の構造体は配列からも
/// 位置引数で読めるため、untagged の導出だと `["x", "y"]` が `$from: "x", path: "y"` の参照に化け、
/// `in` の候補リストをリテラルで書けなかった（Task 15.7 で修正）。
#[derive(Debug, Clone, PartialEq, Serialize, TS)]
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

impl<'de> Deserialize<'de> for ValueExpr {
    /// `$from` / `$template` キーを持つ**オブジェクト**だけを値式として読み、それ以外（配列を含む）は
    /// リテラル。値式の形が崩れたオブジェクトは従来どおりリテラルとして読み、保存時検証 V5 が
    /// 「不正な $from 式」として弾く（生 JSON を走査する）。
    fn deserialize<D: serde::Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        let v = serde_json::Value::deserialize(d)?;
        if let serde_json::Value::Object(m) = &v {
            if m.contains_key("$from") {
                if let Ok(f) = serde_json::from_value::<FromRef>(v.clone()) {
                    return Ok(ValueExpr::From(f));
                }
            } else if m.contains_key("$template") {
                if let Ok(t) = serde_json::from_value::<TemplateExpr>(v.clone()) {
                    return Ok(ValueExpr::Template(t));
                }
            }
        }
        Ok(ValueExpr::Literal(v))
    }
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

/// `$template` の字句（ir.md §3.2）。実行時の組み立て（`control::eval`）と保存時検証 V5 が
/// **同じ [`template_pieces`] で読む**（検証と実行で `{` / `}` の解釈がずれないように）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TemplatePiece<'a> {
    /// そのまま出す文字列。
    Text(&'a str),
    /// エスケープ `{{` / `}}` が表す波括弧 1 文字。
    Escaped(char),
    /// `{name}` の差し込み（name は vars のキー）。
    Var(&'a str),
}

/// テンプレート文字列を字句に分ける。
///
/// エスケープ `{{`→`{`・`}}`→`}` をプレースホルダより先に読む。`{` から次の `}` までが差し込みの名前で、
/// 閉じが無い `{` と対にならない `}` は文字のまま出す。
#[must_use]
pub fn template_pieces(template: &str) -> Vec<TemplatePiece<'_>> {
    // `{` `}` は ASCII なので、バイト位置で切っても UTF-8 の文字の途中にはならない。
    let bytes = template.as_bytes();
    let mut out = Vec::new();
    let mut text_start = 0;
    let mut i = 0;
    fn flush<'a>(out: &mut Vec<TemplatePiece<'a>>, template: &'a str, from: usize, to: usize) {
        if from < to {
            out.push(TemplatePiece::Text(&template[from..to]));
        }
    }
    while i < bytes.len() {
        match bytes[i] {
            b @ (b'{' | b'}') if bytes.get(i + 1) == Some(&b) => {
                flush(&mut out, template, text_start, i);
                out.push(TemplatePiece::Escaped(char::from(b)));
                i += 2;
                text_start = i;
            }
            b'{' => {
                if let Some(close) = template[i + 1..].find('}') {
                    flush(&mut out, template, text_start, i);
                    out.push(TemplatePiece::Var(&template[i + 1..i + 1 + close]));
                    i += close + 2;
                    text_start = i;
                } else {
                    i += 1;
                }
            }
            _ => i += 1,
        }
    }
    flush(&mut out, template, text_start, bytes.len());
    out
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
    #[ts(optional)]
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

    /// 書いた `"default": null` は null の既定値として読み、保存し直しても残る。書いていなければ既定値なし。
    #[test]
    fn from_ref_keeps_a_null_default() {
        let f: FromRef =
            serde_json::from_value(json!({"$from": "input", "path": "/due", "default": null}))
                .unwrap();
        assert_eq!(f.default, Some(serde_json::Value::Null));
        assert_eq!(
            serde_json::to_value(&f).unwrap(),
            json!({"$from": "input", "path": "/due", "default": null})
        );
        let f: FromRef = serde_json::from_value(json!({"$from": "input", "path": "/due"})).unwrap();
        assert_eq!(f.default, None);
        assert_eq!(
            serde_json::to_value(&f).unwrap(),
            json!({"$from": "input", "path": "/due"})
        );
        let v: ValueExpr =
            serde_json::from_value(json!({"$from": "input", "default": null})).unwrap();
        assert_eq!(
            v.as_from().and_then(|f| f.default.clone()),
            Some(serde_json::Value::Null)
        );
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
        // 配列は位置引数の構造体として読まない（`in` の候補リスト・Task 15.7）。
        let a: ValueExpr = serde_json::from_value(json!(["x", "y"])).unwrap();
        assert_eq!(a, ValueExpr::Literal(json!(["x", "y"])));
        let a: ValueExpr = serde_json::from_value(json!(["hi {n}", {}])).unwrap();
        assert!(matches!(a, ValueExpr::Literal(_)));
        // 形の崩れた値式はリテラル（V5 が生 JSON から弾く）。
        let bad: ValueExpr = serde_json::from_value(json!({"$from": "input", "typo": 1})).unwrap();
        assert!(matches!(bad, ValueExpr::Literal(_)));
    }

    #[test]
    fn template_pieces_read_escapes_before_placeholders() {
        use TemplatePiece::{Escaped, Text, Var};
        assert_eq!(
            template_pieces("件名: {title} / {{literal}} {{{who}}}"),
            vec![
                Text("件名: "),
                Var("title"),
                Text(" / "),
                Escaped('{'),
                Text("literal"),
                Escaped('}'),
                Text(" "),
                Escaped('{'),
                Var("who"),
                Escaped('}'),
            ]
        );
        // 閉じの無い `{`・対にならない `}` は文字のまま。
        assert_eq!(
            template_pieces("a { b } c"),
            vec![Text("a "), Var(" b "), Text(" c")]
        );
        assert_eq!(template_pieces("a { b"), vec![Text("a { b")]);
        assert_eq!(template_pieces("x}y"), vec![Text("x}y")]);
        assert_eq!(template_pieces(""), vec![]);
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
