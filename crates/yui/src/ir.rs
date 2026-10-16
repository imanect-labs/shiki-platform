//! 操作を持つ種類の IR（`@imanect/yui` の ir.ts と同じ形の JSON）。

use serde::Serialize;
use serde_json::Value;

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Step {
    pub title: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub desc: Option<String>,
    pub refs: Vec<u32>,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct ChoiceOption {
    pub label: String,
    pub value: String,
}

/// app の入力部品。`kind` は TS の `type`。
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct AppField {
    pub key: String,
    pub label: String,
    #[serde(rename = "type")]
    pub kind: FieldKind,
    pub required: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub placeholder: Option<String>,
    /// 初期値（文字・数値・真偽・文字の配列）。
    #[serde(skip_serializing_if = "Option::is_none")]
    pub initial: Option<Value>,
    #[serde(
        skip_serializing_if = "Option::is_none",
        serialize_with = "ser_opt_num"
    )]
    pub min: Option<f64>,
    #[serde(
        skip_serializing_if = "Option::is_none",
        serialize_with = "ser_opt_num"
    )]
    pub max: Option<f64>,
    #[serde(
        skip_serializing_if = "Option::is_none",
        serialize_with = "ser_opt_num"
    )]
    pub step: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub unit: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub options: Option<Vec<ChoiceOption>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub multi: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub other: Option<bool>,
}

#[allow(clippy::ref_option)]
fn ser_opt_num<S: serde::Serializer>(v: &Option<f64>, s: S) -> Result<S::Ok, S::Error> {
    match v {
        Some(n) => crate::text::json_num(*n).serialize(s),
        None => s.serialize_none(),
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum FieldKind {
    Text,
    Textarea,
    Number,
    Slider,
    Count,
    Choice,
    Date,
    Daterange,
    Month,
    Toggle,
    Rating,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(tag = "kind", rename_all = "lowercase")]
pub enum ActionTarget {
    Workflow { name: String },
    Chat,
}

/// ボタン。`id` はソースの順の `a1`・`a2`…（ask は `a1`、plan は `approve`・`revise`）。
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct AppAction {
    pub id: String,
    pub label: String,
    pub target: ActionTarget,
    pub confirm: bool,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct AppSection {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub desc: Option<String>,
    pub fields: Vec<AppField>,
    pub actions: Vec<AppAction>,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct AskOption {
    pub label: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub desc: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct AskQuestion {
    pub id: String,
    pub text: String,
    pub options: Vec<AskOption>,
    pub multi: bool,
    pub other: bool,
    pub required: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub placeholder: Option<String>,
}

/// 図の共通項目。
#[derive(Debug, Clone, PartialEq, Serialize, Default)]
pub struct Base {
    pub title: String,
    pub refs: Vec<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub note: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub src: Option<String>,
}

/// 操作を持つ図。
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(tag = "kind", rename_all = "lowercase")]
pub enum Figure {
    App {
        #[serde(flatten)]
        base: Base,
        sections: Vec<AppSection>,
    },
    Ask {
        #[serde(flatten)]
        base: Base,
        #[serde(skip_serializing_if = "Option::is_none")]
        intro: Option<String>,
        questions: Vec<AskQuestion>,
        #[serde(rename = "submitLabel", skip_serializing_if = "Option::is_none")]
        submit_label: Option<String>,
    },
    Plan {
        #[serde(flatten)]
        base: Base,
        #[serde(skip_serializing_if = "Option::is_none")]
        intro: Option<String>,
        steps: Vec<Step>,
        revise: bool,
        #[serde(rename = "approveLabel", skip_serializing_if = "Option::is_none")]
        approve_label: Option<String>,
    },
}

/// 操作を持つ種類。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    App,
    Ask,
    Plan,
}
