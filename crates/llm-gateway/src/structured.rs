//! 構造化出力（GAP-13）の gateway 側の整え: モデルごとのモード・指示への落とし込み・プロバイダへ
//! 送るスキーマ・プロバイダの拒否の分類。
//!
//! 設計の正: docs/design.md §4.5（構造化出力）。
//!
//! - **モード**（[`StructuredOutputMode`]）はカタログの `structured_output` が正で、省略時はプロバイダ
//!   種別とモデルの既定（[`mode_for`]）。Anthropic は対応モデルで `json_schema`（`output_config.format`）、
//!   OpenAI 互換は `prompt`（vLLM・Gemini は対応差が大きいので、確かめたモデルだけ個別に有効にする）。
//! - **どのモードでも正しさは呼び出し側の事後検証で守る**。プロバイダの制約付きデコードは数値・文字列の
//!   制約を受けないので、[`wire_schema`] で落として説明文に書き足すだけにする（モデルへの誘導）。
//! - `prompt`・`json_object` モードでは system の末尾に指示とスキーマを足す（[`apply_mode`]）。
//! - プロバイダがスキーマを拒んだ 400（複雑すぎる・使えない書き方）は、本文を利用者に出さない定型文の
//!   [`LlmError::BadRequest`] にする（[`is_schema_rejection`] で見分けられる）。

use serde::{Deserialize, Serialize};
use serde_json::{json, Map, Value};

use crate::config::{ModelEntry, ProviderKind};
use crate::model::{GenerateRequest, ResponseFormat};
use crate::provider::LlmError;

/// 構造化出力の効かせ方（カタログの `structured_output`）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum StructuredOutputMode {
    /// プロバイダの制約付きデコードに写す（Anthropic の `output_config.format`・OpenAI 互換の
    /// `response_format: {type: "json_schema"}`）。
    JsonSchema,
    /// JSON であることだけをプロバイダに強制させ（OpenAI 互換の `response_format: {type: "json_object"}`）、
    /// スキーマは system の指示で伝える。
    JsonObject,
    /// system の指示だけで伝える（プロバイダには形の指定を送らない）。
    Prompt,
}

impl StructuredOutputMode {
    /// 設定・ログで使う文字列表現。
    pub const fn as_str(self) -> &'static str {
        match self {
            StructuredOutputMode::JsonSchema => "json_schema",
            StructuredOutputMode::JsonObject => "json_object",
            StructuredOutputMode::Prompt => "prompt",
        }
    }
}

/// プロバイダ種別がそのモードをプロバイダへ写せるか（写せないものは [`StructuredOutputMode::Prompt`]
/// へ落とす）。Anthropic に `json_object` に当たるものは無い。Responses API・ChatGPT プランへの
/// 写像（`text.format`）は未実装。
const fn kind_supports(kind: ProviderKind, mode: StructuredOutputMode) -> bool {
    match mode {
        StructuredOutputMode::Prompt => true,
        StructuredOutputMode::JsonSchema => matches!(
            kind,
            ProviderKind::Anthropic | ProviderKind::Openai | ProviderKind::Stub
        ),
        StructuredOutputMode::JsonObject => {
            matches!(kind, ProviderKind::Openai | ProviderKind::Stub)
        }
    }
}

/// モデルの構造化出力のモード。カタログの明示 → 種別とモデルの既定の順で決め、種別が写せない
/// モードは `prompt` にする（黙って制約なしで送らない）。
#[must_use]
pub fn mode_for(kind: ProviderKind, entry: &ModelEntry) -> StructuredOutputMode {
    let wanted = entry
        .structured_output
        .unwrap_or_else(|| default_mode(kind, entry.resolved_id()));
    if kind_supports(kind, wanted) {
        wanted
    } else {
        StructuredOutputMode::Prompt
    }
}

/// 種別とモデル（実 ID）の既定。OpenAI 互換は vLLM・Gemini で対応差が大きいので、既定は指示だけ。
fn default_mode(kind: ProviderKind, real_id: &str) -> StructuredOutputMode {
    match kind {
        ProviderKind::Anthropic if anthropic_supports_json_schema(real_id) => {
            StructuredOutputMode::JsonSchema
        }
        ProviderKind::Stub => StructuredOutputMode::JsonSchema,
        _ => StructuredOutputMode::Prompt,
    }
}

