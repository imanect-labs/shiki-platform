//! Word のグラフ（insert_chart）の検証と正規化（#731）。見た目は Excel と共有（[`ChartLook`]）。

use serde_json::{json, Map, Value};

use super::ops::{ChartKind, DocOp};
use crate::engine::chart::ChartLook;

/// 項目の上限（Yui の表と同じ）。
const MAX_CATEGORIES: usize = 500;
/// 系列の上限。
const MAX_SERIES: usize = 30;

/// insert_chart のデータと見た目（位置は呼び出し側）。誤りは利用者向けの文。
pub(super) fn normalize_chart(op: &DocOp) -> Result<Map<String, Value>, String> {
    let DocOp::InsertChart {
        kind,
        categories,
        series,
        title,
        width_cm,
        height_cm,
        legend,
        legend_position,
        colors,
        data_labels,
        y_number_format,
        stacked,
        percent,
        x_title,
        y_title,
        ..
    } = op
    else {
        return Err("insert_chart ではありません".into());
    };
    if *kind == ChartKind::Scatter {
        return Err(
            "Word のグラフは散布図に対応していません（column / bar / line / area / pie / doughnut）"
                .into(),
        );
    }
    if categories.is_empty() || categories.len() > MAX_CATEGORIES {
        return Err(format!(
            "categories（項目）は 1〜{MAX_CATEGORIES} 個にしてください"
        ));
    }
    if series.is_empty() || series.len() > MAX_SERIES {
        return Err(format!("series（系列）は 1〜{MAX_SERIES} 個にしてください"));
    }
    if matches!(kind, ChartKind::Pie | ChartKind::Doughnut) && series.len() != 1 {
        return Err("円・ドーナツのグラフは系列を 1 つにしてください".into());
    }
    for s in series {
        if s.values.len() != categories.len() {
            return Err(format!(
                "系列「{}」の値の数（{}）が項目の数（{}）と合いません",
                s.name,
                s.values.len(),
                categories.len()
            ));
        }
    }
    let look = ChartLook {
        kind: *kind,
        title: title.as_deref(),
        width_cm: *width_cm,
        height_cm: *height_cm,
        legend: *legend,
        legend_position: *legend_position,
        colors: colors.as_deref(),
        data_labels: *data_labels,
        y_number_format: y_number_format.as_deref(),
        stacked: *stacked,
        percent: *percent,
        x_title: x_title.as_deref(),
        y_title: y_title.as_deref(),
    };
    let mut out = look.normalize()?;
    out.insert("categories".into(), json!(categories));
    out.insert(
        "series".into(),
        Value::Array(
            series
                .iter()
                .map(|s| json!({"name": s.name, "values": s.values}))
                .collect(),
        ),
    );
    Ok(out)
}
