//! Excel 操作の正規化の続き（グラフ・印刷設定・入力規則）（#725）。
//!
//! `normalize.rs` の [`Cx`] の impl を、1 ファイルの行数上限のために分けたもの。

use serde_json::{json, Map, Value};

use super::normalize::{date_serial, Cx, OpError};
use super::ops::{LegendPosition, Orientation, Patch, SeriesIn, SheetOp, ValidationRule};
use super::units::{absolute, chart_kind, cm_to_mm100, condition_operator, paper_size, trim_float};
use crate::engine::a1;

impl Cx {
    pub(super) fn add_chart_op(&mut self, op: &SheetOp) -> Result<Vec<Value>, OpError> {
        let SheetOp::AddChart {
            sheet,
            name,
            kind,
            data,
            anchor,
            width_cm,
            height_cm,
            title,
            legend,
            legend_position,
            colors,
            data_labels,
            y_number_format,
            series,
            stacked,
            percent,
            x_title,
            y_title,
            first_row_is_header,
            first_column_is_header,
        } = op
        else {
            return Ok(Vec::new());
        };
        let (anchor_sheet, col, row) =
            a1::parse_cell(anchor).map_err(|e| self.err(e.to_string()))?;
        let chart_sheet = self.sheet(anchor_sheet.as_deref().or(sheet.as_deref()));
        let mut ranges = Vec::new();
        for s in data.items() {
            let a = self.area(s)?;
            let data_sheet = match a.sheet.as_deref() {
                Some(n) => Value::String(self.resolve(n)),
                None => chart_sheet.clone(),
            };
            ranges.push(json!([data_sheet, a.sc, a.sr, a.ec, a.er]));
        }
        let w = width_cm.unwrap_or(15.0);
        let h = height_cm.unwrap_or(9.0);
        if !(3.0..=60.0).contains(&w) || !(3.0..=60.0).contains(&h) {
            return Err(self.err("グラフの大きさは 3〜60 cm で指定してください"));
        }
        if name.as_deref().is_some_and(|n| n.trim().is_empty()) {
            return Err(self.err("グラフの name が空です"));
        }
        let mut palette = Vec::new();
        for c in colors.iter().flatten() {
            palette.push(self.color(c)?);
        }
        let legend_pos = match (legend, legend_position) {
            (false, _) | (_, Some(LegendPosition::None)) => "none",
            (_, Some(LegendPosition::Right)) => "right",
            (_, Some(LegendPosition::Top)) => "top",
            (_, Some(LegendPosition::Left)) => "left",
            _ => "bottom",
        };
        Ok(vec![json!({
            "op": "add_chart", "sheet": chart_sheet, "name": name, "kind": chart_kind(*kind),
            "ranges": ranges, "anchor": [col, row], "size": [cm_to_mm100(w), cm_to_mm100(h)],
            "title": title, "legend_position": legend_pos, "colors": palette, "data_labels": data_labels,
            "y_number_format": y_number_format, "series_in_rows": matches!(series, SeriesIn::Rows),
            "stacked": stacked, "percent": percent, "x_title": x_title, "y_title": y_title,
            "first_row_header": first_row_is_header, "first_col_header": first_column_is_header,
        })])
    }

