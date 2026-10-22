//! チャットの図を開いている文書へグラフとして入れる操作の組み立て（#731）。
//!
//! 画面のボタン（人の操作）から来るデータを、AI が書くのと同じ操作（Excel は add_sheet・set_values・
//! style_table・add_chart、Word は insert_chart）へ写す。検証と適用は既存の経路
//! （normalize → エンジン）がそのまま行う。

use serde::Deserialize;
use serde_json::{json, Value};

use super::a1::{cell_name, col_name, quote_sheet};
use super::calc::ops::ChartKind;
use super::calc::units::chart_kind;

/// 入れるグラフ（項目 × 系列の表と見た目）。
#[derive(Debug, Clone, Deserialize, utoipa::ToSchema)]
#[serde(deny_unknown_fields)]
pub struct ChartInsert {
    #[serde(rename = "type")]
    pub kind: ChartKind,
    #[serde(default)]
    pub title: Option<String>,
    /// 項目の列の見出し（Excel の表の左上）。
    #[serde(default)]
    pub label: Option<String>,
    pub categories: Vec<String>,
    pub series: Vec<ChartInsertSeries>,
    /// 値の単位（縦軸の題にする）。
    #[serde(default)]
    pub unit: Option<String>,
    #[serde(default)]
    pub stacked: bool,
    /// 系列（円は項目）の色 `#RRGGBB`。
    #[serde(default)]
    pub colors: Option<Vec<String>>,
}

/// 系列（values は categories と同じ数・欠けた値は null）。
#[derive(Debug, Clone, Deserialize, utoipa::ToSchema)]
#[serde(deny_unknown_fields)]
pub struct ChartInsertSeries {
    pub name: String,
    pub values: Vec<Option<f64>>,
}

impl ChartInsert {
    /// Excel の表に書けない名前を、人向けの言葉で先に断る。数式に見える文字（`=B2`・`=SUM(`）は
    /// 表へ文字として書くと Excel の検証（AI の書き間違いの案内）に当たるため。
    pub fn check_for_sheet(&self) -> Result<(), String> {
        let names = std::iter::once(self.label.as_deref().unwrap_or(""))
            .chain(self.categories.iter().map(String::as_str))
            .chain(self.series.iter().map(|s| s.name.as_str()));
        for n in names {
            if super::formula::looks_like_formula(n) {
                return Err(format!(
                    "項目名・系列名「{n}」は Excel の数式に見えるため、表に書けません"
                ));
            }
        }
        Ok(())
    }
}

/// シート名に使えない文字（Excel の制限）。
const SHEET_FORBIDDEN: &[char] = &['[', ']', '*', '?', ':', '/', '\\'];
/// シート名の長さの上限（Excel は 31 文字。重複時の「 (12)」の分を残す）。
const SHEET_BASE_CHARS: usize = 25;

/// 既存のシートと重ならない、題から作ったシート名。
pub fn sheet_name_for(title: Option<&str>, existing: &[String]) -> String {
    let cleaned: String = title
        .unwrap_or("")
        .chars()
        .map(|c| {
            if SHEET_FORBIDDEN.contains(&c) || c.is_control() {
                '_'
            } else {
                c
            }
        })
        .collect();
    let cleaned = cleaned.trim().trim_matches('\'').trim();
    let base: String = if cleaned.is_empty() {
        "グラフ".into()
    } else {
        cleaned.chars().take(SHEET_BASE_CHARS).collect()
    };
    let taken = |name: &str| existing.iter().any(|s| s.eq_ignore_ascii_case(name));
    if !taken(&base) {
        return base;
    }
    // 既存のシートの数 + 2 までに必ず空きがある。
    (2..=existing.len() + 2)
        .map(|n| format!("{base} ({n})"))
        .find(|name| !taken(name))
        .unwrap_or(base)
}

/// 文字のセル（先頭が = でも数式にしない）。
fn text(s: &str) -> Value {
    json!({ "text": s })
}

