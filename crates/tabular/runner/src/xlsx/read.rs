//! シートを 1 セルずつ読み、上限で止める（D-148・GAP-84）。
//!
//! 値の数・値の文字の合計・表の大きさ（値のある行の数 × 列の幅）は、1 回の集計で読むシートを合わせて数える
//! （[`Budget`]。シートを名前で読めるようにしても、1 回に読む量は 1 枚のシートのときの上限のまま）。列の幅は
//! シートごとに数える。シートの「範囲」の宣言は使わない（離れた 2 つのセルで巨大な表を確保しない）。

use std::collections::HashSet;
use std::io::{Read, Seek};

use calamine::Xlsx;
use tabular::input::{MAX_CELLS, MAX_COLUMNS, MAX_GRID_CELLS, MAX_TEXT_BYTES};

use super::shape::{sheet_of, value_of, Sheet, Value};
use super::{Unreadable, BROKEN};

/// 読んだ量（読んだシートを合わせて数える）。
#[derive(Debug, Default)]
pub struct Budget {
    cells: usize,
    text_bytes: usize,
    grid_cells: usize,
    /// 読み終えたシートの数。
    sheets: usize,
}

impl Budget {
    /// 上限の言い方（2 枚目からは、読むシートを合わせて数えていることを言う）。`single` は 1 枚目のときの言い方。
    fn limit(&self, single: &str, limit: &str) -> String {
        if self.sheets == 0 {
            format!("{single}{limit}まで")
        } else {
            format!("SQL で読むシートを合わせて、{limit}まで")
        }
    }
}

/// シートを読む。値が 1 つも無ければ `None`。
pub fn read_sheet<RS: Read + Seek>(
    book: &mut Xlsx<RS>,
    name: &str,
    budget: &mut Budget,
) -> Result<Option<Sheet>, Unreadable> {
    let mut reader = book
        .worksheet_cells_reader(name)
        .map_err(|_| Unreadable(BROKEN.into()))?;
    let mut cells: Vec<(u32, u32, Value)> = Vec::new();
    let mut rows: HashSet<u32> = HashSet::new();
    let (mut min_col, mut max_col) = (u32::MAX, 0u32);
    let mut grid = 0usize;
    while let Some(cell) = reader.next_cell().map_err(|_| Unreadable(BROKEN.into()))? {
        let Some(value) = value_of(cell.get_value()) else {
            continue;
        };
        if budget.cells >= MAX_CELLS {
            return Err(Unreadable(format!(
                "Excel のシート「{name}」の値が多すぎます（{}）。使う行と列だけを CSV に保存して選んでください。",
                budget.limit("", &format!("{} 個", group_digits(MAX_CELLS)))
            )));
        }
        budget.text_bytes += match &value {
            Value::Text(s) | Value::Date(s, _) => s.len(),
            _ => 0,
        };
        if budget.text_bytes > MAX_TEXT_BYTES {
            return Err(Unreadable(format!(
                "Excel のシート「{name}」の文字が多すぎて読めません（{}）。使う行と列だけを CSV に保存して選んでください。",
                budget.limit("合わせて ", &format!("{} MB ", MAX_TEXT_BYTES >> 20))
            )));
        }
        let (row, col) = cell.get_position();
        min_col = min_col.min(col);
        max_col = max_col.max(col);
        if max_col - min_col >= MAX_COLUMNS {
            return Err(Unreadable(format!(
                "Excel のシート「{name}」の列が多すぎます（{} 列まで）。表から離れた右の列に値が無いか確かめてください。",
                group_digits(MAX_COLUMNS as usize)
            )));
        }
        rows.insert(row);
        // 表に入れるときは、値のある行ごとに幅の分のセルを書く（右端に離れた値が 1 つあるだけで行の数 × 幅になる）。
        grid = rows.len().saturating_mul((max_col - min_col + 1) as usize);
        if budget.grid_cells.saturating_add(grid) > MAX_GRID_CELLS {
            return Err(Unreadable(format!(
                "Excel のシート「{name}」の表が大きすぎます（{}）。表から離れた右の列に値が無いか確かめるか、使う行と列だけを CSV に保存して選んでください。",
                budget.limit("行の数 × 列の数で ", &format!("{} セル", group_digits(MAX_GRID_CELLS)))
            )));
        }
        budget.cells += 1;
        cells.push((row, col, value));
    }
    budget.grid_cells += grid;
    budget.sheets += 1;
    Ok((!cells.is_empty()).then(|| sheet_of(cells)))
}

/// 3 桁ごとにカンマを入れる（1,000,000）。
fn group_digits(n: usize) -> String {
    let s = n.to_string();
    let mut out = String::with_capacity(s.len() + s.len() / 3);
    for (i, ch) in s.chars().enumerate() {
        if i > 0 && (s.len() - i).is_multiple_of(3) {
            out.push(',');
        }
        out.push(ch);
    }
    out
}
