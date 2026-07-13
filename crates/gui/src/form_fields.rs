//! リッチ入力フォーム部品の props（PR3・[`FormField`](crate::spec::FormField) の一部）。
//!
//! いずれも宣言的な値のみを持ち、送信は既存のフォーム dispatch（`chat.submit`）を通る。
//! 単一選択（radio）/複数選択（checkbox）は選択肢外の自由記述（`allow_other`）を許せる。

use serde::{Deserialize, Serialize};
use ts_rs::TS;

use crate::spec::SelectOption;

/// 複数選択（チェックボックス群）。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS)]
#[serde(deny_unknown_fields)]
#[ts(export)]
pub struct CheckboxGroupProps {
    pub id: String,
    pub label: String,
    pub options: Vec<SelectOption>,
    /// 既定で選択済みの value 群。
    #[serde(default)]
    pub default: Vec<String>,
    #[serde(default)]
    pub required: bool,
    /// 「その他」自由記述を許す。
    #[serde(default)]
    pub allow_other: bool,
}

/// 単一選択（ラジオ群・select の視覚差し替え）。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS)]
#[serde(deny_unknown_fields)]
#[ts(export)]
pub struct RadioGroupProps {
    pub id: String,
    pub label: String,
    pub options: Vec<SelectOption>,
    #[serde(default)]
    pub default: Option<String>,
    #[serde(default)]
    pub required: bool,
    #[serde(default)]
    pub allow_other: bool,
}

/// 日付（`range=true` で期間・値は ISO 文字列）。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(deny_unknown_fields)]
#[ts(export)]
pub struct DateProps {
    pub id: String,
    pub label: String,
    /// 期間選択（開始/終了）にする。
    #[serde(default)]
    pub range: bool,
    #[serde(default)]
    pub min: Option<String>,
    #[serde(default)]
    pub max: Option<String>,
    #[serde(default)]
    pub required: bool,
    #[serde(default)]
    pub default: Option<String>,
}

/// スライダー（数値範囲）。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS)]
#[serde(deny_unknown_fields)]
#[ts(export)]
pub struct SliderProps {
    pub id: String,
    pub label: String,
    pub min: f64,
    pub max: f64,
    #[serde(default)]
    pub step: Option<f64>,
    #[serde(default)]
    pub default: Option<f64>,
}

/// レーティング（星の数など）。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(deny_unknown_fields)]
#[ts(export)]
pub struct RatingProps {
    pub id: String,
    pub label: String,
    /// 最大値（省略時 5）。
    #[serde(default)]
    pub max: Option<u32>,
    #[serde(default)]
    pub default: Option<u32>,
    #[serde(default)]
    pub required: bool,
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::spec::{FormField, SelectOption};

    /// リッチ入力各 props の serde ラウンドトリップ（Serialize/Deserialize 両方向）。
    #[test]
    fn form_field_props_roundtrip() {
        let opt = || SelectOption {
            value: "a".into(),
            label: "A".into(),
        };
        let fields: Vec<FormField> = vec![
            FormField::Checkbox(CheckboxGroupProps {
                id: "c".into(),
                label: "多選".into(),
                options: vec![opt()],
                default: vec!["a".into()],
                required: true,
                allow_other: true,
            }),
            FormField::Radio(RadioGroupProps {
                id: "r".into(),
                label: "単選".into(),
                options: vec![opt()],
                default: Some("a".into()),
                required: false,
                allow_other: false,
            }),
            FormField::Date(DateProps {
                id: "d".into(),
                label: "日付".into(),
                range: true,
                min: Some("2026-01-01".into()),
                max: None,
                required: false,
                default: None,
            }),
            FormField::Slider(SliderProps {
                id: "s".into(),
                label: "量".into(),
                min: 0.0,
                max: 100.0,
                step: Some(5.0),
                default: Some(50.0),
            }),
            FormField::Rating(RatingProps {
                id: "rt".into(),
                label: "評価".into(),
                max: Some(5),
                default: Some(4),
                required: false,
            }),
        ];
        for f in fields {
            let json = serde_json::to_value(&f).expect("serialize");
            let back: FormField = serde_json::from_value(json).expect("deserialize");
            assert_eq!(back, f);
        }
    }
}
