//! 画面の入力と、束ねたワークフローの入力項目の突き合わせ（GAP-12・BUG-122）。
//!
//! - [`input_hints`] は、ワークフローの入力項目（`input_schema`）から、画面の組み立ての部品の候補と突き合わせに使う
//!   手がかりを作る。決まりはここだけに置く（web のエディタは `POST /mini-apps/workflow-inputs` で受け取る）。
//! - [`input_warnings`] は、保存のときに、画面の app の図のボタンの実行先の入力と部品を突き合わせる。規則は画面の
//!   エディタ（Yui の `checkApp`）と同じ（[`yui::check_inputs`]）。注意だけを返し、保存も公開も止めない。
//! - [`screen_input_warnings`] は、保存の前の「書く」タブの突き合わせ（`POST /mini-apps/input-check`）。保存のときと
//!   同じ関数で、同じ図（エラーの無い図・[`yui::extract_action_blocks`]）を読む。警告だけの図も突き合わせる（BUG-221）。
//!
//! 入力項目が読めないワークフロー（削除された・見る権限がない）と、入力項目を決めていない（`properties` が無い）
//! ワークフローは突き合わせない（入力が無いのではなく、分からない・BUG-122）。

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};
use serde_json::Value;
use utoipa::ToSchema;

use crate::miniapp::{MiniAppBody, MAX_SCREEN_PROBLEMS};
use crate::validate::GuiValidationError;

/// 部品の候補の表示名の長さ（字）。
const MAX_LABEL_CHARS: usize = 40;
/// 選択肢の数の上限（部品の候補に載せる数）。
const MAX_OPTIONS: usize = 20;
/// 文章の部品（textarea）にする文字数の下限（`maxLength` がこれを超える項目）。
const TEXTAREA_OVER: f64 = 200.0;

/// 「書く」タブの突き合わせ（[`screen_input_warnings`]）で受ける手がかりの数の上限（束ねたワークフローの合計）。
pub const MAX_CHECK_HINTS: usize = crate::miniapp::MAX_WORKFLOWS * 200;

/// 入力を受ける部品の種類（Yui の app の部品の種類のうち、入力項目から決まるもの）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "lowercase")]
pub enum InputHintType {
    Text,
    Textarea,
    Number,
    Choice,
    Date,
    Toggle,
}

impl InputHintType {
    fn field_kind(self) -> yui::FieldKind {
        match self {
            InputHintType::Text => yui::FieldKind::Text,
            InputHintType::Textarea => yui::FieldKind::Textarea,
            InputHintType::Number => yui::FieldKind::Number,
            InputHintType::Choice => yui::FieldKind::Choice,
            InputHintType::Date => yui::FieldKind::Date,
            InputHintType::Toggle => yui::FieldKind::Toggle,
        }
    }
}

/// ワークフローの入力 1 つの手がかり（Yui のエディタの `WorkflowInputHint` と同じ形・web は OpenAPI の型で受ける）。
///
/// 「書く」タブの突き合わせでは、`POST /mini-apps/workflow-inputs` が返したものを web がそのまま送り返す。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, ToSchema)]
pub struct WorkflowInputHint {
    /// 入力のキー（画面の部品の `as` 名）。
    pub key: String,
    /// 表示名（入力項目の `title`・40 字まで）。
    #[serde(skip_serializing_if = "Option::is_none")]
    pub label: Option<String>,
    /// 受ける部品の種類。
    #[serde(rename = "type")]
    pub kind: InputHintType,
    pub required: bool,
    /// 選択肢（2 つ以上あるときだけ・20 個まで）。
    #[serde(skip_serializing_if = "Option::is_none")]
    pub options: Option<Vec<String>>,
    /// 複数を選ぶ（値の配列を受ける入力）。
    #[serde(skip_serializing_if = "Option::is_none")]
    pub multi: Option<bool>,
    /// 数の範囲（下限と上限の両方があるときだけ）。
    #[serde(skip_serializing_if = "Option::is_none")]
    pub min: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub max: Option<f64>,
}

/// 入力項目から手がかりを作る（入力項目の並び `x-order` の順）。入力項目を決めていない（`properties` が無い）なら
/// `None`（突き合わせない）。部品にできない項目（オブジェクト・選択肢の無い配列など）は載せない。
#[must_use]
pub fn input_hints(schema: Option<&Value>) -> Option<Vec<WorkflowInputHint>> {
    let schema = schema?;
    schema.get("properties").and_then(Value::as_object)?;
    let required: Vec<&str> = schema
        .get("required")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(Value::as_str)
        .collect();
    let hints = workflow_engine::ir::input_schema::ordered_properties(schema)
        .into_iter()
        .filter_map(|(key, spec)| hint(key, spec, required.contains(&key)))
        .collect();
    Some(hints)
}

