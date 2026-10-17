//! Excel 操作の検証と、エンジン（calc_apply.js）が受け取る形への正規化（#725）。
//!
//! ここで落とせる誤り（範囲の書式・色・数式の括弧・大きさの不一致）はすべてここで落とし、
//! どの操作の何が悪いかを LLM が直せる言葉で返す。kit に送るのは検証済みの JSON だけ。

use serde_json::{json, Map, Value};

use super::ops::{
    AlignSpec, BorderSpec, CellInput, ChartKind, ClearWhat, CompareOp, Condition, FontSpec, HAlign,
    Line, LineStyle, Orientation, Paper, Patch, RangeList, SeriesIn, SheetOp, Size, SortOrder,
    VAlign, ValidationRule,
};
use crate::engine::a1::{self, Area};
use crate::engine::formula;

/// 1 回の適用で書けるセル数の上限（kit の 1 スレッドを長く塞がない）。
pub const MAX_CELLS_PER_APPLY: u64 = 20_000;

/// 正規化の誤り（何番目の操作の何が悪いか）。
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("{index} 件目（{op}）: {message}")]
pub struct OpError {
    /// 1 始まり。
    pub index: usize,
    pub op: &'static str,
    pub message: String,
}

/// 読み戻して確かめる範囲。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Touched {
    pub sheet: Option<String>,
    pub area: [u32; 4],
}

/// 正規化の結果。
#[derive(Debug, Clone)]
pub struct Normalized {
    pub ops: Vec<Value>,
    pub touched: Vec<Touched>,
}

/// 操作の並びを正規化する。`default_sheet` は範囲にシート名が無いときに使う（None なら作業中のシート）。
pub fn normalize(ops: &[SheetOp], default_sheet: Option<&str>) -> Result<Normalized, OpError> {
    let mut out = Normalized {
        ops: Vec::with_capacity(ops.len()),
        touched: Vec::new(),
    };
    let mut cells: u64 = 0;
    for (i, op) in ops.iter().enumerate() {
        let mut cx = Cx {
            default_sheet,
            index: i + 1,
            op: op.name(),
        };
        let mut produced = cx.one(op, &mut out.touched, &mut cells)?;
        if cells > MAX_CELLS_PER_APPLY {
            return Err(cx.err(format!(
                "1 回で書けるのは {MAX_CELLS_PER_APPLY} セルまでです。操作を分けてください"
            )));
        }
        out.ops.append(&mut produced);
    }
    Ok(out)
}

struct Cx<'a> {
    default_sheet: Option<&'a str>,
    index: usize,
    op: &'static str,
}

