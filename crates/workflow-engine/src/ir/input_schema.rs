//! 対話トリガの入力の検証（`input_schema`・ir.md §2 の JSON Schema のサブセット）。
//!
//! 対話起動（API・会話の図のボタン・ミニアプリ）は、全経路が run を作る**前に**ここを通る。
//! 画面の検証は信用しない（送られる値は利用者がいくらでも書き換えられる）。
//!
//! 扱うキーワード: `type`・`properties`・`required`・`additionalProperties`・`items`・`enum`・
//! `const`・`minimum`・`maximum`・`exclusiveMinimum`・`exclusiveMaximum`・`minLength`・
//! `maxLength`・`pattern`・`minItems`・`maxItems`。`format` は `date`（`YYYY-MM-DD` の実在する日付）だけを
//! 検査し、ほかの値は注記として読み飛ばす（画面の日付の欄が送る形を、API や会話の図からの起動でも守らせる・
//! BUG-35）。注記（`title`・`description`・`default`・`examples`・`$schema`・`$comment`・`x-order`）は
//! 読み飛ばす。それ以外（`$ref`・`oneOf` など）は検証できないので**起動しない**（黙って緩く通さない・fail-closed）。
//!
//! `x-order` は画面に並べる項目のキーの順（D-60・BUG-34）。IR は jsonb の列に保存されるので、`properties` の
//! キーの順は保存して読み直すと変わる。形の誤りは起動ではなく保存で止める（[`order_errors`]）。
//!
//! 起動のときに確かめられない形（使えないキーワード・型の名前の誤り・読めない pattern など）は、保存のときにも
//! 止める（[`schema_errors`]・GAP-35）。起動の検査は値が通る位置のキーワードしか見ないので、保存で全体を見ないと、
//! 値が入ったときに初めて起動できなくなる。

use std::collections::{HashMap, HashSet};

use serde_json::{Map, Value};

use super::patterns::compile_pattern;

mod check;
pub use check::schema_errors;

/// 返す誤りの上限（巨大な入力で応答を膨らませない）。
const MAX_ERRORS: usize = 20;
/// 入れ子の上限（スキーマ・入力ともに）。
const MAX_DEPTH: usize = 32;

/// 値を縛るキーワード（`output_schema` の保存時検査も同じ集合を使う・[`super::output_schema`]）。
pub(crate) const ASSERTIONS: &[&str] = &[
    "type",
    "properties",
    "required",
    "additionalProperties",
    "items",
    "enum",
    "const",
    "minimum",
    "maximum",
    "exclusiveMinimum",
    "exclusiveMaximum",
    "minLength",
    "maxLength",
    "pattern",
    "minItems",
    "maxItems",
];
/// JSON Schema の型の名前（`type` に書ける値・`output_schema` も同じ）。
pub(crate) const TYPES: &[&str] = &[
    "string", "number", "integer", "boolean", "object", "array", "null",
];
/// 数で書くキーワード（範囲・文字数・個数）。
const NUMBER_KEYWORDS: &[&str] = &[
    "minimum",
    "maximum",
    "exclusiveMinimum",
    "exclusiveMaximum",
    "minLength",
    "maxLength",
    "minItems",
    "maxItems",
];
/// 注記（検証では読み飛ばす）。
pub(crate) const ANNOTATIONS: &[&str] = &[
    "title",
    "description",
    "default",
    "examples",
    "format",
    "$schema",
    "$comment",
    ORDER,
];

/// 項目の並びの注記（`properties` のキーの配列・D-60）。
pub const ORDER: &str = "x-order";

/// `properties` の項目を画面に並べる順で返す（D-60）。`x-order` に書かれたキーをその順に先に置き（properties に無い
/// キー・重なりは飛ばす）、書かれていないキーをその後ろに今の順で置く。properties が無ければ空。
///
/// web の入力項目の表と実行のダイアログ（`orderedProperties`）と同じ決まり。ミニアプリの部品の候補（入力の手がかり）
/// はサーバーがこの順で返す。
#[must_use]
pub fn ordered_properties(schema: &Value) -> Vec<(&str, &Value)> {
    let Some(props) = schema.get("properties").and_then(Value::as_object) else {
        return Vec::new();
    };
    let mut out: Vec<(&str, &Value)> = Vec::with_capacity(props.len());
    for key in schema
        .get(ORDER)
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(Value::as_str)
    {
        if let Some((k, v)) = props.get_key_value(key) {
            if !out.iter().any(|(placed, _)| *placed == key) {
                out.push((k.as_str(), v));
            }
        }
    }
    let rest: Vec<(&str, &Value)> = props
        .iter()
        .filter(|(k, _)| !out.iter().any(|(placed, _)| *placed == k.as_str()))
        .map(|(k, v)| (k.as_str(), v))
        .collect();
    out.extend(rest);
    out
}