    pub(super) fn page_setup_op(&mut self, op: &SheetOp) -> Result<Vec<Value>, OpError> {
        let SheetOp::PageSetup {
            sheet,
            orientation,
            paper,
            fit_to,
            scale,
            margins_cm,
            center_horizontally,
            print_area,
            repeat_rows,
            gridlines,
            header,
            footer,
        } = op
        else {
            return Ok(Vec::new());
        };
        let mut m = Map::new();
        m.insert("op".into(), "page_setup".into());
        for (key, hf) in [("header", header), ("footer", footer)] {
            if let Some(p) = hf {
                let v = match p {
                    Patch::Set(h) => super::parse::header_footer_json(h),
                    Patch::Clear => Value::Null,
                };
                m.insert(key.into(), v);
            }
        }
        m.insert("sheet".into(), self.sheet(sheet.as_deref()));
        if let Some(o) = orientation {
            m.insert(
                "landscape".into(),
                matches!(o, Orientation::Landscape).into(),
            );
        }
        if let Some(p) = paper {
            m.insert("paper".into(), json!(paper_size(*p)));
        }
        if let Some(f) = fit_to {
            m.insert("fit".into(), json!([f.width, f.height]));
        }
        if let Some(s) = scale {
            if !(10..=400).contains(s) {
                return Err(self.err("scale は 10〜400（%）です"));
            }
            m.insert("scale".into(), (*s).into());
        }
        if let Some(mg) = margins_cm {
            let all = [mg.top, mg.right, mg.bottom, mg.left];
            if all.iter().any(|v| !(0.0..=10.0).contains(v)) {
                return Err(self.err("余白は 0〜10 cm です"));
            }
            m.insert("margins".into(), json!(all.map(cm_to_mm100)));
        }
        if let Some(c) = center_horizontally {
            m.insert("center_horizontally".into(), (*c).into());
        }
        if let Some(pa) = print_area {
            let v = match pa {
                Patch::Set(r) => json!(self.area(r)?.coords()),
                Patch::Clear => Value::Null,
            };
            m.insert("print_area".into(), v);
        }
        if let Some(rr) = repeat_rows {
            let v = match rr {
                Patch::Set(r) => {
                    let (_, from, to) = a1::parse_rows(r).map_err(|e| self.err(e.to_string()))?;
                    json!([from, to])
                }
                Patch::Clear => Value::Null,
            };
            m.insert("repeat_rows".into(), v);
        }
        if let Some(g) = gridlines {
            m.insert("gridlines".into(), (*g).into());
        }
        Ok(vec![Value::Object(m)])
    }

    pub(super) fn validation(
        &mut self,
        rule: &ValidationRule,
    ) -> Result<Map<String, Value>, OpError> {
        let mut m = Map::new();
        let num = |v: f64| -> String { trim_float(v) };
        match rule {
            ValidationRule::List { items, source } => {
                m.insert("type".into(), "LIST".into());
                let f1 = match (items, source) {
                    (Some(items), None) if !items.is_empty() => items
                        .iter()
                        .map(|s| format!("\"{}\"", s.replace('"', "\"\"")))
                        .collect::<Vec<_>>()
                        .join(";"),
                    (None, Some(src)) => {
                        let a = self.area(src)?;
                        let abs = match &a.sheet {
                            Some(s) => format!("={}!{}", a1::quote_sheet(s), absolute(&a)),
                            None => format!("={}", absolute(&a)),
                        };
                        self.formula(&abs)?.trim_start_matches('=').to_string()
                    }
                    _ => return Err(self.err("list には items（候補の並び）か source（候補の範囲）のどちらか一方を指定してください")),
                };
                m.insert("formula1".into(), f1.into());
            }
            ValidationRule::WholeNumber {
                operator,
                value,
                value2,
            }
            | ValidationRule::Decimal {
                operator,
                value,
                value2,
            } => {
                let t = if matches!(rule, ValidationRule::WholeNumber { .. }) {
                    "WHOLE"
                } else {
                    "DECIMAL"
                };
                m.insert("type".into(), t.into());
                m.insert("operator".into(), condition_operator(*operator).into());
                m.insert("formula1".into(), num(*value).into());
                if let Some(v2) = value2 {
                    m.insert("formula2".into(), num(*v2).into());
                }
            }
            ValidationRule::Date {
                operator,
                value,
                value2,
            } => {
                m.insert("type".into(), "DATE".into());
                m.insert("operator".into(), condition_operator(*operator).into());
                let d1 = date_serial(value)
                    .ok_or_else(|| {
                        self.err(format!("日付「{value}」は YYYY-MM-DD で書いてください"))
                    })?
                    .0;
                m.insert("formula1".into(), num(d1).into());
                if let Some(v2) = value2 {
                    let d2 = date_serial(v2)
                        .ok_or_else(|| {
                            self.err(format!("日付「{v2}」は YYYY-MM-DD で書いてください"))
                        })?
                        .0;
                    m.insert("formula2".into(), num(d2).into());
                }
            }
            ValidationRule::TextLength {
                operator,
                value,
                value2,
            } => {
                m.insert("type".into(), "TEXT_LEN".into());
                m.insert("operator".into(), condition_operator(*operator).into());
                m.insert("formula1".into(), value.to_string().into());
                if let Some(v2) = value2 {
                    m.insert("formula2".into(), v2.to_string().into());
                }
            }
            ValidationRule::Custom { formula: f } => {
                m.insert("type".into(), "CUSTOM".into());
                m.insert(
                    "formula1".into(),
                    self.formula(f)?.trim_start_matches('=').to_string().into(),
                );
            }
        }
        Ok(m)
    }
}