impl Cx<'_> {
    fn err(&mut self, message: impl Into<String>) -> OpError {
        OpError {
            index: self.index,
            op: self.op,
            message: message.into(),
        }
    }

    fn sheet(&self, explicit: Option<&str>) -> Value {
        explicit
            .or(self.default_sheet)
            .map_or(Value::Null, |s| Value::String(s.to_string()))
    }

    fn area(&mut self, s: &str) -> Result<Area, OpError> {
        a1::parse_area(s).map_err(|e| self.err(e.to_string()))
    }

    /// 1 枚のシートにそろった範囲の並び（シート名・[座標]）。
    fn areas(&mut self, list: &RangeList) -> Result<(Value, Vec<Area>), OpError> {
        let mut areas: Vec<Area> = Vec::new();
        for s in list.items() {
            let a = self.area(s)?;
            if areas.first().is_some_and(|first| first.sheet != a.sheet) {
                return Err(self.err("1 つの操作の範囲は同じシートにそろえてください"));
            }
            areas.push(a);
        }
        let Some(first) = areas.first() else {
            return Err(self.err("範囲がありません"));
        };
        let sheet = self.sheet(first.sheet.as_deref());
        Ok((sheet, areas))
    }

    fn color(&mut self, c: &str) -> Result<String, OpError> {
        normalize_color(c).ok_or_else(|| self.err(format!("色「{c}」は #RRGGBB で書いてください")))
    }

    fn formula(&mut self, f: &str) -> Result<String, OpError> {
        formula::excel_to_calc(f).map_err(|e| self.err(format!("数式「{f}」: {e}")))
    }

    #[allow(clippy::too_many_lines)] // 操作ごとの写像を 1 か所に並べる（分けると対応が追いにくい）。
    fn one(
        &mut self,
        op: &SheetOp,
        touched: &mut Vec<Touched>,
        cells: &mut u64,
    ) -> Result<Vec<Value>, OpError> {
        let v = match op {
            SheetOp::SetValues { range, values } => {
                let a = self.area(range)?;
                let rows = u32::try_from(values.len()).unwrap_or(u32::MAX);
                let cols = u32::try_from(values.iter().map(Vec::len).max().unwrap_or(0))
                    .unwrap_or(u32::MAX);
                if rows == 0 || cols == 0 {
                    return Err(self.err("values が空です"));
                }
                let single = a.sc == a.ec && a.sr == a.er;
                if !single && (a.height() != rows || a.width() != cols) {
                    return Err(self.err(format!(
                        "範囲 {} は {}×{} ですが values は {rows}×{cols} です（左上セルだけを指定すると大きさは values から決まります）",
                        a.a1(),
                        a.height(),
                        a.width()
                    )));
                }
                let ec = a.sc + cols - 1;
                let er = a.sr + rows - 1;
                if ec > a1::MAX_COL || er > a1::MAX_ROW {
                    return Err(self.err("シートの端を越えます"));
                }
                let mut grid = Vec::with_capacity(values.len());
                for row in values {
                    let mut out_row = Vec::with_capacity(row.len());
                    for cell in row {
                        out_row.push(self.cell(cell)?);
                    }
                    grid.push(Value::Array(out_row));
                }
                *cells += u64::from(rows) * u64::from(cols);
                let area = [a.sc, a.sr, ec, er];
                touched.push(Touched {
                    sheet: a.sheet.clone().or(self.default_sheet.map(str::to_string)),
                    area,
                });
                vec![
                    json!({"op": "set_values", "sheet": self.sheet(a.sheet.as_deref()), "area": area, "values": grid}),
                ]
            }
            SheetOp::FillFormula { range, formula: f } => {
                let a = self.area(range)?;
                if a.cells() > MAX_CELLS_PER_APPLY {
                    return Err(self.err("範囲が大きすぎます"));
                }
                // 左上セル用の数式を相対参照でずらしてセルごとに書く（Excel のフィルと同じ結果）。
                let mut grid = Vec::with_capacity(a.height() as usize);
                for dr in 0..a.height() {
                    let mut row = Vec::with_capacity(a.width() as usize);
                    for dc in 0..a.width() {
                        let shifted = formula::shift_relative(f, i64::from(dc), i64::from(dr))
                            .map_err(|e| self.err(format!("数式「{f}」: {e}")))?;
                        row.push(json!({"t": "f", "v": self.formula(&shifted)?}));
                    }
                    grid.push(Value::Array(row));
                }
                *cells += a.cells();
                touched.push(Touched {
                    sheet: a.sheet.clone().or(self.default_sheet.map(str::to_string)),
                    area: a.coords(),
                });
                vec![
                    json!({"op": "set_values", "sheet": self.sheet(a.sheet.as_deref()), "area": a.coords(), "values": grid}),
                ]
            }
            SheetOp::Format {
                range,
                number_format,
                font,
                fill,
                align,
                border,
            } => {
                let (sheet, areas) = self.areas(range)?;
                let mut m = Map::new();
                m.insert("op".into(), "format".into());
                m.insert("sheet".into(), sheet);
                m.insert(
                    "areas".into(),
                    json!(areas.iter().map(Area::coords).collect::<Vec<_>>()),
                );
                if let Some(nf) = number_format {
                    if nf.trim().is_empty() {
                        return Err(
                            self.err("number_format が空です（標準に戻すなら \"General\"）")
                        );
                    }
                    let code = if nf.eq_ignore_ascii_case("general") {
                        "General"
                    } else {
                        nf.as_str()
                    };
                    m.insert("number_format".into(), code.into());
                }
                if let Some(f) = font {
                    m.insert("font".into(), self.font(f)?);
                }
                if let Some(fill) = fill {
                    let v = match fill {
                        Patch::Set(c) => Value::String(self.color(c)?),
                        Patch::Clear => Value::Null,
                    };
                    m.insert("fill".into(), v);
                }
                if let Some(a) = align {
                    m.insert("align".into(), align_json(a));
                }
                if let Some(b) = border {
                    m.insert("borders".into(), self.borders(b)?);
                }
                if m.len() <= 3 {
                    return Err(self.err("変える書式がありません（number_format / font / fill / align / border のいずれかを指定）"));
                }
                vec![Value::Object(m)]
            }
            SheetOp::Merge { range, center } => {
                let a = self.area(range)?;
                if a.cells() < 2 {
                    return Err(self.err("結合には 2 セル以上の範囲が要ります"));
                }
                vec![
                    json!({"op": "merge", "sheet": self.sheet(a.sheet.as_deref()), "area": a.coords(), "merge": true, "center": center}),
                ]
            }
            SheetOp::Unmerge { range } => {
                let a = self.area(range)?;
                vec![
                    json!({"op": "merge", "sheet": self.sheet(a.sheet.as_deref()), "area": a.coords(), "merge": false}),
                ]
            }
            SheetOp::ColumnWidth { columns, width } => {
                let (sheet, from, to) =
                    a1::parse_columns(columns).map_err(|e| self.err(e.to_string()))?;
                let w = match width {
                    Size::Keyword(_) => Value::Null,
                    Size::Value(chars) if (0.5..=255.0).contains(chars) => {
                        json!(chars_to_mm100(*chars))
                    }
                    Size::Value(_) => {
                        return Err(self.err("列幅は 0.5〜255（文字数）で指定してください"))
                    }
                };
                vec![
                    json!({"op": "column_width", "sheet": self.sheet(sheet.as_deref()), "from": from, "to": to, "width": w}),
                ]
            }
            SheetOp::RowHeight { rows, height } => {
                let (sheet, from, to) =
                    a1::parse_rows(rows).map_err(|e| self.err(e.to_string()))?;
                let h = match height {
                    Size::Keyword(_) => Value::Null,
                    Size::Value(pt) if (1.0..=409.0).contains(pt) => json!(pt_to_mm100(*pt)),
                    Size::Value(_) => {
                        return Err(self.err("行の高さは 1〜409（pt）で指定してください"))
                    }
                };
                vec![
                    json!({"op": "row_height", "sheet": self.sheet(sheet.as_deref()), "from": from, "to": to, "height": h}),
                ]
            }
            SheetOp::InsertRows {
                sheet,
                before,
                count,
            } => {
                if *before == 0 || *count == 0 || *count > 10_000 {
                    return Err(self.err("before は 1 以上、count は 1〜10000 です"));
                }
                vec![
                    json!({"op": "insert", "axis": "rows", "sheet": self.sheet(sheet.as_deref()), "at": before - 1, "count": count}),
                ]
            }
            SheetOp::DeleteRows { rows } => {
                let (sheet, from, to) =
                    a1::parse_rows(rows).map_err(|e| self.err(e.to_string()))?;
                vec![
                    json!({"op": "delete", "axis": "rows", "sheet": self.sheet(sheet.as_deref()), "at": from, "count": to - from + 1}),
                ]
            }
            SheetOp::InsertColumns {
                sheet,
                before,
                count,
            } => {
                let at = a1::parse_col(before)
                    .ok_or_else(|| self.err(format!("列名「{before}」が不正です")))?;
                if *count == 0 || *count > 1_000 {
                    return Err(self.err("count は 1〜1000 です"));
                }
                vec![
                    json!({"op": "insert", "axis": "columns", "sheet": self.sheet(sheet.as_deref()), "at": at, "count": count}),
                ]
            }
            SheetOp::DeleteColumns { columns } => {
                let (sheet, from, to) =
                    a1::parse_columns(columns).map_err(|e| self.err(e.to_string()))?;
                vec![
                    json!({"op": "delete", "axis": "columns", "sheet": self.sheet(sheet.as_deref()), "at": from, "count": to - from + 1}),
                ]
            }
            SheetOp::HideRows { rows } | SheetOp::ShowRows { rows } => {
                let (sheet, from, to) =
                    a1::parse_rows(rows).map_err(|e| self.err(e.to_string()))?;
                let hidden = matches!(op, SheetOp::HideRows { .. });
                vec![
                    json!({"op": "hide", "axis": "rows", "sheet": self.sheet(sheet.as_deref()), "from": from, "to": to, "hidden": hidden}),
                ]
            }
            SheetOp::HideColumns { columns } | SheetOp::ShowColumns { columns } => {
                let (sheet, from, to) =
                    a1::parse_columns(columns).map_err(|e| self.err(e.to_string()))?;
                let hidden = matches!(op, SheetOp::HideColumns { .. });
                vec![
                    json!({"op": "hide", "axis": "columns", "sheet": self.sheet(sheet.as_deref()), "from": from, "to": to, "hidden": hidden}),
                ]
            }
            SheetOp::Clear { range, what } => {
                let a = self.area(range)?;
                // com.sun.star.sheet.CellFlags: VALUE|DATETIME|STRING|ANNOTATION|FORMULA|HARDATTR|STYLES|OBJECTS|EDITATTR|FORMATTED
                let flags = match what {
                    ClearWhat::Contents => 1 | 2 | 4 | 16 | 256,
                    ClearWhat::Formats => 32 | 64 | 512,
                    ClearWhat::All => 1 | 2 | 4 | 8 | 16 | 32 | 64 | 256 | 512,
                };
                vec![
                    json!({"op": "clear", "sheet": self.sheet(a.sheet.as_deref()), "area": a.coords(), "flags": flags}),
                ]
            }
            SheetOp::Sort {
                range,
                by,
                has_header,
            } => {
                let a = self.area(range)?;
                if by.is_empty() || by.len() > 3 {
                    return Err(self.err("by には 1〜3 個の列を指定してください"));
                }
                let mut keys = Vec::new();
                for k in by {
                    let col = a1::parse_col(&k.column)
                        .ok_or_else(|| self.err(format!("列名「{}」が不正です", k.column)))?;
                    if col < a.sc || col > a.ec {
                        return Err(self.err(format!(
                            "並べ替えの列 {} が範囲 {} の外です",
                            k.column,
                            a.a1()
                        )));
                    }
                    keys.push(json!({"field": col - a.sc, "ascending": matches!(k.order, SortOrder::Asc)}));
                }
                vec![
                    json!({"op": "sort", "sheet": self.sheet(a.sheet.as_deref()), "area": a.coords(), "keys": keys, "has_header": has_header}),
                ]
            }
            SheetOp::Autofilter {
                range,
                sheet,
                enabled,
            } => match (range, enabled) {
                (Some(r), true) => {
                    let a = self.area(r)?;
                    vec![
                        json!({"op": "autofilter", "sheet": self.sheet(a.sheet.as_deref().or(sheet.as_deref())), "area": a.coords(), "enabled": true}),
                    ]
                }
                (None, true) => {
                    return Err(self.err("オートフィルタを付ける範囲（range）を指定してください"))
                }
                (_, false) => vec![
                    json!({"op": "autofilter", "sheet": self.sheet(sheet.as_deref()), "enabled": false}),
                ],
            },
            SheetOp::Freeze { sheet, cell } => {
                let (cell_sheet, pos) = match cell {
                    Some(c) => {
                        let (s, col, row) =
                            a1::parse_cell(c).map_err(|e| self.err(e.to_string()))?;
                        if col == 0 && row == 0 {
                            return Err(self
                                .err("A1 では何も固定されません（例 B2 で 1 行目と A 列を固定）"));
                        }
                        (s, json!([col, row]))
                    }
                    None => (None, Value::Null),
                };
                vec![
                    json!({"op": "freeze", "sheet": self.sheet(cell_sheet.as_deref().or(sheet.as_deref())), "cell": pos}),
                ]
            }
            SheetOp::AddSheet { name, position } => {
                self.sheet_name(name)?;
                let pos = position.map(|p| p.saturating_sub(1));
                vec![json!({"op": "add_sheet", "name": name, "position": pos})]
            }
            SheetOp::RenameSheet { sheet, name } => {
                self.sheet_name(name)?;
                vec![json!({"op": "rename_sheet", "sheet": sheet, "name": name})]
            }
            SheetOp::DeleteSheet { sheet } => vec![json!({"op": "delete_sheet", "sheet": sheet})],
            SheetOp::MoveSheet { sheet, position } => {
                if *position == 0 {
                    return Err(self.err("position は 1 始まりです"));
                }
                vec![json!({"op": "move_sheet", "sheet": sheet, "position": position - 1})]
            }
            SheetOp::SheetProps {
                sheet,
                tab_color,
                visible,
            } => {
                let mut m = Map::new();
                m.insert("op".into(), "sheet_props".into());
                m.insert("sheet".into(), sheet.clone().into());
                if let Some(c) = tab_color {
                    let v = match c {
                        Patch::Set(c) => Value::String(self.color(c)?),
                        Patch::Clear => Value::Null,
                    };
                    m.insert("tab_color".into(), v);
                }
                if let Some(v) = visible {
                    m.insert("visible".into(), (*v).into());
                }
                vec![Value::Object(m)]
            }
            SheetOp::ConditionalFormat { range, when, style } => {
                let (sheet, areas) = self.areas(range)?;
                let (operator, f1, f2) = match when {
                    Condition::Formula { formula: f } => {
                        let calc = self.formula(f)?;
                        ("FORMULA", calc.trim_start_matches('=').to_string(), None)
                    }
                    Condition::Compare {
                        operator,
                        value,
                        value2,
                    } => {
                        let f1 = self.literal(value)?;
                        let f2 = match value2 {
                            Some(v) => Some(self.literal(v)?),
                            None => None,
                        };
                        if matches!(operator, CompareOp::Between | CompareOp::NotBetween)
                            && f2.is_none()
                        {
                            return Err(self.err("between / not_between には value2 が要ります"));
                        }
                        (condition_operator(*operator), f1, f2)
                    }
                };
                if style.font_color.is_none() && style.fill.is_none() && !style.bold {
                    return Err(
                        self.err("style に font_color / fill / bold のいずれかを指定してください")
                    );
                }
                let look = json!({
                    "font_color": style.font_color.as_deref().map(|c| self.color(c)).transpose()?,
                    "fill": style.fill.as_deref().map(|c| self.color(c)).transpose()?,
                    "bold": style.bold,
                });
                vec![
                    json!({"op": "conditional_format", "sheet": sheet, "areas": areas.iter().map(Area::coords).collect::<Vec<_>>(), "operator": operator, "formula1": f1, "formula2": f2, "look": look}),
                ]
            }
            SheetOp::ClearConditionalFormats { range } => {
                let a = self.area(range)?;
                vec![
                    json!({"op": "clear_conditional_formats", "sheet": self.sheet(a.sheet.as_deref()), "area": a.coords()}),
                ]
            }
            SheetOp::DataValidation {
                range,
                rule,
                input_message,
                error_message,
                allow_blank,
            } => {
                let (sheet, areas) = self.areas(range)?;
                let mut m = self.validation(rule)?;
                m.insert("op".into(), "data_validation".into());
                m.insert("sheet".into(), sheet);
                m.insert(
                    "areas".into(),
                    json!(areas.iter().map(Area::coords).collect::<Vec<_>>()),
                );
                m.insert("allow_blank".into(), (*allow_blank).into());
                if let Some(msg) = input_message {
                    m.insert("input_message".into(), msg.clone().into());
                }
                if let Some(msg) = error_message {
                    m.insert("error_message".into(), msg.clone().into());
                }
                vec![Value::Object(m)]
            }
            SheetOp::DefineName { name, range } => {
                if !is_valid_name(name) {
                    return Err(self.err(format!("名前「{name}」は使えません（先頭は文字か _、空白や記号は不可、A1 のようなセル名も不可）")));
                }
                match range {
                    Some(r) => {
                        let a = self.area(r)?;
                        vec![
                            json!({"op": "define_name", "name": name, "sheet": self.sheet(a.sheet.as_deref()), "area": a.coords()}),
                        ]
                    }
                    None => vec![json!({"op": "define_name", "name": name, "area": null})],
                }
            }
            SheetOp::AddChart {
                sheet,
                name,
                kind,
                data,
                anchor,
                width_cm,
                height_cm,
                title,
                legend,
                series,
                stacked,
                percent,
                x_title,
                y_title,
                first_row_is_header,
                first_column_is_header,
            } => {
                let (anchor_sheet, col, row) =
                    a1::parse_cell(anchor).map_err(|e| self.err(e.to_string()))?;
                let chart_sheet = self.sheet(anchor_sheet.as_deref().or(sheet.as_deref()));
                let mut ranges = Vec::new();
                for s in data.items() {
                    let a = self.area(s)?;
                    let data_sheet = match a.sheet.as_deref() {
                        Some(n) => Value::String(n.to_string()),
                        None => chart_sheet.clone(),
                    };
                    ranges.push(json!([data_sheet, a.sc, a.sr, a.ec, a.er]));
                }
                let w = width_cm.unwrap_or(15.0);
                let h = height_cm.unwrap_or(9.0);
                if !(3.0..=60.0).contains(&w) || !(3.0..=60.0).contains(&h) {
                    return Err(self.err("グラフの大きさは 3〜60 cm で指定してください"));
                }
                if name.trim().is_empty() {
                    return Err(self.err("グラフの name が空です"));
                }
                vec![json!({
                    "op": "add_chart", "sheet": chart_sheet, "name": name, "kind": chart_kind(*kind),
                    "ranges": ranges, "anchor": [col, row], "size": [cm_to_mm100(w), cm_to_mm100(h)],
                    "title": title, "legend": legend, "series_in_rows": matches!(series, SeriesIn::Rows),
                    "stacked": stacked, "percent": percent, "x_title": x_title, "y_title": y_title,
                    "first_row_header": first_row_is_header, "first_col_header": first_column_is_header,
                })]
            }
            SheetOp::DeleteChart { sheet, name } => {
                vec![
                    json!({"op": "delete_chart", "sheet": self.sheet(sheet.as_deref()), "name": name}),
                ]
            }
            SheetOp::PageSetup {
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
            } => {
                let mut m = Map::new();
                m.insert("op".into(), "page_setup".into());
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
                            let (_, from, to) =
                                a1::parse_rows(r).map_err(|e| self.err(e.to_string()))?;
                            json!([from, to])
                        }
                        Patch::Clear => Value::Null,
                    };
                    m.insert("repeat_rows".into(), v);
                }
                if let Some(g) = gridlines {
                    m.insert("gridlines".into(), (*g).into());
                }
                vec![Value::Object(m)]
            }
        };
        Ok(v)
    }

    fn cell(&mut self, c: &CellInput) -> Result<Value, OpError> {
        Ok(match c {
            CellInput::Null(()) => Value::Null,
            CellInput::Bool(b) => json!({"t": "b", "v": b}),
            CellInput::Number(n) => {
                if !n.is_finite() {
                    return Err(self.err("数値が不正です（NaN/∞）"));
                }
                json!({"t": "n", "v": n})
            }
            CellInput::Text(s) if s.starts_with('=') && s.len() > 1 => {
                json!({"t": "f", "v": self.formula(s)?})
            }
            CellInput::Text(s) | CellInput::Literal { text: s } => json!({"t": "s", "v": s}),
            CellInput::Date { date } => {
                let (serial, has_time) = date_serial(date).ok_or_else(|| {
                    self.err(format!("日付「{date}」は YYYY-MM-DD（時刻つきは YYYY-MM-DD HH:MM）で書いてください"))
                })?;
                let fmt = if has_time {
                    "yyyy/mm/dd hh:mm"
                } else {
                    "yyyy/mm/dd"
                };
                json!({"t": "d", "v": serial, "fmt": fmt})
            }
        })
    }

    fn font(&mut self, f: &FontSpec) -> Result<Value, OpError> {
        let mut m = Map::new();
        if let Some(n) = &f.name {
            m.insert("name".into(), n.clone().into());
        }
        if let Some(s) = f.size {
            if !(1.0..=409.0).contains(&s) {
                return Err(self.err("文字サイズは 1〜409 pt です"));
            }
            m.insert("size".into(), s.into());
        }
        for (k, v) in [
            ("bold", f.bold),
            ("italic", f.italic),
            ("underline", f.underline),
            ("strike", f.strike),
        ] {
            if let Some(v) = v {
                m.insert(k.into(), v.into());
            }
        }
        if let Some(c) = &f.color {
            let v = match c {
                Patch::Set(c) => Value::String(self.color(c)?),
                Patch::Clear => Value::Null,
            };
            m.insert("color".into(), v);
        }
        Ok(Value::Object(m))
    }

    fn borders(&mut self, b: &BorderSpec) -> Result<Value, OpError> {
        // `all` は外枠と内側の両方の既定。個別の指定（outline / inner）が優先する。
        let sides: [(&str, Option<&Line>); 8] = [
            ("outline", b.outline.as_ref().or(b.all.as_ref())),
            ("inner", b.inner.as_ref().or(b.all.as_ref())),
            ("top", b.top.as_ref()),
            ("bottom", b.bottom.as_ref()),
            ("left", b.left.as_ref()),
            ("right", b.right.as_ref()),
            ("inside_h", b.inside_horizontal.as_ref()),
            ("inside_v", b.inside_vertical.as_ref()),
        ];
        let mut m = Map::new();
        for (key, line) in sides {
            if let Some(l) = line {
                m.insert(key.into(), self.line(l)?);
            }
        }
        if m.is_empty() {
            return Err(self.err("border に線の指定がありません（例 {\"all\": \"thin\"}）"));
        }
        Ok(Value::Object(m))
    }

    fn line(&mut self, l: &Line) -> Result<Value, OpError> {
        let (style, color) = match l {
            Line::Style(s) => (*s, None),
            Line::Full { style, color } => (*style, color.as_deref()),
        };
        if style == LineStyle::None {
            return Ok(json!({"style": "none"}));
        }
        let color = match color {
            Some(c) => self.color(c)?,
            None => "#000000".to_string(),
        };
        let (width, pattern) = match style {
            LineStyle::Hair => (9, "solid"),
            LineStyle::Thin => (26, "solid"),
            LineStyle::Medium => (53, "solid"),
            LineStyle::Thick => (79, "solid"),
            LineStyle::Double => (79, "double"),
            LineStyle::Dashed => (26, "dashed"),
            LineStyle::Dotted => (26, "dotted"),
            LineStyle::None => (0, "none"),
        };
        Ok(json!({"width": width, "pattern": pattern, "color": color}))
    }

    fn literal(&mut self, v: &Value) -> Result<String, OpError> {
        match v {
            Value::Number(n) => Ok(n.to_string()),
            Value::Bool(b) => Ok(if *b {
                "TRUE()".into()
            } else {
                "FALSE()".into()
            }),
            Value::String(s) if s.starts_with('=') => {
                Ok(self.formula(s)?.trim_start_matches('=').to_string())
            }
            Value::String(s) => Ok(format!("\"{}\"", s.replace('"', "\"\""))),
            _ => {
                Err(self.err("比較する値は数値・文字列・真偽値・数式（= で始める）のいずれかです"))
            }
        }
    }

    fn validation(&mut self, rule: &ValidationRule) -> Result<Map<String, Value>, OpError> {
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

    fn sheet_name(&mut self, name: &str) -> Result<(), OpError> {
        let bad = name.trim().is_empty()
            || name.chars().count() > 31
            || name.contains(['[', ']', '*', '?', ':', '/', '\\'])
            || name.starts_with('\'')
            || name.ends_with('\'');
        if bad {
            return Err(self.err(format!(
                "シート名「{name}」は使えません（1〜31 文字・[]*?:/\\ と前後の ' は不可）"
            )));
        }
        Ok(())
    }
}

