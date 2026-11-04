//! llm.invoke の出力の形（`output_schema`・ir.md §7.3・GAP-13）: 保存時のスキーマの検査・オブジェクトを
//! 閉じる正規化・出力の検証。
//!
//! キーワードは `input_schema` と同じ集合（[`super::input_schema`]）で、値の検証も同じ検査器を使う
//! （[`super::input_schema::validate_value`]）。そのうえで、プロバイダの制約付きデコード（Anthropic の
//! `output_config.format`・OpenAI 互換の `response_format`）に載る形に絞る。
//!
//! - 根は `type: "object"`。どのスキーマも JSON のオブジェクトで書く（`true` / `false` は書けない）。
//! - オブジェクトは閉じる: `additionalProperties` は `false` か省略。省略は閉じたものとして扱い、
//!   [`close_objects`] で `false` を書き足してから、検証にもプロバイダへの送信にも使う（input_schema の
//!   「省略は開いている」と意味が違うので、必ず正規化を通す）。
//! - `type` の配列は `[T, "null"]` だけ（1 つの型と null の組）。
//! - プロパティ名は `^[A-Za-z_][A-Za-z0-9_]{0,63}$`。`required` の名前は `properties` にあり、重ならない。
//! - `pattern` はコンパイルできる。`enum` は空でなく、値は文字列・数値・真偽・null。
//! - 上限（shiki 側）: 入れ子の深さ [`MAX_DEPTH`]・プロパティ [`MAX_PROPERTIES`]・enum の値
//!   [`MAX_ENUM_VALUES`]・JSON [`MAX_SCHEMA_BYTES`]。
//! - 上限（Anthropic の schema complexity limits・1 回の要求の全スキーマの合計。llm.invoke は 1 回に
//!   スキーマを 1 つだけ送る）: 任意項目 [`MAX_OPTIONAL`]・null を許す項目（union）[`MAX_UNIONS`]。
//!
//! プロバイダが受けない制約（数値の範囲・文字列の長さ・pattern 等）は gateway が送る前に落とし、説明文に
//! 書き足す（`llm_gateway::structured::wire_schema`）。正しさは [`validate_output`] の事後検証で守る。

use serde_json::{Map, Value};

use super::input_schema::{order_errors, validate_value, ANNOTATIONS, ASSERTIONS, TYPES};

/// 入れ子の深さの上限（根から `properties` / `items` をたどる段数）。
pub const MAX_DEPTH: usize = 5;
/// プロパティの総数の上限（入れ子を含む）。
pub const MAX_PROPERTIES: usize = 100;
/// enum の値の総数の上限（入れ子を含む）。
pub const MAX_ENUM_VALUES: usize = 256;
/// スキーマの JSON の大きさの上限（バイト）。
pub const MAX_SCHEMA_BYTES: usize = 16 * 1024;
/// 任意項目（`required` に無いプロパティ・入れ子を含む）の上限（Anthropic の上限 24）。
pub const MAX_OPTIONAL: usize = 24;
/// null を許す項目（`type: [T, "null"]`）の上限（Anthropic の union 型の上限 16）。
pub const MAX_UNIONS: usize = 16;

/// 出力の検証の誤りの文で、根を指す呼び名。
const ROOT_LABEL: &str = "出力";

/// 保存時のスキーマの検査の誤り（スキーマの中の位置の JSON Pointer と文言）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SchemaIssue {
    /// スキーマの中の位置（根は空文字・`/properties/category/enum` など）。
    pub path: String,
    /// 利用者向けの文言（日本語）。
    pub message: String,
}

impl SchemaIssue {
    fn new(path: impl Into<String>, message: impl Into<String>) -> Self {
        SchemaIssue {
            path: path.into(),
            message: message.into(),
        }
    }
}

/// 数える量（上限の検査用）。
#[derive(Default)]
struct Counts {
    properties: usize,
    enum_values: usize,
    optional: usize,
    unions: usize,
}