/// `x-order` の形の誤り（保存時検証が `ir.schema_violation` にする）。スキーマの全階層を見る。
///
/// 返すのは（スキーマの中の位置の JSON Pointer, 文言）の一覧。`x-order` は文字列の配列で、同じオブジェクトの
/// `properties` のキーだけを重ねずに書く（全部のキーを書かなくてよい。書かれていないキーは、画面がその後ろに
/// 今の順で並べる）。起動の検証は注記として読み飛ばすので、形を守らせるのはここだけ。
#[must_use]
pub fn order_errors(schema: &Value) -> Vec<(String, String)> {
    let mut out = Vec::new();
    walk_order(schema, "", 0, &mut out);
    out
}

fn walk_order(schema: &Value, ptr: &str, depth: usize, out: &mut Vec<(String, String)>) {
    let Value::Object(s) = schema else {
        return;
    };
    if depth > MAX_DEPTH || out.len() >= MAX_ERRORS {
        return;
    }
    let props = s.get("properties").and_then(Value::as_object);
    if let Some(order) = s.get(ORDER) {
        if let Some((at, message)) = order_error(order, props) {
            out.push((format!("{ptr}/{ORDER}{at}"), message));
        }
    }
    for (k, sub) in props.into_iter().flatten() {
        walk_order(
            sub,
            &format!("{ptr}/properties/{}", pointer_token(k)),
            depth + 1,
            out,
        );
    }
    for key in ["items", "additionalProperties"] {
        if let Some(sub) = s.get(key) {
            walk_order(sub, &format!("{ptr}/{key}"), depth + 1, out);
        }
    }
}

/// 1 つの `x-order` の誤り（`x-order` の中の位置, 文言）。最初の 1 件だけ返す。
fn order_error(order: &Value, props: Option<&Map<String, Value>>) -> Option<(String, String)> {
    let Value::Array(keys) = order else {
        return Some((
            String::new(),
            "x-order は項目のキー（文字列）の配列で書いてください".into(),
        ));
    };
    let mut seen = HashSet::new();
    for (i, key) in keys.iter().enumerate() {
        let message = match key.as_str() {
            None => "x-order には項目のキー（文字列）だけを書いてください".to_string(),
            Some(k) if !props.is_some_and(|p| p.contains_key(k)) => {
                format!("x-order の「{k}」は properties にありません")
            }
            Some(k) if !seen.insert(k) => format!("x-order の「{k}」が重なっています"),
            Some(_) => continue,
        };
        return Some((format!("/{i}"), message));
    }
    None
}

/// JSON Pointer の 1 段（RFC 6901 の `~0`・`~1`）。
fn pointer_token(key: &str) -> String {
    key.replace('~', "~0").replace('/', "~1")
}

/// 入力の誤り 1 件（場所と理由・BUG-109）。
///
/// 実行のダイアログが理由を欄の下に出せるよう、場所と理由を分けて持つ（「場所: 理由」の文を読み直させない）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InputIssue {
    /// 誤りの場所。入力の項目のキーで、入れ子は `addr.zip`、リストの要素は `tags[2]`。値全体の誤りは空。
    pub key: String,
    /// 理由（利用者に見せてよい日本語・例: `1 以上にしてください`）。
    pub message: String,
}

impl InputIssue {
    /// 「場所: 理由」の文（場所が空なら根の呼び名・「入力」「出力」など）。
    #[must_use]
    pub fn describe(&self, root: &str) -> String {
        let at = if self.key.is_empty() { root } else { &self.key };
        format!("{at}: {}", self.message)
    }
}

/// 入力をスキーマで検証する。誤りは「場所: 理由」の文（日本語）の一覧。
pub fn validate_input(schema: &Value, input: &Value) -> Result<(), Vec<String>> {
    validate_value(schema, input, "入力")
}

/// 入力をスキーマで検証する（誤りを場所と理由に分けて返す・実行のダイアログ向け・BUG-109）。
pub fn validate_input_issues(schema: &Value, input: &Value) -> Result<(), Vec<InputIssue>> {
    let mut errors = Vec::new();
    Checker::new("入力").check(schema, input, "", 0, &mut errors);
    if errors.is_empty() {
        Ok(())
    } else {
        Err(errors)
    }
}

