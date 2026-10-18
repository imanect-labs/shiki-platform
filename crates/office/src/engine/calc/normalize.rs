//! Excel 操作の検証と、エンジン（calc_apply.js）が受け取る形への正規化（#725）。
//!
//! ここで落とせる誤り（範囲の書式・色・数式の括弧・大きさの不一致）はすべてここで落とし、
//! どの操作の何が悪いかを LLM が直せる言葉で返す。kit に送るのは検証済みの JSON だけ。

use serde_json::{json, Map, Value};

use std::collections::HashMap;

use super::ops::{
    BorderSpec, CellInput, ClearWhat, CompareOp, Condition, FontSpec, Line, LineStyle, Patch,
    RangeList, SheetOp, Size, SortOrder,
};
use super::units::{align_json, condition_operator, is_valid_name};
pub use super::units::{
    chars_to_mm100, date_serial, mm100_to_chars, mm100_to_pt, normalize_color, pt_to_mm100,
};
use crate::engine::a1::{self, Area};
use crate::engine::formula;

/// 1 回の適用で書けるセル数の上限（kit の 1 スレッドを長く塞がない）。
pub const MAX_CELLS_PER_APPLY: u64 = 20_000;

/// fill_formula で複製する数式の合計文字数の上限（20,000 セル×平均 100 文字）。
const MAX_FILL_FORMULA_CHARS: u64 = 2_000_000;

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
    /// 各エンジン操作の元になった操作（1 始まりの番号と名前）。style_table・fill_formula は
    /// 複数／別名のエンジン操作へ展開されるので、失敗の報告はこれで元の番号へ戻す。
    pub origin: Vec<(usize, &'static str)>,
}

/// 操作の並びを正規化する。`default_sheet` は範囲にシート名が無いときに使う（None なら作業中のシート）。
pub fn normalize(ops: &[SheetOp], default_sheet: Option<&str>) -> Result<Normalized, OpError> {
    let items: Vec<(&SheetOp, Option<&str>)> = ops.iter().map(|op| (op, None)).collect();
    normalize_with_sheets(&items, default_sheet)
}

/// 操作ごとの既定シート（操作に書かれた `sheet`）つきで正規化する（[`super::parse::parse_ops`] の後段）。
pub fn normalize_with_sheets(
    ops: &[(&SheetOp, Option<&str>)],
    default_sheet: Option<&str>,
) -> Result<Normalized, OpError> {
    let mut out = Normalized {
        ops: Vec::with_capacity(ops.len()),
        touched: Vec::new(),
        origin: Vec::with_capacity(ops.len()),
    };
    let mut cells: u64 = 0;
    let mut cx = Cx {
        default_sheet: default_sheet.map(str::to_string),
        explicit_default: default_sheet.is_some(),
        renames: HashMap::new(),
        index: 0,
        op: "",
    };
    for (i, (op, op_sheet)) in ops.iter().enumerate() {
        cx.index = i + 1;
        cx.op = op.name();
        // 操作に書かれた sheet はその操作だけの既定。
        let saved = op_sheet.map(|s| cx.default_sheet.replace(s.to_string()));
        let produced = cx.one(op, &mut out.touched, &mut cells);
        if let Some(prev) = saved {
            cx.default_sheet = prev;
        }
        let mut produced = produced?;
        if cells > MAX_CELLS_PER_APPLY {
            return Err(cx.err(format!(
                "1 回で書けるのは {MAX_CELLS_PER_APPLY} セルまでです。操作を分けてください"
            )));
        }
        out.origin
            .extend(std::iter::repeat_n((i + 1, op.name()), produced.len()));
        out.ops.append(&mut produced);
    }
    Ok(out)
}

/// 表示形式から BCP 47 の言語の指定（`[$-ja-JP]`）を外す（LCID の `[$-411]` は残す）。Calc の API は `[$-ja-JP]` を
/// 解釈できず表示形式ごと拒否する（Gemma 4 の評価）。通貨記号つき（`[$¥-411]`）は残す。
fn strip_locale_tags(code: &str) -> String {
    let mut out = String::with_capacity(code.len());
    let mut rest = code;
    while let Some(i) = rest.find("[$-") {
        let Some(end) = rest[i..].find(']') else {
            break;
        };
        let tag = &rest[i + 3..i + end];
        // LCID（[$-411]・[$-F800]）は Calc が解釈でき、和暦や曜日の言語を決めるので残す。
        let lcid = !tag.is_empty() && tag.chars().all(|c| c.is_ascii_hexdigit());
        out.push_str(&rest[..if lcid { i + end + 1 } else { i }]);
        rest = &rest[i + end + 1..];
    }
    out.push_str(rest);
    out
}