/// Excel: 新しいシートに表を書き、整え、その右にグラフを置く操作の並び。
pub fn sheet_ops(c: &ChartInsert, sheet: &str) -> Vec<Value> {
    let mut rows = Vec::with_capacity(c.categories.len() + 1);
    let mut header = vec![text(c.label.as_deref().unwrap_or("項目"))];
    header.extend(c.series.iter().map(|s| text(&s.name)));
    rows.push(Value::Array(header));
    for (r, cat) in c.categories.iter().enumerate() {
        let mut row = vec![text(cat)];
        // 新しいシートなので null（セルを変えない）は空のセルになる。
        row.extend(c.series.iter().map(|s| {
            s.values
                .get(r)
                .copied()
                .flatten()
                .map_or(Value::Null, |v| json!(v))
        }));
        rows.push(Value::Array(row));
    }
    let last_col = u32::try_from(c.series.len()).unwrap_or(u32::MAX);
    let last_row = u32::try_from(c.categories.len()).unwrap_or(u32::MAX);
    let q = quote_sheet(sheet);
    let table = format!("{q}!A1:{}", cell_name(last_col, last_row));
    let mut chart = json!({
        "op": "add_chart", "type": chart_kind(c.kind), "data": table,
        "anchor": format!("{q}!{}2", col_name(last_col + 2)),
        "title": c.title, "y_title": c.unit, "stacked": c.stacked,
    });
    if let Some(colors) = &c.colors {
        chart["colors"] = json!(colors);
    }
    vec![
        json!({"op": "add_sheet", "name": sheet}),
        json!({"op": "set_values", "range": format!("{q}!A1"), "values": rows}),
        json!({"op": "style_table", "range": table, "theme": "blue"}),
        json!({"op": "column_width", "columns": format!("{q}!A:{}", col_name(last_col)), "width": "auto"}),
        chart,
    ]
}

/// Word: 文書の末尾にグラフを入れる操作。
pub fn doc_ops(c: &ChartInsert) -> Vec<Value> {
    let mut op = json!({
        "op": "insert_chart", "at": "end", "type": chart_kind(c.kind), "title": c.title,
        "categories": c.categories,
        "series": c.series.iter().map(|s| json!({"name": s.name, "values": s.values})).collect::<Vec<_>>(),
        "y_title": c.unit, "stacked": c.stacked,
    });
    if let Some(colors) = &c.colors {
        op["colors"] = json!(colors);
    }
    vec![op]
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample() -> ChartInsert {
        serde_json::from_value(json!({
            "type": "column", "title": "支店別: 売上", "label": "四半期",
            "categories": ["Q1", "=Q2売上"], "unit": "百万円",
            "series": [{"name": "東京", "values": [120, null]}, {"name": "大阪", "values": [80, 95]}],
        }))
        .unwrap()
    }

    #[test]
    fn sheet_names_are_cleaned_and_unique() {
        assert_eq!(sheet_name_for(Some("支店別: 売上"), &[]), "支店別_ 売上");
        assert_eq!(sheet_name_for(None, &[]), "グラフ");
        let existing = vec!["グラフ".to_string(), "グラフ (2)".to_string()];
        assert_eq!(sheet_name_for(Some(" "), &existing), "グラフ (3)");
        let long = "あ".repeat(40);
        assert_eq!(sheet_name_for(Some(&long), &[]).chars().count(), 25);
    }

    #[test]
    fn sheet_ops_write_a_table_and_place_the_chart_beside_it() {
        let ops = sheet_ops(&sample(), "支店別_ 売上");
        let names: Vec<_> = ops.iter().map(|o| o["op"].as_str().unwrap()).collect();
        assert_eq!(
            names,
            [
                "add_sheet",
                "set_values",
                "style_table",
                "column_width",
                "add_chart"
            ]
        );
        assert_eq!(ops[1]["values"][0][0], json!({"text": "四半期"}));
        // 先頭が = の項目も文字のまま。欠けた値は空のセル。
        assert_eq!(ops[1]["values"][2][0], json!({"text": "=Q2売上"}));
        assert_eq!(ops[1]["values"][2][1], Value::Null);
        assert_eq!(ops[4]["data"], "'支店別_ 売上'!A1:C3");
        assert_eq!(ops[4]["anchor"], "'支店別_ 売上'!E2");
        assert_eq!(ops[4]["y_title"], "百万円");
        assert_eq!(ops[3]["columns"], "'支店別_ 売上'!A:C");
        // 組み立てた操作は Excel の検証をそのまま通る。
        let parsed = crate::engine::calc::parse::parse_ops(&ops).unwrap();
        let items: Vec<_> = parsed.iter().map(|(op, s)| (op, s.as_deref())).collect();
        crate::engine::calc::normalize::normalize_with_sheets(&items, None).unwrap();
    }

    #[test]
    fn names_that_look_like_formulas_are_refused_for_sheets() {
        let mut c = sample();
        assert!(c.check_for_sheet().is_ok());
        c.series[1].name = "=B2".into();
        assert!(c.check_for_sheet().unwrap_err().contains("「=B2」"));
    }

    #[test]
    fn doc_ops_pass_the_word_validation() {
        let ops = doc_ops(&sample());
        assert_eq!(ops[0]["op"], "insert_chart");
        crate::engine::writer::normalize::normalize_doc_ops(&ops).unwrap();
    }
}