/// 値をスキーマで検証する（`root` は根の呼び名・「入力」「出力」など）。誤りは「場所: 理由」の文の一覧。
///
/// 対話起動の入力（[`validate_input`]）と llm.invoke の出力（[`super::output_schema::validate_output`]）が
/// 同じ検査器を使う（キーワードの意味を 2 つに分けない）。
pub fn validate_value(schema: &Value, v: &Value, root: &str) -> Result<(), Vec<String>> {
    let mut errors = Vec::new();
    Checker::new(root).check(schema, v, "", 0, &mut errors);
    if errors.is_empty() {
        Ok(())
    } else {
        Err(errors.iter().map(|e| e.describe(root)).collect())
    }
}

/// 誤りを積む（場所と理由）。
fn issue(errs: &mut Vec<InputIssue>, key: &str, message: String) {
    errs.push(InputIssue {
        key: key.to_string(),
        message,
    });
}

/// 検査の文脈（文言の根の呼び名と、1 回の確かめの中でコンパイルした pattern）。
struct Checker<'a> {
    root: &'a str,
    /// pattern の文字列からコンパイルの結果へ（読めなければ `None`）。同じ pattern は 1 回の確かめで一度だけ
    /// コンパイルする（値ごと・配列の要素ごとにコンパイルし直さない・BUG-227）。
    patterns: HashMap<&'a str, Option<regex::Regex>>,
}

impl<'a> Checker<'a> {
    fn new(root: &'a str) -> Self {
        Checker {
            root,
            patterns: HashMap::new(),
        }
    }

    /// `p` をコンパイルした正規表現（読めなければ `None`）。この確かめの中で初めて見た pattern だけをコンパイルする。
    fn pattern(&mut self, p: &'a str) -> Option<&regex::Regex> {
        self.patterns
            .entry(p)
            .or_insert_with(|| compile_pattern(p))
            .as_ref()
    }

    fn check(
        &mut self,
        schema: &'a Value,
        v: &Value,
        path: &str,
        depth: usize,
        errs: &mut Vec<InputIssue>,
    ) {
        if errs.len() >= MAX_ERRORS {
            return;
        }
        let s = match schema {
            Value::Bool(true) => return,
            Value::Bool(false) => {
                issue(errs, path, "この値は受け付けません".into());
                return;
            }
            Value::Object(s) => s,
            _ => {
                issue(errs, path, format!("{}スキーマが不正です", self.root));
                return;
            }
        };
        if depth > MAX_DEPTH {
            issue(errs, path, "入れ子が深すぎます".into());
            return;
        }
        if let Some(k) = s
            .keys()
            .find(|k| !ASSERTIONS.contains(&k.as_str()) && !ANNOTATIONS.contains(&k.as_str()))
        {
            issue(
                errs,
                path,
                format!("{}スキーマの「{k}」には対応していません", self.root),
            );
            return;
        }
        if let Some(t) = s.get("type") {
            let types: Vec<&str> = match t {
                Value::String(x) => vec![x.as_str()],
                Value::Array(a) => a.iter().filter_map(Value::as_str).collect(),
                _ => Vec::new(),
            };
            if types.is_empty() {
                issue(
                    errs,
                    path,
                    format!("{}スキーマの type が不正です", self.root),
                );
                return;
            }
            if !types.iter().any(|t| type_matches(t, v)) {
                let names: Vec<&str> = types.iter().map(|t| type_name(t)).collect();
                issue(errs, path, format!("{}を入れてください", names.join("か")));
                return;
            }
        }
        if let Some(e) = s.get("enum") {
            match e {
                Value::Array(opts) if opts.iter().any(|o| json_eq(o, v)) => {}
                Value::Array(_) => issue(errs, path, "選択肢にない値です".into()),
                _ => issue(
                    errs,
                    path,
                    format!("{}スキーマの enum が不正です", self.root),
                ),
            }
        }
        if let Some(c) = s.get("const") {
            if !json_eq(c, v) {
                issue(errs, path, "決められた値ではありません".into());
            }
        }
        match v {
            Value::Number(n) => self.check_number(s, n.as_f64().unwrap_or(f64::NAN), path, errs),
            Value::String(text) => self.check_string(s, text, path, errs),
            Value::Array(items) => self.check_array(s, items, path, depth, errs),
            Value::Object(map) => self.check_object(s, map, path, depth, errs),
            Value::Bool(_) | Value::Null => {}
        }
    }