/// Anthropic のモデルが `output_config.format` に対応するか（[対応表] に無い旧モデルだけを外す）。
///
/// 対応表に載るのは Claude 4.5 世代以降（Haiku 4.5・Sonnet 4.5・Opus 4.5〜）。Opus 4.1 以前と
/// Claude 3 系は外す（Opus 4.1 は旧版の文書で対応とされたが、今の表に無いので指示で動かす）。
/// 未知の ID（新しいモデル）は対応扱いにする（`supports_tool_search` と同じ流儀）。
///
/// [対応表]: https://platform.claude.com/docs/en/build-with-claude/structured-outputs
fn anthropic_supports_json_schema(model: &str) -> bool {
    const UNSUPPORTED: [&str; 7] = [
        "claude-3",
        "claude-opus-4-0",
        "claude-opus-4-1",
        "claude-opus-4-2025",
        "claude-sonnet-4-0",
        "claude-sonnet-4-2025",
        "claude-haiku-4-0",
    ];
    !UNSUPPORTED.iter().any(|p| model.starts_with(p))
}

/// 指示に埋め込んだスキーマの開始・終了の印（stub が読み戻すのにも使う）。
pub(crate) const SCHEMA_OPEN: &str = "<json_schema>";
pub(crate) const SCHEMA_CLOSE: &str = "</json_schema>";

/// `response_format` をモードに合わせて整える（gateway の `stream` がアダプタへ渡す前に呼ぶ）。
///
/// - `json_schema`: そのまま渡す（スキーマを求めない [`ResponseFormat::JsonObject`] だけは指示に落とす）。
/// - `json_object`: system の末尾に指示とスキーマを足し、[`ResponseFormat::JsonObject`] に置き換える。
/// - `prompt`: system の末尾に指示とスキーマを足し、`response_format` を外す。
pub fn apply_mode(req: &mut GenerateRequest, mode: StructuredOutputMode) {
    let Some(format) = req.response_format.take() else {
        return;
    };
    match (&format, mode) {
        (ResponseFormat::JsonSchema { .. }, StructuredOutputMode::JsonSchema) => {
            req.response_format = Some(format);
        }
        (ResponseFormat::JsonSchema { schema, .. }, StructuredOutputMode::JsonObject) => {
            append_instruction(req, Some(schema));
            req.response_format = Some(ResponseFormat::JsonObject);
        }
        (ResponseFormat::JsonSchema { schema, .. }, StructuredOutputMode::Prompt) => {
            append_instruction(req, Some(schema));
        }
        (ResponseFormat::JsonObject, StructuredOutputMode::JsonObject) => {
            append_instruction(req, None);
            req.response_format = Some(ResponseFormat::JsonObject);
        }
        (ResponseFormat::JsonObject, _) => append_instruction(req, None),
    }
}

/// 出力の形の指示（スキーマはこちらが組んだ JSON をそのまま埋める）。
fn instruction(schema: Option<&Value>) -> String {
    const HEAD: &str = "出力の形式: ";
    const TAIL: &str = "前後に説明やコードフェンス（```）を付けないでください。";
    match schema {
        Some(s) => format!(
            "{HEAD}次の JSON Schema に合う JSON オブジェクトを 1 つだけ出力してください。{TAIL}\n\
             {SCHEMA_OPEN}\n{s}\n{SCHEMA_CLOSE}"
        ),
        None => format!("{HEAD}JSON オブジェクトを 1 つだけ出力してください。{TAIL}"),
    }
}

fn append_instruction(req: &mut GenerateRequest, schema: Option<&Value>) {
    let text = instruction(schema);
    req.system = Some(match req.system.take() {
        Some(s) if !s.trim().is_empty() => format!("{s}\n\n{text}"),
        _ => text,
    });
}

/// 指示に埋め込んだスキーマを読み戻す（`prompt`・`json_object` モードを stub で試すため）。
pub(crate) fn schema_in_system(system: &str) -> Option<Value> {
    let start = system.rfind(SCHEMA_OPEN)? + SCHEMA_OPEN.len();
    let end = start + system[start..].find(SCHEMA_CLOSE)?;
    serde_json::from_str(system[start..end].trim()).ok()
}

/// プロバイダが受けない制約（落として説明文に書き足す）。Anthropic は数値・文字列の制約を受けず、
/// 配列の `minItems` も 0・1 しか受けない。正規表現も一部の書き方しか受けないので落とす。
const DROPPED_CONSTRAINTS: [&str; 10] = [
    "minimum",
    "maximum",
    "exclusiveMinimum",
    "exclusiveMaximum",
    "multipleOf",
    "minLength",
    "maxLength",
    "pattern",
    "minItems",
    "maxItems",
];
/// 送っても意味のない注記（黙って落とす）。
const DROPPED_ANNOTATIONS: [&str; 5] = ["default", "examples", "$schema", "$comment", "x-order"];
/// プロバイダが受ける文字列の `format`（Anthropic の対応表）。それ以外は落として説明文に書く。
const SUPPORTED_FORMATS: [&str; 10] = [
    "date-time",
    "time",
    "date",
    "duration",
    "email",
    "hostname",
    "uri",
    "ipv4",
    "ipv6",
    "uuid",
];
/// 辿る入れ子の上限（呼び出し側の検査で深さは抑えてあるので、ここは暴走止め）。
const MAX_WIRE_DEPTH: usize = 64;