/// `$A$1:$E$5` の形（名前・入力規則の参照元は絶対参照にする）。
fn absolute(a: &Area) -> String {
    let c = |col: u32, row: u32| format!("${}${}", a1::col_name(col), row + 1);
    if a.sc == a.ec && a.sr == a.er {
        c(a.sc, a.sr)
    } else {
        format!("{}:{}", c(a.sc, a.sr), c(a.ec, a.er))
    }
}

fn align_json(a: &AlignSpec) -> Value {
    let mut m = Map::new();
    if let Some(h) = a.horizontal {
        let v = match h {
            HAlign::General => "general",
            HAlign::Left => "left",
            HAlign::Center => "center",
            HAlign::Right => "right",
            HAlign::Justify => "justify",
        };
        m.insert("horizontal".into(), v.into());
    }
    if let Some(v) = a.vertical {
        let v = match v {
            VAlign::Top => "top",
            VAlign::Center => "center",
            VAlign::Bottom => "bottom",
        };
        m.insert("vertical".into(), v.into());
    }
    if let Some(w) = a.wrap {
        m.insert("wrap".into(), w.into());
    }
    if let Some(s) = a.shrink {
        m.insert("shrink".into(), s.into());
    }
    if let Some(i) = a.indent {
        m.insert("indent".into(), i.into());
    }
    if let Some(r) = a.rotate {
        m.insert("rotate".into(), (r.rem_euclid(360) * 100).into());
    }
    Value::Object(m)
}