    fn num(
        &self,
        s: &Map<String, Value>,
        key: &str,
        path: &str,
        errs: &mut Vec<InputIssue>,
    ) -> Option<f64> {
        let v = s.get(key)?;
        let n = v.as_f64();
        if n.is_none() {
            issue(
                errs,
                path,
                format!("{}スキーマの {key} が不正です", self.root),
            );
        }
        n
    }

    fn check_number(&self, s: &Map<String, Value>, x: f64, path: &str, errs: &mut Vec<InputIssue>) {
        if let Some(m) = self.num(s, "minimum", path, errs) {
            if x < m {
                issue(errs, path, format!("{m} 以上にしてください"));
            }
        }
        if let Some(m) = self.num(s, "maximum", path, errs) {
            if x > m {
                issue(errs, path, format!("{m} 以下にしてください"));
            }
        }
        if let Some(m) = self.num(s, "exclusiveMinimum", path, errs) {
            if x <= m {
                issue(errs, path, format!("{m} より大きくしてください"));
            }
        }
        if let Some(m) = self.num(s, "exclusiveMaximum", path, errs) {
            if x >= m {
                issue(errs, path, format!("{m} より小さくしてください"));
            }
        }
    }

    fn check_string(
        &mut self,
        s: &'a Map<String, Value>,
        text: &str,
        path: &str,
        errs: &mut Vec<InputIssue>,
    ) {
        // JSON Schema の文字数はコードポイントで数える。
        let len = count_f64(text.chars().count());
        if let Some(m) = self.num(s, "minLength", path, errs) {
            if len < m {
                issue(errs, path, format!("{m} 文字以上にしてください"));
            }
        }
        if let Some(m) = self.num(s, "maxLength", path, errs) {
            if len > m {
                issue(errs, path, format!("{m} 文字までです"));
            }
        }
        if let Some(p) = s.get("pattern") {
            // ワークフローの作者が書いたパターン。regex crate は線形時間なので入力で暴走しない。
            let matched = p
                .as_str()
                .and_then(|p| self.pattern(p))
                .map(|re| re.is_match(text));
            match matched {
                Some(true) => {}
                Some(false) => issue(errs, path, "形式が違います".into()),
                None => issue(
                    errs,
                    path,
                    format!("{}スキーマの pattern が不正です", self.root),
                ),
            }
        }
        if s.get("format").and_then(Value::as_str) == Some("date") && !is_full_date(text) {
            issue(errs, path, "日付（例: 2026-04-01）を入れてください".into());
        }
    }

    fn check_array(
        &mut self,
        s: &'a Map<String, Value>,
        items: &[Value],
        path: &str,
        depth: usize,
        errs: &mut Vec<InputIssue>,
    ) {
        let n = count_f64(items.len());
        if let Some(m) = self.num(s, "minItems", path, errs) {
            if n < m {
                issue(errs, path, format!("{m} 個以上選んでください"));
            }
        }
        if let Some(m) = self.num(s, "maxItems", path, errs) {
            if n > m {
                issue(errs, path, format!("{m} 個までです"));
            }
        }
        if let Some(sub) = s.get("items") {
            for (i, item) in items.iter().enumerate() {
                self.check(
                    sub,
                    item,
                    // 根がリストのときは根の呼び名に番号を付ける（入力・出力の根はふつう項目のまとまり）。
                    &format!("{}[{i}]", if path.is_empty() { self.root } else { path }),
                    depth + 1,
                    errs,
                );
            }
        }
    }

    fn check_object(
        &mut self,
        s: &'a Map<String, Value>,
        map: &Map<String, Value>,
        path: &str,
        depth: usize,
        errs: &mut Vec<InputIssue>,
    ) {
        let child = |k: &str| {
            if path.is_empty() {
                k.to_string()
            } else {
                format!("{path}.{k}")
            }
        };
        match s.get("required") {
            None => {}
            // 名前でない要素があれば黙って捨てず、スキーマの誤りとして起動しない（fail-closed）。
            Some(Value::Array(req)) if req.iter().all(Value::is_string) => {
                for k in req.iter().filter_map(Value::as_str) {
                    if !map.contains_key(k) {
                        issue(errs, &child(k), "必須です".into());
                    }
                }
            }
            Some(_) => issue(
                errs,
                path,
                format!("{}スキーマの required が不正です", self.root),
            ),
        }
        let props = match s.get("properties") {
            None => None,
            Some(Value::Object(p)) => Some(p),
            Some(_) => {
                issue(
                    errs,
                    path,
                    format!("{}スキーマの properties が不正です", self.root),
                );
                return;
            }
        };
        for (k, v) in map {
            match props.and_then(|p| p.get(k)) {
                Some(sub) => self.check(sub, v, &child(k), depth + 1, errs),
                None => match s.get("additionalProperties") {
                    None | Some(Value::Bool(true)) => {}
                    Some(Value::Bool(false)) => {
                        issue(errs, &child(k), "受け付けない項目です".into());
                    }
                    Some(sub) => self.check(sub, v, &child(k), depth + 1, errs),
                },
            }
        }
    }
}

