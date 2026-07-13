//! UI スペックの型付きツリー（Task 6.2）。
//!
//! **カタログ外コンポーネント・生 HTML・インラインコード・未知 props はこの型で表現不可能**
//! （serde タグ付き enum ＋ 各 props 構造体の `deny_unknown_fields`）。スキーマは Rust 型が
//! 単一ソースで、ts-rs によりフロントへ生成する（手書き型なし）。
//! 意味検証（深さ/個数/文字列/アクション参照）は [`validate`](crate::validate) が重ねる。

use serde::{Deserialize, Serialize};
use ts_rs::TS;

use crate::action::ActionBinding;
use crate::chart::ChartSpec;
use crate::vocab::ComponentKind;

/// UI スペック文書＝アクション束縛の宣言＋コンポーネントツリー。
///
/// アクションはここで**宣言されたものだけ**が UI から参照でき（Task 6.5）、
/// ツリー側は [`ActionRef`] で id を参照するのみ（任意 URL・任意コードは書けない）。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS)]
#[serde(deny_unknown_fields)]
#[ts(export)]
pub struct UiSpecDoc {
    /// スキーマ版（現在は 1 のみ）。
    pub version: u32,
    /// 宣言的アクション束縛（UI から実行できる操作の全て）。
    #[serde(default)]
    pub actions: Vec<ActionBinding>,
    /// ルートコンポーネント。
    pub root: UiNode,
}

/// 宣言済みアクションへの参照（id のみ・束縛定義はクライアントから送れない）。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(deny_unknown_fields)]
#[ts(export)]
pub struct ActionRef {
    /// [`UiSpecDoc::actions`] 内の束縛 id。
    pub action: String,
}

/// 信頼カタログのコンポーネントツリー（`component` タグ・閉じた集合）。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS)]
#[serde(tag = "component", rename_all = "snake_case")]
#[ts(export)]
pub enum UiNode {
    Container(ContainerProps),
    Text(TextProps),
    Link(LinkProps),
    Form(FormProps),
    Button(ButtonProps),
    Table(TableProps),
    Chart(ChartSpec),
    Stat(StatProps),
    // ---- レイアウト/コンテンツ基盤（PR2・props は crate::layout） ----
    Callout(crate::layout::CalloutProps),
    Accordion(crate::layout::AccordionProps),
    Tabs(crate::layout::TabsProps),
    Stepper(crate::layout::StepperProps),
    BadgeList(crate::layout::BadgeListProps),
    KeyValue(crate::layout::KeyValueProps),
    CodeBlock(crate::layout::CodeBlockProps),
    // ---- 将来予約（vocab::ComponentKind と同期・検証が拒否する） ----
    Map(ReservedProps),
    Image(ReservedProps),
}

impl UiNode {
    /// カタログ語彙との対応（serde タグと 1:1・drift はテストで固定）。
    pub fn kind(&self) -> ComponentKind {
        match self {
            UiNode::Container(_) => ComponentKind::Container,
            UiNode::Text(_) => ComponentKind::Text,
            UiNode::Link(_) => ComponentKind::Link,
            UiNode::Form(_) => ComponentKind::Form,
            UiNode::Button(_) => ComponentKind::Button,
            UiNode::Table(_) => ComponentKind::Table,
            UiNode::Chart(_) => ComponentKind::Chart,
            UiNode::Stat(_) => ComponentKind::Stat,
            UiNode::Callout(_) => ComponentKind::Callout,
            UiNode::Accordion(_) => ComponentKind::Accordion,
            UiNode::Tabs(_) => ComponentKind::Tabs,
            UiNode::Stepper(_) => ComponentKind::Stepper,
            UiNode::BadgeList(_) => ComponentKind::BadgeList,
            UiNode::KeyValue(_) => ComponentKind::KeyValue,
            UiNode::CodeBlock(_) => ComponentKind::CodeBlock,
            UiNode::Map(_) => ComponentKind::Map,
            UiNode::Image(_) => ComponentKind::Image,
        }
    }
}

/// 子要素を縦/横に並べるコンテナ。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS)]
#[serde(deny_unknown_fields)]
#[ts(export)]
pub struct ContainerProps {
    #[serde(default)]
    pub title: Option<String>,
    #[serde(default)]
    pub layout: Layout,
    pub children: Vec<UiNode>,
}