fn hint(key: &str, spec: &Value, required: bool) -> Option<WorkflowInputHint> {
    let p = spec.as_object()?;
    let ty = match p.get("type") {
        Some(Value::Array(types)) => types
            .iter()
            .filter_map(Value::as_str)
            .find(|t| *t != "null"),
        Some(Value::String(t)) => Some(t.as_str()),
        _ => None,
    };
    let label = p
        .get("title")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|t| !t.is_empty())
        .map(|t| t.chars().take(MAX_LABEL_CHARS).collect());
    let mut out = WorkflowInputHint {
        key: key.to_string(),
        label,
        kind: InputHintType::Text,
        required,
        options: None,
        multi: None,
        min: None,
        max: None,
    };
    let options: Option<Vec<String>> = p
        .get("enum")
        .or_else(|| p.get("items").and_then(|i| i.get("enum")))
        .and_then(Value::as_array)
        .map(|o| {
            o.iter()
                .filter_map(Value::as_str)
                .map(str::to_string)
                .collect()
        });
    match (options, ty) {
        (Some(options), _) if options.len() >= 2 => {
            out.kind = InputHintType::Choice;
            out.options = Some(options.into_iter().take(MAX_OPTIONS).collect());
            // 値の配列（items.enum）は複数選択（文字 1 つを送ると、起動のときに入力の検証で断られる）。
            if ty == Some("array") {
                out.multi = Some(true);
            }
        }
        (_, Some("number" | "integer")) => {
            out.kind = InputHintType::Number;
            if let (Some(min), Some(max)) = (
                p.get("minimum").and_then(Value::as_f64),
                p.get("maximum").and_then(Value::as_f64),
            ) {
                out.min = Some(min);
                out.max = Some(max);
            }
        }
        (_, Some("boolean")) => out.kind = InputHintType::Toggle,
        (_, Some("string")) if p.get("format").and_then(Value::as_str) == Some("date") => {
            out.kind = InputHintType::Date;
        }
        (_, Some("string")) => {
            if p.get("maxLength")
                .and_then(Value::as_f64)
                .is_some_and(|n| n > TEXTAREA_OVER)
            {
                out.kind = InputHintType::Textarea;
            }
        }
        _ => return None,
    }
    Some(out)
}

/// 保存のときの突き合わせの注意（画面の app の図ごと・`path` は `screen`・保存も公開も止めない）。
///
/// `inputs` は束ねたワークフローの別名ごとの手がかり（読めない・入力項目を決めていないものは `None`）。文言は
/// Yui のエディタと同じ文に、図と部品（ボタン）の場所を前に付ける。
#[must_use]
pub fn input_warnings(
    body: &MiniAppBody,
    inputs: &BTreeMap<String, Option<Vec<WorkflowInputHint>>>,
) -> Vec<GuiValidationError> {
    screen_input_warnings(
        &body.screen,
        body.workflows
            .iter()
            .map(|w| (w.alias.as_str(), inputs.get(&w.alias).cloned().flatten())),
    )
}