/// RFC 3339 の full-date（`YYYY-MM-DD`・桁は固定）で、暦に実在する日付か。
fn is_full_date(text: &str) -> bool {
    let b = text.as_bytes();
    b.len() == 10
        && b[4] == b'-'
        && b[7] == b'-'
        && b.iter()
            .enumerate()
            .all(|(i, c)| i == 4 || i == 7 || c.is_ascii_digit())
        && chrono::NaiveDate::parse_from_str(text, "%Y-%m-%d").is_ok()
}

/// 個数を数値の比較に使う（入力の大きさの上限があるので精度は落ちない）。
#[allow(clippy::cast_precision_loss)]
fn count_f64(n: usize) -> f64 {
    n as f64
}

fn type_matches(t: &str, v: &Value) -> bool {
    match t {
        "string" => v.is_string(),
        "number" => v.is_number(),
        "integer" => v
            .as_f64()
            .is_some_and(|x| x.is_finite() && x.fract() == 0.0),
        "boolean" => v.is_boolean(),
        "object" => v.is_object(),
        "array" => v.is_array(),
        "null" => v.is_null(),
        _ => false,
    }
}

/// 値の種類の呼び名（列の種類と同じ名前にそろえる・付録 C の §9-15・付録 D の N4.7）。
fn type_name(t: &str) -> &str {
    match t {
        "string" => "テキスト",
        "number" => "数値",
        "integer" => "整数",
        "boolean" => "はい／いいえ",
        "object" => "項目のまとまり",
        "array" => "リスト",
        "null" => "空の値",
        other => other,
    }
}

/// JSON の数の一致。整数どうしは整数のまま比べ（2^53 を超えても丸めない）、
/// 小数が混じるときだけ 1 と 1.0 を同じとみなす。
fn num_eq(x: &serde_json::Number, y: &serde_json::Number) -> bool {
    if let (Some(a), Some(b)) = (x.as_i64(), y.as_i64()) {
        return a == b;
    }
    if let (Some(a), Some(b)) = (x.as_u64(), y.as_u64()) {
        return a == b;
    }
    if x.is_f64() || y.is_f64() {
        // 整数側が f64 で正確に表せないほど大きいなら、小数とは一致しない。
        let exact = |n: &serde_json::Number| {
            n.is_f64()
                || n.as_i64().is_some_and(|v| v.unsigned_abs() <= 1 << 53)
                || n.as_u64().is_some_and(|v| v <= 1 << 53)
        };
        return exact(x) && exact(y) && x.as_f64() == y.as_f64();
    }
    // 片方が負の i64、もう片方が i64 に収まらない u64。
    false
}