pub(super) struct Cx {
    default_sheet: Option<String>,
    /// 呼び出しが既定シート（sheet）を明示したか。
    explicit_default: bool,
    /// この一括の中で改名したシート（旧名 → 新名）。以降の参照を新名へ読み替える。
    renames: HashMap<String, String>,
    pub(super) index: usize,
    pub(super) op: &'static str,
}

impl Cx {
    pub(super) fn err(&mut self, message: impl Into<String>) -> OpError {
        OpError {
            index: self.index,
            op: self.op,
            message: message.into(),
        }
    }

    /// シート名を、この一括の中の改名を反映した名前にする。
    pub(super) fn resolve(&self, name: &str) -> String {
        let mut n = name.to_string();
        for _ in 0..8 {
            match self.renames.get(&n) {
                Some(next) => n.clone_from(next),
                None => break,
            }
        }
        n
    }

    /// 範囲に書かれたシート名（無ければ既定）を解決した名前（無ければ作業中＝null）。
    pub(super) fn sheet_name(&self, explicit: Option<&str>) -> Option<String> {
        explicit
            .or(self.default_sheet.as_deref())
            .map(|s| self.resolve(s))
    }

    pub(super) fn sheet(&self, explicit: Option<&str>) -> Value {
        self.sheet_name(explicit).map_or(Value::Null, Value::String)
    }

    pub(super) fn area(&mut self, s: &str) -> Result<Area, OpError> {
        a1::parse_area(s).map_err(|e| self.err(e.to_string()))
    }