/// コンテナのレイアウト。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize, TS)]
#[serde(rename_all = "snake_case")]
#[ts(export)]
pub enum Layout {
    #[default]
    Vertical,
    Horizontal,
}

/// プレーンテキスト（markdown/HTML は解釈しない）。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS)]
#[serde(deny_unknown_fields)]
#[ts(export)]
pub struct TextProps {
    pub text: String,
    #[serde(default)]
    pub variant: TextVariant,
}

/// テキストの見た目（意味的バリアントのみ・任意スタイルは書けない）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize, TS)]
#[serde(rename_all = "snake_case")]
#[ts(export)]
pub enum TextVariant {
    #[default]
    Body,
    Heading,
    Caption,
}

/// 外部リンク（`https://` のみ・検証で強制）。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS)]
#[serde(deny_unknown_fields)]
#[ts(export)]
pub struct LinkProps {
    pub text: String,
    pub href: String,
}

/// フォーム（送信は宣言済みアクションのみ）。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS)]
#[serde(deny_unknown_fields)]
#[ts(export)]
pub struct FormProps {
    /// フォーム id（文書内で一意）。
    pub id: String,
    #[serde(default)]
    pub title: Option<String>,
    /// 送信先アクション（宣言済み束縛の参照のみ）。
    pub submit: ActionRef,
    pub fields: Vec<FormField>,
    #[serde(default)]
    pub submit_label: Option<String>,
}

/// フォーム部品（`component` タグはカタログ語彙 `text_input` / `select` と共有）。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS)]
#[serde(tag = "component", rename_all = "snake_case")]
#[ts(export)]
pub enum FormField {
    TextInput(TextInputProps),
    Select(SelectProps),
}

impl FormField {
    /// フォーム値のキー。
    pub fn id(&self) -> &str {
        match self {
            FormField::TextInput(p) => &p.id,
            FormField::Select(p) => &p.id,
        }
    }
}

/// テキスト入力。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS)]
#[serde(deny_unknown_fields)]
#[ts(export)]
pub struct TextInputProps {
    /// フォーム値のキー（フォーム内で一意）。
    pub id: String,
    pub label: String,
    #[serde(default)]
    pub placeholder: Option<String>,
    #[serde(default)]
    pub multiline: bool,
    #[serde(default)]
    pub required: bool,
    #[serde(default)]
    pub default: Option<String>,
}

/// 選択（宣言された選択肢のみ）。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS)]
#[serde(deny_unknown_fields)]
#[ts(export)]
pub struct SelectProps {
    pub id: String,
    pub label: String,
    pub options: Vec<SelectOption>,
    #[serde(default)]
    pub required: bool,
    #[serde(default)]
    pub default: Option<String>,
}

/// 選択肢 1 件。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(deny_unknown_fields)]
#[ts(export)]
pub struct SelectOption {
    pub value: String,
    pub label: String,
}

/// ボタン（押下は宣言済みアクションのみ）。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS)]
#[serde(deny_unknown_fields)]
#[ts(export)]
pub struct ButtonProps {
    pub label: String,
    pub on_click: ActionRef,
    #[serde(default)]
    pub variant: ButtonVariant,
}

/// ボタンの見た目。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize, TS)]
#[serde(rename_all = "snake_case")]
#[ts(export)]
pub enum ButtonVariant {
    #[default]
    Primary,
    Secondary,
}

/// 表示専用テーブル（データは props 内・構造化データ連携は Phase 9）。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS)]
#[serde(deny_unknown_fields)]
#[ts(export)]
pub struct TableProps {
    #[serde(default)]
    pub title: Option<String>,
    pub columns: Vec<TableColumn>,
    /// 行（各行の長さは columns と一致・検証で強制）。
    pub rows: Vec<Vec<CellValue>>,
}

/// テーブル列定義。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(deny_unknown_fields)]
#[ts(export)]
pub struct TableColumn {
    pub label: String,
    #[serde(default)]
    pub align: CellAlign,
}

/// セルの揃え。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize, TS)]
#[serde(rename_all = "snake_case")]
#[ts(export)]
pub enum CellAlign {
    #[default]
    Left,
    Right,
    Center,
}