/// JSON の値の一致（数は [`num_eq`]）。
fn json_eq(a: &Value, b: &Value) -> bool {
    match (a, b) {
        (Value::Number(x), Value::Number(y)) => num_eq(x, y),
        (Value::Array(x), Value::Array(y)) => {
            x.len() == y.len() && x.iter().zip(y).all(|(p, q)| json_eq(p, q))
        }
        (Value::Object(x), Value::Object(y)) => {
            x.len() == y.len()
                && x.iter()
                    .all(|(k, p)| y.get(k).is_some_and(|q| json_eq(p, q)))
        }
        _ => a == b,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    /// 並びは x-order のキーが先（無いキー・重なりは飛ばす）、残りは今の順（D-60）。
    #[test]
    fn properties_are_ordered_by_the_order_note() {
        let keys = |s: Value| -> Vec<String> {
            ordered_properties(&s)
                .into_iter()
                .map(|(k, _)| k.to_string())
                .collect()
        };
        let props = json!({ "a": {}, "b": {}, "c": {}, "d": {} });
        assert_eq!(
            keys(json!({ "properties": props, "x-order": ["c", "zz", "a", "c"] })),
            vec!["c", "a", "b", "d"]
        );
        assert_eq!(
            keys(json!({ "properties": props, "x-order": 5 })),
            vec!["a", "b", "c", "d"]
        );
        assert!(keys(json!({ "type": "object" })).is_empty());
    }

    fn schema() -> Value {
        json!({
            "type": "object",
            "title": "経費申請",
            "required": ["amount", "category"],
            "additionalProperties": false,
            "properties": {
                "amount": { "type": "integer", "minimum": 1, "maximum": 1_000_000 },
                "category": { "type": "string", "enum": ["交通費", "交際費"] },
                "memo": { "type": "string", "maxLength": 5, "description": "任意" },
                "code": { "type": "string", "pattern": "^[A-Z]{3}$" },
                "tags": { "type": "array", "items": { "type": "string" }, "maxItems": 2 },
                "urgent": { "type": "boolean" }
            }
        })
    }

    #[test]
    fn accepts_valid_input() {
        let input = json!({ "amount": 1200, "category": "交通費", "memo": "あいう", "tags": ["a"], "urgent": true, "code": "ABC" });
        assert_eq!(validate_input(&schema(), &input), Ok(()));
        // 1200.0 も整数として受ける
        assert!(validate_input(
            &schema(),
            &json!({ "amount": 1200.0, "category": "交際費" })
        )
        .is_ok());
    }

    #[test]
    fn reports_each_violation() {
        let input = json!({ "amount": 0, "memo": "あいうえおか", "tags": ["a", "b", 3], "extra": 1, "code": "abc" });
        let errs = validate_input(&schema(), &input).unwrap_err();
        let joined = errs.join("\n");
        assert!(joined.contains("category: 必須です"), "{joined}");
        assert!(joined.contains("amount: 1 以上"), "{joined}");
        assert!(joined.contains("memo: 5 文字まで"), "{joined}");
        assert!(joined.contains("tags: 2 個まで"), "{joined}");
        assert!(
            joined.contains("tags[2]: テキストを入れてください"),
            "{joined}"
        );
        assert!(joined.contains("extra: 受け付けない項目です"), "{joined}");
        assert!(joined.contains("code: 形式が違います"), "{joined}");
    }

    /// BUG-109: 誤りは場所（項目のキー）と理由に分けて返す（画面が欄の下に出す）。文の形は今と同じ。
    #[test]
    fn issues_split_the_key_from_the_reason() {
        let s = json!({ "type": "object", "required": ["amount"], "properties": {
            "amount": { "type": "integer", "minimum": 1 },
            "addr": { "type": "object", "properties": { "zip": { "type": "string" } } },
            "tags": { "type": "array", "items": { "type": "string" } } } });
        let issues =
            validate_input_issues(&s, &json!({ "addr": { "zip": 1 }, "tags": ["a", true] }))
                .unwrap_err();
        assert_eq!(
            issues,
            vec![
                InputIssue {
                    key: "amount".into(),
                    message: "必須です".into()
                },
                InputIssue {
                    key: "addr.zip".into(),
                    message: "テキストを入れてください".into()
                },
                InputIssue {
                    key: "tags[1]".into(),
                    message: "テキストを入れてください".into()
                },
            ]
        );
        // 値全体の誤りは場所が空で、文では根の呼び名になる。
        let root = validate_input_issues(&s, &json!([1])).unwrap_err();
        assert_eq!(root[0].key, "");
        assert_eq!(
            root[0].describe("入力"),
            "入力: 項目のまとまりを入れてください"
        );
        assert_eq!(
            validate_input(&s, &json!([1])).unwrap_err(),
            vec!["入力: 項目のまとまりを入れてください".to_string()]
        );
    }

    #[test]
    fn type_and_enum_mismatch() {
        let errs = validate_input(&schema(), &json!({ "amount": "1200", "category": "食費" }))
            .unwrap_err();
        assert!(errs.iter().any(|e| e == "amount: 整数を入れてください"));
        assert!(errs.iter().any(|e| e == "category: 選択肢にない値です"));
        assert!(validate_input(&schema(), &json!([1])).is_err());
        assert!(validate_input(&json!({ "type": ["string", "null"] }), &Value::Null).is_ok());
    }

    /// 検証できないキーワードは緩く通さず、起動しない。
    #[test]
    fn unsupported_keywords_fail_closed() {
        let s = json!({ "type": "object", "properties": { "a": { "$ref": "#/defs/x" } } });
        let errs = validate_input(&s, &json!({ "a": 1 })).unwrap_err();
        assert_eq!(
            errs,
            vec!["a: 入力スキーマの「$ref」には対応していません".to_string()]
        );
        assert!(validate_input(&json!({ "oneOf": [] }), &json!(1)).is_err());
        assert!(validate_input(&json!({ "pattern": "(" }), &json!("x")).is_err());
        assert!(validate_input(&json!(false), &json!(1)).is_err());
        assert!(validate_input(&json!(true), &json!(1)).is_ok());
    }

    #[test]
    fn exclusive_bounds_and_const() {
        let s = json!({ "type": "number", "exclusiveMinimum": 0, "exclusiveMaximum": 10 });
        assert!(validate_input(&s, &json!(0)).is_err());
        assert!(validate_input(&s, &json!(10)).is_err());
        assert!(validate_input(&s, &json!(9.5)).is_ok());
        assert!(validate_input(&json!({ "const": { "a": [1] } }), &json!({ "a": [1.0] })).is_ok());
        assert!(validate_input(&json!({ "const": "x" }), &json!("y")).is_err());
    }

    #[test]
    fn large_integers_are_not_rounded() {
        let s = json!({ "enum": [9_007_199_254_740_992_u64] });
        assert!(validate_input(&s, &json!(9_007_199_254_740_993_u64)).is_err());
        assert!(validate_input(&s, &json!(9_007_199_254_740_992_u64)).is_ok());
        assert!(validate_input(&json!({ "const": u64::MAX }), &json!(u64::MAX)).is_ok());
        assert!(validate_input(&json!({ "const": -1 }), &json!(u64::MAX)).is_err());
        assert!(validate_input(&json!({ "const": 2 }), &json!(2.0)).is_ok());
    }

    /// BUG-35: `format: date` は YYYY-MM-DD の実在する日付だけを受ける。ほかの format は注記のまま。
    #[test]
    fn date_format_is_checked() {
        let s = json!({ "type": "object", "properties": {
            "due": { "type": "string", "format": "date" },
            "at": { "type": "string", "format": "date-time" } } });
        assert!(validate_input(&s, &json!({ "due": "2026-04-01" })).is_ok());
        assert!(validate_input(&s, &json!({ "due": "2028-02-29" })).is_ok());
        for bad in [
            "2026-4-1",
            "2026/04/01",
            "2026-02-30",
            "２０２６-04-01",
            "2026-04-01T09:00:00Z",
            "",
        ] {
            let errs = validate_input(&s, &json!({ "due": bad })).unwrap_err();
            assert_eq!(
                errs,
                vec!["due: 日付（例: 2026-04-01）を入れてください".to_string()],
                "{bad}"
            );
        }
        // date-time などは検査しない（注記）。文字でない値は type の誤りだけを返す。
        assert!(validate_input(&s, &json!({ "at": "あした" })).is_ok());
        assert!(
            validate_input(&json!({ "format": "date" }), &json!(20_260_401)).is_ok(),
            "type の無いスキーマの数値は format の対象外"
        );
    }

    /// 名前でない required の要素は黙って捨てず、起動しない。
    #[test]
    fn malformed_required_fails_closed() {
        let s = json!({ "type": "object", "required": [123] });
        let errs = validate_input(&s, &json!({})).unwrap_err();
        assert_eq!(
            errs,
            vec!["入力: 入力スキーマの required が不正です".to_string()]
        );
    }

    /// D-60: `x-order` は起動の検証では読み飛ばす注記（入力の値には関わらない）。
    #[test]
    fn order_is_an_annotation_at_launch() {
        let s = json!({ "type": "object", "x-order": ["title", "amount"], "properties": {
            "amount": { "type": "integer" }, "title": { "type": "string" } } });
        assert!(validate_input(&s, &json!({ "title": "出張", "amount": 1200 })).is_ok());
        assert!(order_errors(&s).is_empty());
        // 一部のキーだけ・空の配列も書ける（書かれていないキーは画面がその後ろに並べる）。
        assert!(
            order_errors(&json!({ "x-order": ["b"], "properties": { "a": {}, "b": {} } }))
                .is_empty()
        );
        assert!(order_errors(&json!({ "type": "object", "x-order": [] })).is_empty());
    }

    /// D-60: `x-order` の形の誤り（文字列の配列でない・properties に無いキー・重なり）を、位置つきで返す。
    #[test]
    fn malformed_order_is_reported_with_its_position() {
        let props = json!({ "a": { "type": "string" }, "b": { "type": "string" } });
        let cases = [
            (
                json!("a"),
                "/x-order",
                "x-order は項目のキー（文字列）の配列で書いてください",
            ),
            (
                json!(["a", 1]),
                "/x-order/1",
                "x-order には項目のキー（文字列）だけを書いてください",
            ),
            (
                json!(["a", "c"]),
                "/x-order/1",
                "x-order の「c」は properties にありません",
            ),
            (
                json!(["b", "a", "b"]),
                "/x-order/2",
                "x-order の「b」が重なっています",
            ),
        ];
        for (order, at, message) in cases {
            let s = json!({ "type": "object", "properties": props, "x-order": order });
            assert_eq!(
                order_errors(&s),
                vec![(at.to_string(), message.to_string())],
                "{order}"
            );
        }
        // properties の無いスキーマのキーは、どれも properties にない。
        assert_eq!(
            order_errors(&json!({ "type": "object", "x-order": ["a"] })),
            vec![(
                "/x-order/0".to_string(),
                "x-order の「a」は properties にありません".to_string()
            )]
        );
    }

    /// 入れ子のオブジェクト（properties・items・additionalProperties の中）の x-order も確かめる。
    #[test]
    fn nested_order_is_checked() {
        let s = json!({ "type": "object", "x-order": ["addr", "a/b"], "properties": {
            "addr": { "type": "object", "x-order": ["zip", "city"],
                      "properties": { "zip": { "type": "string" } } },
            "a/b": { "type": "object", "x-order": ["x"] },
            "list": { "type": "array", "items": { "type": "object", "x-order": [1] } }
        }, "additionalProperties": { "type": "object", "x-order": "x" } });
        let mut errs = order_errors(&s);
        errs.sort();
        assert_eq!(
            errs,
            vec![
                (
                    "/additionalProperties/x-order".to_string(),
                    "x-order は項目のキー（文字列）の配列で書いてください".to_string()
                ),
                (
                    "/properties/addr/x-order/1".to_string(),
                    "x-order の「city」は properties にありません".to_string()
                ),
                (
                    "/properties/a~1b/x-order/0".to_string(),
                    "x-order の「x」は properties にありません".to_string()
                ),
                (
                    "/properties/list/items/x-order/0".to_string(),
                    "x-order には項目のキー（文字列）だけを書いてください".to_string()
                ),
            ]
        );
    }

    /// BUG-227: 同じ pattern は 1 回の確かめの中で一度だけコンパイルする。配列の要素や値が増えても、コンパイルの回数は
    /// 増えない（起動の入力の確かめと、出力の確かめが使う validate_value の両方）。
    #[test]
    fn patterns_compile_once_per_check() {
        use crate::ir::patterns::compiled_count;

        let s = json!({ "type": "object", "properties": {
            "codes": { "type": "array", "items": { "type": "string", "pattern": "^[A-Z]{3}$" } },
            "same": { "type": "string", "pattern": "^[A-Z]{3}$" },
            "other": { "type": "string", "pattern": "^[a-z]+$" } } });
        let input = |n: usize| {
            let codes: Vec<Value> = (0..n)
                .map(|i| json!(if i == 0 { "abc" } else { "ABC" }))
                .collect();
            json!({ "codes": codes, "same": "ABC", "other": "x" })
        };
        let launch = |n: usize| {
            let before = compiled_count();
            let issues = validate_input_issues(&s, &input(n)).unwrap_err();
            assert_eq!(
                issues,
                vec![InputIssue {
                    key: "codes[0]".into(),
                    message: "形式が違います".into()
                }]
            );
            compiled_count() - before
        };
        assert_eq!(launch(1), 2);
        assert_eq!(launch(50_000), 2);
        let output = |n: usize| {
            let before = compiled_count();
            let errs = validate_value(&s, &input(n), "出力").unwrap_err();
            assert_eq!(errs, vec!["codes[0]: 形式が違います".to_string()]);
            compiled_count() - before
        };
        assert_eq!(output(50_000), 2);
    }

    #[test]
    fn error_count_is_bounded() {
        let mut props = Map::new();
        let mut input = Map::new();
        for i in 0..100 {
            props.insert(format!("k{i}"), json!({ "type": "string" }));
            input.insert(format!("k{i}"), json!(i));
        }
        let s = json!({ "type": "object", "properties": props });
        let errs = validate_input(&s, &Value::Object(input)).unwrap_err();
        assert_eq!(errs.len(), MAX_ERRORS);
    }
}