fn condition_operator(op: CompareOp) -> &'static str {
    match op {
        CompareOp::Eq => "EQUAL",
        CompareOp::Ne => "NOT_EQUAL",
        CompareOp::Lt => "LESS",
        CompareOp::Le => "LESS_EQUAL",
        CompareOp::Gt => "GREATER",
        CompareOp::Ge => "GREATER_EQUAL",
        CompareOp::Between => "BETWEEN",
        CompareOp::NotBetween => "NOT_BETWEEN",
    }
}

fn chart_kind(k: ChartKind) -> &'static str {
    match k {
        ChartKind::Column => "column",
        ChartKind::Bar => "bar",
        ChartKind::Line => "line",
        ChartKind::Area => "area",
        ChartKind::Pie => "pie",
        ChartKind::Doughnut => "doughnut",
        ChartKind::Scatter => "scatter",
    }
}

/// 用紙の寸法（1/100 mm・縦置き）。B 判は JIS。
fn paper_size(p: Paper) -> [u32; 2] {
    match p {
        Paper::A3 => [29_700, 42_000],
        Paper::A4 => [21_000, 29_700],
        Paper::A5 => [14_800, 21_000],
        Paper::B4 => [25_700, 36_400],
        Paper::B5 => [18_200, 25_700],
        Paper::Letter => [21_590, 27_940],
    }
}

