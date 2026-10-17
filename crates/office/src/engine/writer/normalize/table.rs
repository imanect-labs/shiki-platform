//! 表の操作（行の追加・削除・セルの文字と書式・表全体の設定・まとめて整える）の正規化（#725）。
//!
//! セルは Excel と同じ A1 表記（"B2"・"A1:D1"）で受け、0 始まりの数値座標にして渡す。
//! 結合のある表は座標が揃わないことがある（エンジンが見つからないセルを誤りとして返す）。

use serde_json::{json, Map, Value};

use super::{align, mm100, Cx};
use crate::engine::a1;
use crate::engine::calc::normalize::OpError;
use crate::engine::calc::ops::{BorderSpec, Line, LineStyle, Patch, VAlign};
use crate::engine::writer::ops::DocOp;

/// 1 回の操作で触れるセルの上限（表は文書の一部なので小さめ）。
const MAX_CELLS: u64 = 2_000;

impl Cx<'_> {
    pub(super) fn table_op(&mut self, op: &DocOp) -> Result<Vec<Value>, OpError> {
        Ok(match op {
            DocOp::InsertRows {
                table,
                after_row,
                count,
                values,
            } => {
                if *count == 0 || *count > 200 {
                    return Err(self.err("count は 1〜200 行です"));
                }
                let values = match values {
                    Some(v) => Value::Array(self.cell_values(v)?),
                    None => Value::Null,
                };
                vec![
                    json!({"op": "table_insert_rows", "table": self.id(table)?, "after": after_row, "count": count, "values": values}),
                ]
            }
            DocOp::DeleteRows { table, rows } => {
                let (_, start, end) = a1::parse_rows(rows)
                    .map_err(|e| self.err(format!("行「{rows}」: {e}（例 \"3\"・\"3:5\"）")))?;
                vec![
                    json!({"op": "table_delete_rows", "table": self.id(table)?, "start": start, "end": end}),
                ]
            }
            DocOp::SetCells {
                table,
                cell,
                values,
            } => {
                let (sheet, col, row) = a1::parse_cell(cell)
                    .map_err(|e| self.err(format!("セル「{cell}」: {e}（例 \"B2\"）")))?;
                if sheet.is_some() {
                    return Err(self.err("表のセルにシート名は付けません（例 \"B2\"）"));
                }
                let values = self.cell_values(values)?;
                vec![
                    json!({"op": "table_set_cells", "table": self.id(table)?, "col": col, "row": row, "values": values}),
                ]
            }
            DocOp::FormatCells { .. } => vec![self.format_cells(op)?],
            DocOp::TableProps { .. } => vec![self.table_props(op)?],
            DocOp::StyleTable {
                table,
                theme,
                header_rows,
                total_row,
                banded,
                first_column,
            } => {
                let header_rows = header_rows.unwrap_or(1);
                if header_rows > 5 {
                    return Err(self.err("header_rows は 0〜5 です"));
                }
                let (head, band, total, line) = crate::engine::calc::parse::palette(*theme);
                vec![json!({
                    "op": "table_style", "table": self.id(table)?, "header_rows": header_rows,
                    "total_row": total_row, "banded": banded.unwrap_or(true), "first_column": first_column,
                    "colors": {"head": head, "band": band, "total": total, "line": line},
                })]
            }
            _ => unreachable!("表の操作だけが来る"),
        })
    }

    fn format_cells(&mut self, op: &DocOp) -> Result<Value, OpError> {
        let DocOp::FormatCells {
            table,
            range,
            fill,
            font,
            align: a,
            valign,
            border,
        } = op
        else {
            unreachable!("format_cells だけが来る")
        };
        let area = self.cell_area(range)?;
        let mut m = Map::new();
        m.insert("op".into(), "table_format_cells".into());
        m.insert("table".into(), self.id(table)?.into());
        m.insert("area".into(), json!(area));
        if let Some(f) = fill {
            let v = match f {
                Patch::Set(c) => Value::String(self.color(c)?),
                Patch::Clear => Value::Null,
            };
            m.insert("fill".into(), v);
        }
        if let Some(f) = font {
            m.insert("font".into(), self.font(f)?);
        }
        if let Some(a) = a {
            m.insert("align".into(), align(*a).into());
        }
        if let Some(v) = valign {
            let v = match v {
                VAlign::Top => 1,
                VAlign::Center => 2,
                VAlign::Bottom => 3,
            };
            m.insert("valign".into(), v.into());
        }
        if let Some(b) = border {
            m.insert("border".into(), self.cell_borders(b)?);
        }
        if m.len() <= 3 {
            return Err(self.err("変える書式がありません（fill・font・align・valign・border）"));
        }
        Ok(Value::Object(m))
    }

    fn table_props(&mut self, op: &DocOp) -> Result<Value, OpError> {
        let DocOp::TableProps {
            table,
            width_mm,
            columns_mm,
            align: a,
            repeat_header,
            allow_row_split,
            keep_with_next,
        } = op
        else {
            unreachable!("table_props だけが来る")
        };
        let mut m = Map::new();
        m.insert("op".into(), "table_props".into());
        m.insert("table".into(), self.id(table)?.into());
        if let Some(w) = width_mm {
            if !(10.0..=500.0).contains(w) {
                return Err(self.err("width_mm は 10〜500 mm です"));
            }
            m.insert("width".into(), mm100(*w).into());
        }
        if let Some(cols) = columns_mm {
            if cols.is_empty() || cols.iter().any(|c| !(2.0..=500.0).contains(c)) {
                return Err(self.err("columns_mm は各列 2〜500 mm の配列です"));
            }
            let cols: Vec<i64> = cols.iter().map(|c| mm100(*c)).collect();
            m.insert("columns".into(), json!(cols));
        }
        if let Some(a) = a {
            m.insert("align".into(), align(*a).into());
        }
        if let Some(n) = repeat_header {
            m.insert("repeat_header".into(), (*n).into());
        }
        if let Some(v) = allow_row_split {
            m.insert("split".into(), (*v).into());
        }
        if let Some(v) = keep_with_next {
            m.insert("keep".into(), (*v).into());
        }
        if m.len() <= 2 {
            return Err(self.err("変える設定がありません"));
        }
        Ok(Value::Object(m))
    }

    /// セルの値（文字列・数値・null＝変えない）→ 文字列か null の 2 次元配列。
    fn cell_values(&self, rows: &[Vec<Value>]) -> Result<Vec<Value>, OpError> {
        let cells: usize = rows.iter().map(Vec::len).sum();
        if rows.is_empty() || cells as u64 > MAX_CELLS {
            return Err(self.err(format!("values は 1〜{MAX_CELLS} セルの 2 次元配列です")));
        }
        rows.iter()
            .map(|row| {
                row.iter()
                    .map(|v| match v {
                        Value::Null => Ok(Value::Null),
                        Value::String(s) => Ok(Value::String(s.clone())),
                        Value::Number(n) => Ok(Value::String(n.to_string())),
                        Value::Bool(b) => Ok(Value::String(b.to_string())),
                        _ => Err(self.err("セルの値は文字列・数値・null（変えない）です")),
                    })
                    .collect::<Result<Vec<_>, _>>()
                    .map(Value::Array)
            })
            .collect()
    }

    fn cell_area(&self, range: &str) -> Result<[u32; 4], OpError> {
        let a = a1::parse_area(range)
            .map_err(|e| self.err(format!("セルの範囲「{range}」: {e}（例 \"A1:D1\"）")))?;
        if a.sheet.is_some() {
            return Err(self.err("表のセルにシート名は付けません（例 \"A1:D1\"）"));
        }
        if a.cells() > MAX_CELLS {
            return Err(self.err(format!("セルの範囲は {MAX_CELLS} セルまでです")));
        }
        Ok(a.coords())
    }

    fn cell_borders(&self, b: &BorderSpec) -> Result<Value, OpError> {
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
                m.insert(key.into(), self.word_line(l)?);
            }
        }
        if m.is_empty() {
            return Err(self.err("border に線の指定がありません（例 {\"all\": \"thin\"}）"));
        }
        Ok(Value::Object(m))
    }

    /// Word の罫線（BorderLine2 の線種と太さ・1/100 mm）。docx の線の太さ（0.5pt・1pt…）に合わせる。
    fn word_line(&self, l: &Line) -> Result<Value, OpError> {
        let (style, color) = match l {
            Line::Style(s) => (*s, None),
            Line::Full { style, color } => (*style, color.as_deref()),
        };
        let (st, width) = match style {
            LineStyle::None => return Ok(json!({"none": true})),
            LineStyle::Hair => (0, 9),
            LineStyle::Thin => (0, 18),
            LineStyle::Medium => (0, 35),
            LineStyle::Thick => (0, 53),
            LineStyle::Double => (3, 53),
            LineStyle::Dashed => (2, 18),
            LineStyle::Dotted => (1, 18),
        };
        let color = match color {
            Some(c) => self.color(c)?,
            None => "#000000".to_string(),
        };
        Ok(json!({"style": st, "width": width, "color": color}))
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]

    use serde_json::json;

    use crate::engine::writer::normalize::normalize_doc_ops;

    #[test]
    fn table_ops_become_numeric_coordinates() {
        let n = normalize_doc_ops(&[
            json!({"op": "insert_rows", "table": "b5-a3df", "after_row": 5, "values": [["合計", 1341, null]]}),
            json!({"op": "delete_rows", "table": "b5-a3df", "rows": "3:4"}),
            json!({"op": "set_cells", "table": "b5-a3df", "cell": "B2", "values": [["628", "590"]]}),
            json!({"op": "format_cells", "table": "b5-a3df", "range": "B2:D6", "align": "right", "fill": null, "border": {"top": "double"}}),
            json!({"op": "table_props", "table": "b5-a3df", "columns_mm": [40, 30, 30, 30], "align": "center", "repeat_header": 1}),
            json!({"op": "style_table", "table": "b5-a3df", "total_row": true}),
        ])
        .unwrap();
        assert_eq!(n.ops[0]["values"], json!([["合計", "1341", null]]));
        assert_eq!(n.ops[1]["start"], 2);
        assert_eq!(n.ops[1]["end"], 3);
        assert_eq!(
            (n.ops[2]["col"].clone(), n.ops[2]["row"].clone()),
            (json!(1), json!(1))
        );
        assert_eq!(n.ops[3]["area"], json!([1, 1, 3, 5]));
        assert_eq!(n.ops[3]["fill"], json!(null));
        assert_eq!(n.ops[3]["border"]["top"]["style"], 3);
        assert_eq!(n.ops[4]["columns"], json!([4000, 3000, 3000, 3000]));
        assert_eq!(n.ops[5]["colors"]["head"], "#1F4E79");
        assert_eq!(n.block_ids, vec!["b5-a3df"]);
    }

    #[test]
    fn table_ops_reject_sheet_names_and_empty_formats() {
        let e = normalize_doc_ops(&[
            json!({"op": "set_cells", "table": "b5-a3df", "cell": "売上!B2", "values": [["1"]]}),
        ])
        .unwrap_err();
        assert!(e.to_string().contains("シート名"), "{e}");
        let e =
            normalize_doc_ops(&[json!({"op": "format_cells", "table": "b5-a3df", "range": "A1"})])
                .unwrap_err();
        assert!(e.to_string().contains("変える書式"), "{e}");
    }
}
