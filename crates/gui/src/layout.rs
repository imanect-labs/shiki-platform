//! レイアウト/コンテンツ系コンポーネントの props（PR2・信頼カタログの一部）。
//!
//! いずれも表示専用で、任意 HTML/コード実行の口は持たない（`code_block` も表示のみ）。
//! `accordion` / `tabs` は子に [`UiNode`](crate::spec::UiNode) を持ち、検証は
//! [`validate`](crate::validate) がツリー走査で深さ/個数上限とともに掛ける。

use serde::{Deserialize, Serialize};
use ts_rs::TS;

use crate::spec::UiNode;

/// 注意喚起カード（info/success/warning/danger のトーン）。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS)]
#[serde(deny_unknown_fields)]
#[ts(export)]
pub struct CalloutProps {
    #[serde(default)]
    pub tone: CalloutTone,
    #[serde(default)]
    pub title: Option<String>,
    pub text: String,
}

/// callout のトーン（意味的バリアントのみ）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize, TS)]
#[serde(rename_all = "snake_case")]
#[ts(export)]
pub enum CalloutTone {
    #[default]
    Info,
    Success,
    Warning,
    Danger,
}

/// アコーディオン（折りたたみ節）。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS)]
#[serde(deny_unknown_fields)]
#[ts(export)]
pub struct AccordionProps {
    pub items: Vec<AccordionItem>,
}

/// アコーディオンの 1 節（子ツリーを持つ）。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS)]
#[serde(deny_unknown_fields)]
#[ts(export)]
pub struct AccordionItem {
    pub title: String,
    /// 既定で開くか。
    #[serde(default)]
    pub open: bool,
    pub children: Vec<UiNode>,
}

/// タブ。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS)]
#[serde(deny_unknown_fields)]
#[ts(export)]
pub struct TabsProps {
    pub tabs: Vec<TabItem>,
}

/// タブ 1 枚（子ツリーを持つ）。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS)]
#[serde(deny_unknown_fields)]
#[ts(export)]
pub struct TabItem {
    pub label: String,
    pub children: Vec<UiNode>,
}

/// ステッパー（工程の進捗）。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS)]
#[serde(deny_unknown_fields)]
#[ts(export)]
pub struct StepperProps {
    pub steps: Vec<StepItem>,
}

/// ステップ 1 件。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS)]
#[serde(deny_unknown_fields)]
#[ts(export)]
pub struct StepItem {
    pub title: String,
    #[serde(default)]
    pub status: StepStatus,
    #[serde(default)]
    pub description: Option<String>,
}

/// ステップの状態。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize, TS)]
#[serde(rename_all = "snake_case")]
#[ts(export)]
pub enum StepStatus {
    #[default]
    Todo,
    Doing,
    Done,
}

/// バッジ列（タグ/ラベルの集まり）。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS)]
#[serde(deny_unknown_fields)]
#[ts(export)]
pub struct BadgeListProps {
    pub badges: Vec<BadgeItem>,
}

/// バッジ 1 件。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS)]
#[serde(deny_unknown_fields)]
#[ts(export)]
pub struct BadgeItem {
    pub label: String,
    #[serde(default)]
    pub tone: BadgeTone,
}

/// バッジのトーン。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize, TS)]
#[serde(rename_all = "snake_case")]
#[ts(export)]
pub enum BadgeTone {
    #[default]
    Neutral,
    Info,
    Success,
    Warning,
    Danger,
}

/// 定義リスト（キー: 値の詳細表示）。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS)]
#[serde(deny_unknown_fields)]
#[ts(export)]
pub struct KeyValueProps {
    #[serde(default)]
    pub title: Option<String>,
    pub items: Vec<KeyValueItem>,
}

/// キー: 値の 1 組。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(deny_unknown_fields)]
#[ts(export)]
pub struct KeyValueItem {
    pub key: String,
    pub value: String,
}

/// コードブロック（表示専用・実行しない）。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(deny_unknown_fields)]
#[ts(export)]
pub struct CodeBlockProps {
    pub code: String,
    /// 言語ヒント（シンタックスハイライト用・任意）。
    #[serde(default)]
    pub language: Option<String>,
}