/// `#RRGGBB`（`#` 省略・小文字も可）→ `#RRGGBB`。
pub fn normalize_color(c: &str) -> Option<String> {
    let h = c.trim().trim_start_matches('#');
    (h.len() == 6 && h.chars().all(|c| c.is_ascii_hexdigit()))
        .then(|| format!("#{}", h.to_ascii_uppercase()))
}

/// Excel の列幅（文字数）→ 1/100 mm（Calibri 11pt・最大桁幅 7px・96 dpi の換算）。
pub fn chars_to_mm100(chars: f64) -> u32 {
    let px = (chars * 7.0 + 5.0).round();
    float_to_u32(px / 96.0 * 2540.0)
}

/// 1/100 mm → Excel の列幅（文字数・小数 1 桁）。
pub fn mm100_to_chars(mm100: u32) -> f64 {
    let px = f64::from(mm100) / 2540.0 * 96.0;
    (((px - 5.0) / 7.0).max(0.0) * 10.0).round() / 10.0
}

pub fn pt_to_mm100(pt: f64) -> u32 {
    float_to_u32(pt * 2540.0 / 72.0)
}

pub fn mm100_to_pt(mm100: u32) -> f64 {
    (f64::from(mm100) * 72.0 / 2540.0 * 10.0).round() / 10.0
}