/// 出力の形を保存時に検査する（最初の誤りを 1 件返す・V1 の cross-field 検査から呼ぶ）。
///
/// # Errors
/// 形・キーワード・上限のどれかに合わないとき。
pub fn check_output_schema(schema: &Value) -> Result<(), SchemaIssue> {
    let size = serde_json::to_vec(schema).map_or(usize::MAX, |b| b.len());
    if size > MAX_SCHEMA_BYTES {
        return Err(SchemaIssue::new(
            "",
            format!(
                "出力の形が大きすぎます（JSON で {} KiB まで）",
                MAX_SCHEMA_BYTES / 1024
            ),
        ));
    }
    let Some(root) = schema.as_object() else {
        return Err(SchemaIssue::new(
            "",
            "出力の形は JSON Schema のオブジェクトで書いてください",
        ));
    };
    // 使えないキーワード（$from・$ref など）は根の型より先に知らせる（直し方が分かる方を先に）。
    let mut counts = Counts::default();
    walk(schema, "", 0, &mut counts)?;
    if root.get("type").and_then(Value::as_str) != Some("object") {
        return Err(SchemaIssue::new(
            "/type",
            "出力の形の根は type: \"object\" にしてください",
        ));
    }
    if let Some((path, message)) = order_errors(schema).into_iter().next() {
        return Err(SchemaIssue::new(path, message));
    }
    let limits = [
        (counts.properties, MAX_PROPERTIES, "項目（properties）"),
        (
            counts.optional,
            MAX_OPTIONAL,
            "任意の項目（required に無い項目）",
        ),
        (
            counts.unions,
            MAX_UNIONS,
            "null を許す項目（type: [型, \"null\"]）",
        ),
    ];
    for (n, max, what) in limits {
        if n > max {
            return Err(SchemaIssue::new(
                "",
                format!("出力の形の{what}は全体で {max} 個までです（{n} 個あります）"),
            ));
        }
    }
    Ok(())
}

/// オブジェクトのスキーマを閉じる（`additionalProperties` を省いたものに `false` を書き足す）。
///
/// 検証にもプロバイダへの送信にも、正規化した形を使う（省略を「閉じている」と読む規則をここに閉じる）。
#[must_use]
pub fn close_objects(schema: &Value) -> Value {
    let mut out = schema.clone();
    close(&mut out, 0);
    out
}

/// 出力の本文（モデルが返した文字列）を読み、閉じた出力の形で検証する。
///
/// 厳密に JSON として読み、だめなら前後のコードフェンス（```json … ```）を剥がしてもう一度読む
/// （指示で形を伝えるモデルはフェンスを付けることがある）。返すのは検証済みの値。
///
/// # Errors
/// 読めない・形に合わないときは、場所と理由の文（日本語）の一覧。
pub fn validate_output(closed_schema: &Value, text: &str) -> Result<Value, Vec<String>> {
    let Some(data) = parse_output(text) else {
        return Err(vec![format!(
            "{ROOT_LABEL}: JSON として読めません（JSON のオブジェクトを 1 つだけ出してください）"
        )]);
    };
    validate_value(closed_schema, &data, ROOT_LABEL)?;
    Ok(data)
}

/// 本文を JSON として読む（厳密 → 前後のコードフェンスを剥がしたもの）。
fn parse_output(text: &str) -> Option<Value> {
    let t = text.trim();
    if let Ok(v) = serde_json::from_str(t) {
        return Some(v);
    }
    let rest = t.strip_prefix("```")?;
    // 開きのフェンスの行（言語名 json など）を飛ばす。
    let body = &rest[rest.find('\n')? + 1..];
    let end = body.trim_end().strip_suffix("```")?;
    serde_json::from_str(end.trim()).ok()
}

fn close(v: &mut Value, depth: usize) {
    let Value::Object(map) = v else {
        return;
    };
    // 保存時に深さを抑えてある（ここは暴走止め）。
    if depth > MAX_DEPTH + 1 {
        return;
    }
    if is_object_schema(map) && !map.contains_key("additionalProperties") {
        map.insert("additionalProperties".into(), Value::Bool(false));
    }
    if let Some(Value::Object(props)) = map.get_mut("properties") {
        for sub in props.values_mut() {
            close(sub, depth + 1);
        }
    }
    if let Some(items) = map.get_mut("items") {
        close(items, depth + 1);
    }
}