/// セル値（プリミティブのみ・HTML/オブジェクトは表現不可能）。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS)]
#[serde(untagged)]
#[ts(export)]
pub enum CellValue {
    Text(String),
    Number(f64),
    Bool(bool),
}

/// KPI スタットタイル（数値＋前期比デルタ＋インライン sparkline）。
///
/// `value` は表示用の整形済み文字列（例 `"¥1.2M"` `"98.3"`）。`delta` は前期比などの
/// 変化率で、正なら改善（↑・緑）／負なら悪化（↓・赤）として色付けする。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS)]
#[serde(deny_unknown_fields)]
#[ts(export)]
pub struct StatProps {
    /// 指標名（例「今月の売上」）。
    pub label: String,
    /// 表示値（整形済み文字列）。
    pub value: String,
    /// 単位（例「件」「%」）。
    #[serde(default)]
    pub unit: Option<String>,
    /// 前期比などの変化率（正=改善・負=悪化として色付け）。
    #[serde(default)]
    pub delta: Option<f64>,
    /// デルタの補足ラベル（例「前月比」）。
    #[serde(default)]
    pub delta_label: Option<String>,
    /// インライン sparkline の値列（省略可・上限は validate が課す）。
    #[serde(default)]
    pub trend: Vec<f64>,
    /// 補足キャプション。
    #[serde(default)]
    pub caption: Option<String>,
}

/// 予約コンポーネントの props（プレースホルダ・props を持たない）。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(deny_unknown_fields)]
#[ts(export)]
pub struct ReservedProps {}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn catalog_component_serde_shape() {
        // フロントと共有する表現: { "component": "text", "text": "..." }。
        let node = UiNode::Text(TextProps {
            text: "hello".into(),
            variant: TextVariant::Body,
        });
        let json = serde_json::to_value(&node).unwrap();
        assert_eq!(json["component"], "text");
        assert_eq!(json["text"], "hello");
        let back: UiNode = serde_json::from_value(json).unwrap();
        assert_eq!(back, node);
    }

    #[test]
    fn unknown_component_is_unrepresentable() {
        // カタログ外（iframe/script 等）はデシリアライズ不可能＝スキーマ上表現できない。
        let err = serde_json::from_value::<UiNode>(serde_json::json!({
            "component": "iframe", "src": "https://example.com"
        }))
        .unwrap_err();
        assert!(err.to_string().contains("unknown variant"));
    }

    #[test]
    fn unknown_props_are_rejected() {
        // 未知 props（onclick・dangerouslySetInnerHTML 等）は deny_unknown_fields で拒否。
        let err = serde_json::from_value::<UiNode>(serde_json::json!({
            "component": "text", "text": "x", "onclick": "alert(1)"
        }))
        .unwrap_err();
        assert!(err.to_string().contains("unknown field"));
    }

    #[test]
    fn node_kind_matches_serde_tag() {
        // UiNode::kind() と serde タグの drift を固定する（カタログ語彙との 1:1）。
        let samples: Vec<UiNode> = vec![
            UiNode::Container(ContainerProps {
                title: None,
                layout: Layout::Vertical,
                children: vec![],
            }),
            UiNode::Text(TextProps {
                text: String::new(),
                variant: TextVariant::Body,
            }),
            UiNode::Link(LinkProps {
                text: String::new(),
                href: String::new(),
            }),
            UiNode::Button(ButtonProps {
                label: String::new(),
                on_click: ActionRef {
                    action: String::new(),
                },
                variant: ButtonVariant::Primary,
            }),
            UiNode::Stat(StatProps {
                label: String::new(),
                value: String::new(),
                unit: None,
                delta: None,
                delta_label: None,
                trend: vec![],
                caption: None,
            }),
            UiNode::Map(ReservedProps {}),
            UiNode::Image(ReservedProps {}),
        ];
        for node in samples {
            let json = serde_json::to_value(&node).unwrap();
            assert_eq!(json["component"], node.kind().as_str());
        }
    }

    #[test]
    fn cell_value_is_primitive_only() {
        let cell: CellValue = serde_json::from_value(serde_json::json!(1.5)).unwrap();
        assert_eq!(cell, CellValue::Number(1.5));
        // オブジェクト（HTML 断片等の持ち込み口）は表現不可能。
        assert!(
            serde_json::from_value::<CellValue>(serde_json::json!({"html": "<b>x</b>"})).is_err()
        );
    }
}