fn cm_to_mm100(cm: f64) -> u32 {
    float_to_u32(cm * 1000.0)
}

fn float_to_u32(v: f64) -> u32 {
    // 値域は呼び出し側で検証済み（列幅・行高・寸法）。
    #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
    let n = v.round().clamp(0.0, f64::from(u32::MAX)) as u32;
    n
}

fn trim_float(v: f64) -> String {
    let s = format!("{v}");
    s.strip_suffix(".0").map_or(s.clone(), str::to_string)
}

/// `YYYY-MM-DD`（`/` 区切りも可・時刻 `HH:MM[:SS]` つき可）→ (シリアル値, 時刻つきか)。
pub fn date_serial(s: &str) -> Option<(f64, bool)> {
    let s = s.trim().replace('/', "-");
    let (date, time) = match s.split_once([' ', 'T']) {
        Some((d, t)) => (d, Some(t)),
        None => (s.as_str(), None),
    };
    let d = chrono::NaiveDate::parse_from_str(date, "%Y-%m-%d").ok()?;
    let base = chrono::NaiveDate::from_ymd_opt(1899, 12, 30)?;
    #[allow(clippy::cast_precision_loss)] // 日数は 10^6 未満。
    let mut serial = (d - base).num_days() as f64;
    if let Some(t) = time {
        let t = chrono::NaiveTime::parse_from_str(t, "%H:%M:%S")
            .or_else(|_| chrono::NaiveTime::parse_from_str(t, "%H:%M"))
            .ok()?;
        serial += f64::from(chrono::Timelike::num_seconds_from_midnight(&t)) / 86_400.0;
    }
    Some((serial, time.is_some()))
}

/// 定義できる名前か（セル参照と紛れる名前・空白や記号を含む名前は不可）。
fn is_valid_name(name: &str) -> bool {
    let first_ok = name
        .chars()
        .next()
        .is_some_and(|c| c.is_alphabetic() || c == '_');
    let rest_ok = name
        .chars()
        .all(|c| c.is_alphanumeric() || c == '_' || c == '.');
    let looks_like_cell = a1::parse_area(name).is_ok();
    first_ok && rest_ok && !looks_like_cell && name.chars().count() <= 255
}

#[cfg(test)]
#[path = "normalize_tests.rs"]
mod tests;