/// 画面（Markdown）の app の図ごとの突き合わせの注意（保存の前の「書く」タブと、保存のときの 1 か所・BUG-221）。
///
/// `workflows` はボタンの実行先に選べるワークフローの別名と手がかり（読めない・入力項目を決めていないものは `None`・
/// 突き合わせない）。読む図は、画面がボタンを出す図（閉じていて、エラーの無いトップレベルの図）だけ。警告だけの図も
/// ボタンを出すので突き合わせる。
#[must_use]
pub fn screen_input_warnings<'a>(
    screen: &str,
    workflows: impl IntoIterator<Item = (&'a str, Option<Vec<WorkflowInputHint>>)>,
) -> Vec<GuiValidationError> {
    let workflows: Vec<yui::WorkflowInputs> = workflows
        .into_iter()
        .map(|(alias, hints)| yui::WorkflowInputs {
            name: alias.to_string(),
            inputs: hints.map(|hints| {
                hints
                    .into_iter()
                    .map(|h| yui::InputHint {
                        key: h.key,
                        label: h.label,
                        kind: Some(h.kind.field_kind()),
                        required: h.required,
                    })
                    .collect()
            }),
        })
        .collect();
    let mut out = Vec::new();
    for block in yui::extract_action_blocks(screen) {
        let yui::Figure::App { base, sections } = &block.figure else {
            continue;
        };
        for p in yui::check_inputs(&block.figure, &workflows) {
            let section = &sections[p.section];
            let part = match (p.field, p.action) {
                (Some(fi), _) => format!("の「{}」", section.fields[fi].label),
                (None, Some(ai)) => format!("のボタン「{}」", section.actions[ai].label),
                (None, None) => String::new(),
            };
            out.push(
                GuiValidationError::new(
                    &format!("miniapp.{}", p.code),
                    format!("画面の図「{}」{part}: {}", base.title, p.message),
                )
                .at("screen"),
            );
            if out.len() >= MAX_SCREEN_PROBLEMS {
                return out;
            }
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;
    use crate::miniapp::validate_miniapp_body;

    #[test]
    fn hints_follow_the_input_schema_in_order() {
        let schema = json!({
            "type": "object", "required": ["amount", "title"], "x-order": ["title", "amount"],
            "properties": {
                "amount": { "type": "integer", "minimum": 1, "maximum": 100 },
                "body": { "type": "string", "maxLength": 1000 },
                "day": { "type": "string", "format": "date" },
                "kind": { "type": "string", "enum": ["a", "b"] },
                "ok": { "type": "boolean" },
                "tags": { "type": "array", "items": { "type": "string", "enum": ["x", "y"] } },
                "title": { "type": ["string", "null"], "title": "  件名  " },
                "where": { "type": "object" }
            }
        });
        let hints = input_hints(Some(&schema)).unwrap();
        let got: Vec<String> = hints
            .iter()
            .map(|h| serde_json::to_string(h).unwrap())
            .collect();
        assert_eq!(
            got,
            vec![
                r#"{"key":"title","label":"件名","type":"text","required":true}"#,
                r#"{"key":"amount","type":"number","required":true,"min":1.0,"max":100.0}"#,
                r#"{"key":"body","type":"textarea","required":false}"#,
                r#"{"key":"day","type":"date","required":false}"#,
                r#"{"key":"kind","type":"choice","required":false,"options":["a","b"]}"#,
                r#"{"key":"ok","type":"toggle","required":false}"#,
                r#"{"key":"tags","type":"choice","required":false,"options":["x","y"],"multi":true}"#,
            ]
        );
        // 入力項目を決めていない・読めないものは突き合わせない。
        assert_eq!(input_hints(Some(&json!({ "type": "object" }))), None);
        assert_eq!(input_hints(None), None);
        assert_eq!(
            input_hints(Some(&json!({ "type": "object", "properties": {} }))),
            Some(vec![])
        );
    }

    /// 保存のときの注意は、図と部品の場所を前に付けた Yui のエディタと同じ文。読めないワークフローは言わない。
    #[test]
    fn warnings_name_the_figure_and_the_field() {
        let body = validate_miniapp_body(&json!({
            "description": "経費",
            "screen": "```ui\napp 経費の申請\n金額 = text as amount\nメモ = text as memo\nbutton 申請する -> workflow expense\n```\n\n```ui\napp 照会\n番号 = text 必須 as no\nbutton 探す -> workflow lookup\n```\n",
            "workflows": [
                { "alias": "expense", "artifact_id": uuid::Uuid::from_u128(1), "version": 1 },
                { "alias": "lookup", "artifact_id": uuid::Uuid::from_u128(2), "version": 1 }
            ]
        }))
        .unwrap();
        let schema = json!({ "type": "object", "required": ["amount", "title"], "properties": {
            "amount": { "type": "integer" }, "title": { "type": "string", "title": "件名" } } });
        let mut inputs = BTreeMap::new();
        inputs.insert("expense".to_string(), input_hints(Some(&schema)));
        // lookup は読めない（入力項目が分からない）。
        inputs.insert("lookup".to_string(), None);
        let got: Vec<(String, String)> = input_warnings(&body, &inputs)
            .into_iter()
            .map(|e| {
                assert_eq!(e.path.as_deref(), Some("screen"));
                (e.code, e.message)
            })
            .collect();
        assert_eq!(
            got,
            vec![
                ("miniapp.input_type".to_string(), "画面の図「経費の申請」の「金額」: 「expense」の「amount」は数値を受けます（この部品は文字）".to_string()),
                ("miniapp.input_required".to_string(), "画面の図「経費の申請」の「金額」: 「expense」では必須の入力です。「必須」にしてください".to_string()),
                ("miniapp.missing_input".to_string(), "画面の図「経費の申請」のボタン「申請する」: 「expense」の必須の入力「件名」（title）に当たる部品がありません".to_string()),
                ("miniapp.unknown_input".to_string(), "画面の図「経費の申請」の「メモ」: 「expense」に「memo」という入力はありません（送っても使われないか、断られます）".to_string()),
            ]
        );
    }

    /// 警告だけの図（読めない行がある）もボタンを出すので、保存の前の「書く」タブと同じ関数で突き合わせる（BUG-221）。
    /// エラーのある図と閉じていない図はボタンを出さないので、突き合わせない。
    #[test]
    fn warning_only_figures_are_checked_like_on_save() {
        let schema = json!({ "type": "object", "required": ["title"], "properties": {
            "title": { "type": "string", "title": "件名" } } });
        let hints = input_hints(Some(&schema));
        let check = |screen: &str| -> Vec<String> {
            screen_input_warnings(screen, [("expense", hints.clone())])
                .into_iter()
                .map(|e| e.code)
                .collect()
        };
        let warned = "```ui\napp 経費の申請\n金額 = number as amount\nこれは読めない行\nbutton 申請する -> workflow expense\n```\n";
        let parsed = yui::extract_action_blocks(warned);
        assert!(
            !parsed[0].diagnostics.is_empty(),
            "警告のある図: {:?}",
            parsed[0].diagnostics
        );
        assert_eq!(
            check(warned),
            vec!["miniapp.missing_input", "miniapp.unknown_input"]
        );
        let unclosed =
            "```ui\napp 経費の申請\n金額 = number as amount\nbutton 申請する -> workflow expense\n";
        assert!(
            check(unclosed).is_empty(),
            "閉じていない図はボタンを出さない"
        );
        // 読めないワークフロー（手がかりが無い）は突き合わせない。
        assert!(screen_input_warnings(warned, [("expense", None)]).is_empty());
    }
}
