//! チャートスペック（vega-lite 的サブセット・Task 6.2 / design §4.7）。
//!
//! 宣言的なデータ＋種別のみを許し、式・コールバック・外部データ参照は表現不可能。
//! 描画はフロントの信頼実装（recharts への props マッピング）が行う。

use serde::{Deserialize, Serialize};
use ts_rs::TS;

use crate::vocab::ChartKind;

/// チャート 1 枚の宣言。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS)]
#[serde(deny_unknown_fields)]
#[ts(export)]
pub struct ChartSpec {
    pub kind: ChartKind,
    /// データ点（上限は validate が課す）。
    pub data: Vec<ChartPoint>,
    #[serde(default)]
    pub title: Option<String>,
    #[serde(default)]
    pub x_label: Option<String>,
    #[serde(default)]
    pub y_label: Option<String>,
    /// 多系列を積み上げる（bar / area のみ有効・他種では無視）。
    #[serde(default)]
    pub stacked: bool,
    /// `combo` で line として描く系列名（空なら全系列 bar）。存在しない系列名は無視。
    #[serde(default)]
    pub line_series: Vec<String>,
}

/// データ点 1 件（カテゴリ x ＋ 値 y ＋ 任意の系列名）。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS)]
#[serde(deny_unknown_fields)]
#[ts(export)]
pub struct ChartPoint {
    pub x: String,
    pub y: f64,
    /// 複数系列チャートの系列名（単一系列なら省略）。
    #[serde(default)]
    pub series: Option<String>,
    /// 散布図（`scatter`）の数値 x。省略時は `x`（カテゴリ）を軸に使う。
    #[serde(default)]
    pub xv: Option<f64>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn chart_spec_roundtrip() {
        let spec: ChartSpec = serde_json::from_value(serde_json::json!({
            "kind": "bar",
            "title": "売上",
            "data": [ {"x": "1月", "y": 10.0}, {"x": "2月", "y": 20.0, "series": "A"} ]
        }))
        .unwrap();
        assert_eq!(spec.kind, ChartKind::Bar);
        assert_eq!(spec.data.len(), 2);
    }

    #[test]
    fn unknown_chart_fields_rejected() {
        // 式・コールバック等の持ち込み口は表現不可能。
        assert!(serde_json::from_value::<ChartSpec>(serde_json::json!({
            "kind": "bar", "data": [], "transform": "datum.y * 2"
        }))
        .is_err());
    }
}