/// `type` の名前（文字列か配列）。
fn type_names(map: &Map<String, Value>) -> Vec<&str> {
    match map.get("type") {
        Some(Value::String(t)) => vec![t.as_str()],
        Some(Value::Array(ts)) => ts.iter().filter_map(Value::as_str).collect(),
        _ => Vec::new(),
    }
}

fn is_object_schema(map: &Map<String, Value>) -> bool {
    map.contains_key("properties") || type_names(map).contains(&"object")
}

/// JSON Pointer の 1 段（RFC 6901）。
fn token(key: &str) -> String {
    key.replace('~', "~0").replace('/', "~1")
}

fn walk(schema: &Value, ptr: &str, depth: usize, c: &mut Counts) -> Result<(), SchemaIssue> {
    let Some(s) = schema.as_object() else {
        return Err(SchemaIssue::new(
            ptr,
            "出力の形の各項目は JSON Schema のオブジェクトで書いてください（true / false は使えません）",
        ));
    };
    if depth > MAX_DEPTH {
        return Err(SchemaIssue::new(
            ptr,
            format!("出力の形の入れ子は {MAX_DEPTH} 段までです"),
        ));
    }
    if let Some(k) = s
        .keys()
        .find(|k| !ASSERTIONS.contains(&k.as_str()) && !ANNOTATIONS.contains(&k.as_str()))
    {
        return Err(SchemaIssue::new(
            format!("{ptr}/{}", token(k)),
            format!("出力の形に「{k}」は使えません"),
        ));
    }
    check_type(s, ptr, c)?;
    check_scalars(s, ptr, c)?;
    let types = type_names(s);
    let object = is_object_schema(s);
    for key in ["required", "additionalProperties"] {
        if s.contains_key(key) && !object {
            return Err(SchemaIssue::new(
                format!("{ptr}/{key}"),
                format!("{key} は type が object の項目にだけ書けます"),
            ));
        }
    }
    if object {
        check_object(s, ptr, depth, c)?;
    }
    if let Some(items) = s.get("items") {
        if !types.contains(&"array") {
            return Err(SchemaIssue::new(
                format!("{ptr}/items"),
                "items は type が array の項目にだけ書けます",
            ));
        }
        walk(items, &format!("{ptr}/items"), depth + 1, c)?;
    }
    Ok(())
}

/// `type` は 1 つの型か `[T, "null"]`。
fn check_type(s: &Map<String, Value>, ptr: &str, c: &mut Counts) -> Result<(), SchemaIssue> {
    let bad = || {
        SchemaIssue::new(
            format!("{ptr}/type"),
            "type は 1 つの型（string・number・integer・boolean・object・array・null）か、[型, \"null\"] で書いてください",
        )
    };
    match s.get("type") {
        None => Ok(()),
        Some(Value::String(t)) if TYPES.contains(&t.as_str()) => Ok(()),
        Some(Value::Array(ts)) => {
            let names: Vec<&str> = ts.iter().filter_map(Value::as_str).collect();
            let ok = names.len() == 2
                && ts.len() == 2
                && names.contains(&"null")
                && names.iter().any(|t| *t != "null" && TYPES.contains(t));
            if !ok {
                return Err(bad());
            }
            c.unions += 1;
            Ok(())
        }
        Some(_) => Err(bad()),
    }
}