/// プロバイダへ送るスキーマ。受けない制約を落として説明文へ書き足し、全オブジェクトを閉じる
/// （`additionalProperties: false`。Anthropic は全オブジェクトに求め、OpenAI の strict も同じ）。
///
/// 元のスキーマ（呼び出し側が事後検証に使うもの）は変えない。落とした制約は、事後検証で守る。
#[must_use]
pub fn wire_schema(schema: &Value) -> Value {
    let mut out = schema.clone();
    close_and_strip(&mut out, 0);
    out
}

fn close_and_strip(v: &mut Value, depth: usize) {
    let Value::Object(map) = v else {
        return;
    };
    if depth > MAX_WIRE_DEPTH {
        return;
    }
    let mut notes: Vec<String> = DROPPED_CONSTRAINTS
        .iter()
        .filter_map(|key| map.remove(*key).map(|val| constraint_note(key, &val)))
        .collect();
    if let Some(f) = map.get("format").and_then(Value::as_str).map(str::to_owned) {
        if !SUPPORTED_FORMATS.contains(&f.as_str()) {
            map.remove("format");
            notes.push(format!("形式 {f}"));
        }
    }
    for key in DROPPED_ANNOTATIONS {
        map.remove(key);
    }
    if !notes.is_empty() {
        let note = notes.join("、");
        let described = match map.get("description").and_then(Value::as_str) {
            Some(d) if !d.trim().is_empty() => format!("{d}（制約: {note}）"),
            _ => format!("制約: {note}"),
        };
        map.insert("description".into(), Value::String(described));
    }
    if is_object_schema(map) {
        map.insert("additionalProperties".into(), Value::Bool(false));
    }
    for child in child_schemas(map) {
        close_and_strip(child, depth + 1);
    }
}

/// スキーマの子（`properties` の値・`items`・`anyOf` 等の枝・`$defs`）。
fn child_schemas(map: &mut Map<String, Value>) -> Vec<&mut Value> {
    let mut out: Vec<&mut Value> = Vec::new();
    for (key, val) in map.iter_mut() {
        match (key.as_str(), val) {
            ("properties" | "$defs" | "definitions", Value::Object(m)) => {
                out.extend(m.values_mut());
            }
            ("items" | "anyOf" | "allOf" | "oneOf" | "prefixItems", Value::Array(xs)) => {
                out.extend(xs.iter_mut());
            }
            ("items", v @ Value::Object(_)) => out.push(v),
            _ => {}
        }
    }
    out
}

fn is_object_schema(map: &Map<String, Value>) -> bool {
    map.contains_key("properties")
        || match map.get("type") {
            Some(Value::String(t)) => t == "object",
            Some(Value::Array(ts)) => ts.iter().any(|t| t == "object"),
            _ => false,
        }
}

/// 落とした制約の説明（モデルに読ませる短い日本語）。
fn constraint_note(key: &str, val: &Value) -> String {
    let v = match val {
        Value::String(s) => s.clone(),
        other => other.to_string(),
    };
    match key {
        "minimum" => format!("{v} 以上"),
        "maximum" => format!("{v} 以下"),
        "exclusiveMinimum" => format!("{v} より大きい"),
        "exclusiveMaximum" => format!("{v} より小さい"),
        "multipleOf" => format!("{v} の倍数"),
        "minLength" => format!("{v} 文字以上"),
        "maxLength" => format!("{v} 文字以内"),
        "pattern" => format!("正規表現 {v} に合う"),
        "minItems" => format!("{v} 件以上"),
        "maxItems" => format!("{v} 件以内"),
        _ => format!("{key} {v}"),
    }
}

/// OpenAI の strict（制約付きデコード）を名乗れるか: 全オブジェクトで、全プロパティが required。
///
/// strict は任意項目を許さない。満たさないスキーマは strict を付けずに送り、事後検証で守る。
/// 引数は [`wire_schema`] を通したもの（閉じていて、受けない制約が落ちている）。
#[must_use]
pub fn openai_strict_eligible(wire: &Value) -> bool {
    strict_node(wire, 0)
}

fn strict_node(v: &Value, depth: usize) -> bool {
    let Value::Object(map) = v else {
        return true;
    };
    if depth > MAX_WIRE_DEPTH {
        return false;
    }
    if let Some(Value::Object(props)) = map.get("properties") {
        let required: Vec<&str> = map
            .get("required")
            .and_then(Value::as_array)
            .map(|r| r.iter().filter_map(Value::as_str).collect())
            .unwrap_or_default();
        if !props.keys().all(|k| required.contains(&k.as_str())) {
            return false;
        }
    }
    child_schema_refs(map)
        .into_iter()
        .all(|c| strict_node(c, depth + 1))
}