    /// 1 枚のシートにそろった範囲の並び（シート名・[座標]）。
    pub(super) fn areas(&mut self, list: &RangeList) -> Result<(Value, Vec<Area>), OpError> {
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

    pub(super) fn color(&mut self, c: &str) -> Result<String, OpError> {
        normalize_color(c).ok_or_else(|| self.err(format!("色「{c}」は #RRGGBB で書いてください")))
    }

    pub(super) fn formula(&mut self, f: &str) -> Result<String, OpError> {
        if f.chars().count() > formula::MAX_FORMULA_CHARS {
            return Err(self.err(format!(
                "数式が長すぎます（{} 文字まで）",
                formula::MAX_FORMULA_CHARS
            )));
        }
        if let Some(what) = formula::external_access(f) {
            return Err(self.err(format!(
                "数式「{f}」: {what}は使えません（文書の外のデータを読む関数・外部ファイルへの参照は書けません。\
                 シート内の参照は INDEX / MATCH / OFFSET で書いてください）"
            )));
        }
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
                // 縦 1 列の範囲へ横 1 行で渡した（またはその逆の）値は向きを合わせる。どちらも
                // 1 列・1 行なので取り違えようがなく、弱いモデルがよく間違える形（Gemma 4 の評価）。
                let transposed: Vec<Vec<CellInput>>;
                let values = if values.len() == 1
                    && values[0].len() > 1
                    && a.width() == 1
                    && a.height() == u32::try_from(values[0].len()).unwrap_or(0)
                {
                    transposed = values[0].iter().map(|c| vec![c.clone()]).collect();
                    &transposed
                } else if values.len() > 1
                    && values.iter().all(|r| r.len() == 1)
                    && a.height() == 1
                    && a.width() == u32::try_from(values.len()).unwrap_or(0)
                {
                    transposed = vec![values.iter().map(|r| r[0].clone()).collect()];
                    &transposed
                } else {
                    values
                };
                let rows = u32::try_from(values.len()).unwrap_or(u32::MAX);
                let cols = u32::try_from(values.iter().map(Vec::len).max().unwrap_or(0))
                    .unwrap_or(u32::MAX);
                if rows == 0 || cols == 0 {
                    return Err(self.err("values が空です"));
                }
                // 値が 1 つなら範囲の左上へ書く（「A1:E1 に表題」のように結合する範囲を書いた形）。
                let single = (a.sc == a.ec && a.sr == a.er) || (rows == 1 && cols == 1);
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
                    sheet: self.sheet_name(a.sheet.as_deref()),
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
                // セルごとに数式を複製するので、数式の長さ×セル数で上限を見る（短い入力から
                // 巨大な kit 宛てのメッセージを作らせない）。
                let chars = u64::try_from(f.chars().count()).unwrap_or(u64::MAX);
                if chars.saturating_mul(a.cells()) > MAX_FILL_FORMULA_CHARS {
                    return Err(self.err(format!(
                        "数式×セル数が大きすぎます（合計 {MAX_FILL_FORMULA_CHARS} 文字まで。範囲を分けるか数式を短くしてください）"
                    )));
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
                    sheet: self.sheet_name(a.sheet.as_deref()),
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
                    let stripped = strip_locale_tags(nf);
                    let code = if nf.eq_ignore_ascii_case("general") {
                        "General"
                    } else {
                        stripped.as_str()
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
            SheetOp::StyleTable {
                range,
                theme,
                header_rows,
                total_row,
                total_rows,
                banded,
            } => {
                let a = self.area(range)?;
                touched.push(Touched {
                    sheet: self.sheet_name(a.sheet.as_deref()),
                    area: a.coords(),
                });
                let totals = total_rows.unwrap_or(u32::from(*total_row));
                self.style_table(range, *theme, *header_rows, totals, *banded)?
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
                self.check_sheet_name(name)?;
                let pos = position.map(|p| p.saturating_sub(1));
                // シートを足した後の、シートを書いていない操作はその新しいシートに書く（足したシートを
                // 埋めるつもりで、元の表を上書きするのを防ぐ・Gemma 4 の評価で実際に起きた）。
                // 呼び出しが sheet を明示したときはそれに従う。
                if !self.explicit_default {
                    self.default_sheet = Some(name.clone());
                }
                vec![json!({"op": "add_sheet", "name": name, "position": pos})]
            }
            SheetOp::RenameSheet { sheet, name } => {
                self.check_sheet_name(name)?;
                let from = self.resolve(sheet);
                // 以降の操作（と既定シート）が旧名を書いても新名へ読み替える。
                if self
                    .default_sheet
                    .as_deref()
                    .map(|d| self.resolve(d))
                    .as_deref()
                    == Some(from.as_str())
                {
                    self.default_sheet = Some(name.clone());
                }
                self.renames.insert(from.clone(), name.clone());
                vec![json!({"op": "rename_sheet", "sheet": from, "name": name})]
            }
            SheetOp::DeleteSheet { sheet } => {
                vec![json!({"op": "delete_sheet", "sheet": self.resolve(sheet)})]
            }
            SheetOp::MoveSheet { sheet, position } => {
                if *position == 0 {
                    return Err(self.err("position は 1 始まりです"));
                }
                vec![
                    json!({"op": "move_sheet", "sheet": self.resolve(sheet), "position": position - 1}),
                ]
            }
            SheetOp::SheetProps {
                sheet,
                tab_color,
                visible,
            } => {
                let mut m = Map::new();
                m.insert("op".into(), "sheet_props".into());
                m.insert("sheet".into(), self.resolve(sheet).into());
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
            SheetOp::AddChart { .. } => self.add_chart_op(op)?,
            SheetOp::DeleteChart { sheet, name } => {
                vec![
                    json!({"op": "delete_chart", "sheet": self.sheet(sheet.as_deref()), "name": name}),
                ]
            }
            SheetOp::PageSetup { .. } => self.page_setup_op(op)?,
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
            CellInput::Literal { text: s } if formula::looks_like_formula(s) => {
                return Err(self.err(format!(
                    "{{\"text\": \"{s}\"}} は計算されない文字として入ります。数式なら \"{s}\" と文字列のまま書いてください\
                     （{{\"text\"}} は先頭の = や 0 を文字として残したいときだけ使う）"
                )));
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

    /// 線 1 本。値は「Excel の罫線を LibreOffice が読み込んだときと同じ」にする
    /// （それ以外の幅は xlsx へ書き出すと細線などに丸められる・実機で確認）。
    pub(super) fn line(&mut self, l: &Line) -> Result<Value, OpError> {
        let (style, color) = match l {
            Line::Style(s) => (*s, None),
            Line::Full { style, color } => (*style, color.as_deref()),
        };
        if style == LineStyle::None {
            return Ok(json!({"none": true}));
        }
        let color = match color {
            Some(c) => self.color(c)?,
            None => "#000000".to_string(),
        };
        // (LineStyle, LineWidth, OuterLineWidth, InnerLineWidth, LineDistance)
        let (st, width, outer, inner, dist) = match style {
            LineStyle::Hair => (0, 2, 2, 0, 0),
            LineStyle::Thin => (0, 26, 26, 0, 0),
            LineStyle::Medium => (0, 62, 62, 0, 0),
            LineStyle::Thick => (0, 88, 88, 0, 0),
            LineStyle::Double => (15, 62, 18, 18, 26),
            LineStyle::Dashed => (14, 26, 26, 0, 0),
            LineStyle::Dotted => (1, 26, 26, 0, 0),
            LineStyle::None => unreachable!("上で返している"),
        };
        Ok(
            json!({"style": st, "width": width, "outer": outer, "inner": inner, "dist": dist, "color": color}),
        )
    }

    pub(super) fn literal(&mut self, v: &Value) -> Result<String, OpError> {
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

    pub(super) fn check_sheet_name(&mut self, name: &str) -> Result<(), OpError> {
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

#[cfg(test)]
#[path = "normalize_tests.rs"]
mod tests;