/// 値の制約（enum・pattern・数の制約・注記の型）。
fn check_scalars(s: &Map<String, Value>, ptr: &str, c: &mut Counts) -> Result<(), SchemaIssue> {
    if let Some(e) = s.get("enum") {
        let at = format!("{ptr}/enum");
        let Some(values) = e.as_array().filter(|v| !v.is_empty()) else {
            return Err(SchemaIssue::new(
                at,
                "enum は 1 つ以上の値の配列で書いてください",
            ));
        };
        if values.iter().any(|v| v.is_array() || v.is_object()) {
            return Err(SchemaIssue::new(
                at,
                "enum の値は文字列・数値・真偽・null だけです",
            ));
        }
        c.enum_values += values.len();
        if c.enum_values > MAX_ENUM_VALUES {
            return Err(SchemaIssue::new(
                at,
                format!("出力の形の enum の値は全体で {MAX_ENUM_VALUES} 個までです"),
            ));
        }
    }
    if let Some(p) = s.get("pattern") {
        let compiled = p
            .as_str()
            .and_then(|p| regex::RegexBuilder::new(p).size_limit(1 << 20).build().ok());
        if compiled.is_none() {
            return Err(SchemaIssue::new(
                format!("{ptr}/pattern"),
                "pattern は正しい正規表現の文字列で書いてください",
            ));
        }
    }
    for key in ["minimum", "maximum", "exclusiveMinimum", "exclusiveMaximum"] {
        if s.get(key).is_some_and(|v| !v.is_number()) {
            return Err(SchemaIssue::new(
                format!("{ptr}/{key}"),
                format!("{key} は数値で書いてください"),
            ));
        }
    }
    for key in ["minLength", "maxLength", "minItems", "maxItems"] {
        if s.get(key).is_some_and(|v| v.as_u64().is_none()) {
            return Err(SchemaIssue::new(
                format!("{ptr}/{key}"),
                format!("{key} は 0 以上の整数で書いてください"),
            ));
        }
    }
    for key in ["title", "description", "format"] {
        if s.get(key).is_some_and(|v| !v.is_string()) {
            return Err(SchemaIssue::new(
                format!("{ptr}/{key}"),
                format!("{key} は文字列で書いてください"),
            ));
        }
    }
    Ok(())
}

/// プロパティ名（`^[A-Za-z_][A-Za-z0-9_]{0,63}$`）。
fn valid_property_name(name: &str) -> bool {
    let mut chars = name.chars();
    chars
        .next()
        .is_some_and(|c| c.is_ascii_alphabetic() || c == '_')
        && chars.all(|c| c.is_ascii_alphanumeric() || c == '_')
        && name.len() <= 64
}

/// オブジェクトの項目（閉じていること・プロパティ名・required）。
fn check_object(
    s: &Map<String, Value>,
    ptr: &str,
    depth: usize,
    c: &mut Counts,
) -> Result<(), SchemaIssue> {
    if !matches!(
        s.get("additionalProperties"),
        None | Some(Value::Bool(false))
    ) {
        return Err(SchemaIssue::new(
            format!("{ptr}/additionalProperties"),
            "additionalProperties は false にするか省いてください（出力の形は決めた項目だけの閉じたオブジェクトにします）",
        ));
    }
    let empty = Map::new();
    let props = match s.get("properties") {
        None => &empty,
        Some(Value::Object(p)) => p,
        Some(_) => {
            return Err(SchemaIssue::new(
                format!("{ptr}/properties"),
                "properties は項目名からスキーマへのオブジェクトで書いてください",
            ))
        }
    };
    let mut required: Vec<&str> = Vec::new();
    if let Some(r) = s.get("required") {
        let at = format!("{ptr}/required");
        let Some(names) = r.as_array() else {
            return Err(SchemaIssue::new(
                at,
                "required は項目名の配列で書いてください",
            ));
        };
        for (i, name) in names.iter().enumerate() {
            let Some(name) = name.as_str() else {
                return Err(SchemaIssue::new(
                    format!("{at}/{i}"),
                    "required には項目名（文字列）だけを書いてください",
                ));
            };
            if !props.contains_key(name) {
                return Err(SchemaIssue::new(
                    format!("{at}/{i}"),
                    format!("required の「{name}」は properties にありません"),
                ));
            }
            if required.contains(&name) {
                return Err(SchemaIssue::new(
                    format!("{at}/{i}"),
                    format!("required の「{name}」が重なっています"),
                ));
            }
            required.push(name);
        }
    }
    for (name, sub) in props {
        let at = format!("{ptr}/properties/{}", token(name));
        if !valid_property_name(name) {
            return Err(SchemaIssue::new(
                at,
                format!(
                    "項目名「{name}」は使えません（英字か _ で始め、英数字と _ だけで 64 文字まで）"
                ),
            ));
        }
        c.properties += 1;
        if !required.contains(&name.as_str()) {
            c.optional += 1;
        }
        walk(sub, &at, depth + 1, c)?;
    }
    Ok(())
}

#[cfg(test)]
mod tests;