/// [`child_schemas`] の読み取り版。
fn child_schema_refs(map: &Map<String, Value>) -> Vec<&Value> {
    let mut out: Vec<&Value> = Vec::new();
    for (key, val) in map {
        match (key.as_str(), val) {
            ("properties" | "$defs" | "definitions", Value::Object(m)) => out.extend(m.values()),
            ("items" | "anyOf" | "allOf" | "oneOf" | "prefixItems", Value::Array(xs)) => {
                out.extend(xs.iter());
            }
            ("items", v @ Value::Object(_)) => out.push(v),
            _ => {}
        }
    }
    out
}

/// OpenAI 互換（Chat Completions）の `response_format`。
#[must_use]
pub fn openai_response_format(format: &ResponseFormat) -> Value {
    match format {
        ResponseFormat::JsonSchema { name, schema } => {
            let wire = wire_schema(schema);
            let strict = openai_strict_eligible(&wire);
            json!({
                "type": "json_schema",
                "json_schema": { "name": wire_name(name), "schema": wire, "strict": strict },
            })
        }
        ResponseFormat::JsonObject => json!({ "type": "json_object" }),
    }
}

/// Anthropic の `output_config.format`（スキーマを求めない形は Anthropic に無いので `None`）。
#[must_use]
pub fn anthropic_format(format: &ResponseFormat) -> Option<Value> {
    match format {
        ResponseFormat::JsonSchema { schema, .. } => {
            Some(json!({ "type": "json_schema", "schema": wire_schema(schema) }))
        }
        ResponseFormat::JsonObject => None,
    }
}

/// OpenAI の `json_schema.name`（`^[a-zA-Z0-9_-]{1,64}$`）。外れる文字は `_` にし、空なら `output`。
fn wire_name(name: &str) -> String {
    let cleaned: String = name
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '_' || c == '-' {
                c
            } else {
                '_'
            }
        })
        .take(64)
        .collect();
    if cleaned.is_empty() {
        "output".to_string()
    } else {
        cleaned
    }
}

/// プロバイダが構造化出力のスキーマを拒んだときの [`LlmError::BadRequest`] の文言の頭。
pub const SCHEMA_REJECTED_PREFIX: &str = "出力の形（JSON Schema）";
/// スキーマが複雑すぎる（任意項目・union の上限・文法のコンパイルの上限）。
const SCHEMA_TOO_COMPLEX: &str = "出力の形（JSON Schema）が複雑すぎて、モデルが受け付けませんでした。任意の項目や null を許す項目を減らしてください。";
/// それ以外の拒否（使えない書き方）。
const SCHEMA_REJECTED: &str = "出力の形（JSON Schema）を、モデルが受け付けませんでした。このモデルでは使えない書き方が含まれています。";

/// 構造化出力を求めた要求への 4xx が、スキーマを拒んだものなら定型文の誤りにする。
///
/// プロバイダの本文（組織 ID・内部の文言が入り得る）は利用者に出さず、ログにだけ残す。スキーマと
/// 関係のない 4xx（プロンプトが長すぎる等）は `None`（呼び出し側の従来の扱いに任せる）。
pub(crate) fn schema_rejection(
    provider: &str,
    status: reqwest::StatusCode,
    body: &str,
) -> Option<LlmError> {
    if status != reqwest::StatusCode::BAD_REQUEST
        && status != reqwest::StatusCode::UNPROCESSABLE_ENTITY
    {
        return None;
    }
    let lower = body.to_ascii_lowercase();
    // Anthropic の実際の文言（2026-10-27 に確かめた）: 上限を超えた任意項目は「Schemas contains too many
    // optional parameters (26), which would make grammar compilation inefficient. … (limit: 24)」、
    // コンパイルの上限は「Schema is too complex for compilation.」。
    let too_complex = ["too complex", "grammar", "too many optional", "union type"]
        .iter()
        .any(|k| lower.contains(k));
    let about_format = ["output_config", "response_format", "json_schema", "guided"]
        .iter()
        .any(|k| lower.contains(k));
    if !too_complex && !about_format {
        return None;
    }
    tracing::warn!(provider, %status, body = %body, "プロバイダが構造化出力のスキーマを拒否した");
    Some(LlmError::BadRequest(
        if too_complex {
            SCHEMA_TOO_COMPLEX
        } else {
            SCHEMA_REJECTED
        }
        .to_string(),
    ))
}

/// プロバイダが構造化出力のスキーマを拒んだ誤りか（呼び出し側が出力の形の誤りとして扱うため）。
#[must_use]
pub fn is_schema_rejection(err: &LlmError) -> bool {
    matches!(err, LlmError::BadRequest(m) if m.starts_with(SCHEMA_REJECTED_PREFIX))
}

#[cfg(test)]
mod tests;
