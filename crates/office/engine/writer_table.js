// Shiki Office Engine — Word の表の操作（#725）。
//
// 行の追加・削除、セルの文字と書式、表全体の設定、まとめて整える（style_table）。
// セルの座標は 0 始まりの (列, 行)。結合のある表は座標が揃わないことがあり、見つからないセルは誤りにする。

function tableOf(resolved, id) {
  const t = resolved[id];
  if (!t || !isTable(t)) fail("not_table", `ブロック ${id} は表ではありません（表のブロック ID を指定してください）`);
  return t;
}

function cellAt(t, col, row) {
  try {
    const c = t.getCellByPosition(col, row);
    if (c) return c;
  } catch (e) {
    // 範囲外か、結合で座標が揃わない。
  }
  fail("no_cell", `セル ${colLetters(col)}${row + 1} がありません（表の大きさか結合を office.read で確かめてください）`);
}

/** 表の行数と、行ごとの列数の最大（結合のある表は行ごとに違う）。 */
function tableShape(t) {
  let cols = 0;
  for (const n of Array.from(t.getCellNames())) {
    const m = /^([A-Za-z]+)\d+$/.exec(n);
    if (!m) continue;
    let c = 0;
    for (const ch of m[1]) c = c * 52 + (ch >= "a" ? ch.charCodeAt(0) - 97 + 26 : ch.charCodeAt(0) - 65) + 1;
    cols = Math.max(cols, c);
  }
  return { rows: t.getRows().getCount(), cols };
}

/** セルの文字全体を覆うカーソル。 */
function cellCursor(cell) {
  const cur = cell.getText().createTextCursor();
  cur.gotoStart(false);
  cur.gotoEnd(true);
  return cur;
}

function cellParagraphs(cell) {
  const out = [];
  const en = cell.getText().createEnumeration();
  while (en.hasMoreElements()) {
    const p = en.nextElement();
    if (!isTable(p)) out.push(p);
  }
  return out;
}

function borderLineW(spec) {
  const line = new I.table.BorderLine2();
  if (!spec || spec.none) {
    line.LineStyle = 0x7fff;
    line.LineWidth = 0;
    return line;
  }
  line.Color = hexToColor(spec.color);
  line.LineStyle = spec.style;
  line.LineWidth = spec.width;
  return line;
}

function writeRow(t, row, startCol, values) {
  for (let c = 0; c < values.length; c++) {
    if (values[c] === null) continue;
    cellAt(t, startCol + c, row).setString(values[c]);
  }
}

WOPS.table_insert_rows = (op, resolved) => {
  const t = tableOf(resolved, op.table);
  const rows = t.getRows();
  const n = rows.getCount();
  if (op.after > n) fail("bad_row", `表は ${n} 行です（after_row は 0〜${n}）`);
  // insertByIndex は index の位置へ入れ、その位置にあった行の書式を写す。末尾へ足すときは最後の行の書式。
  rows.insertByIndex(op.after, op.count);
  if (op.values) {
    for (let r = 0; r < op.values.length && r < op.count; r++) writeRow(t, op.after + r, 0, op.values[r]);
  }
  return { rows: rows.getCount() };
};

WOPS.table_delete_rows = (op, resolved) => {
  const t = tableOf(resolved, op.table);
  const rows = t.getRows();
  const n = rows.getCount();
  if (op.end >= n) fail("bad_row", `表は ${n} 行です`);
  if (op.end - op.start + 1 >= n) fail("delete_all", "表の行をすべて消すことはできません（表ごと消すなら delete）");
  rows.removeByIndex(op.start, op.end - op.start + 1);
  return { rows: rows.getCount() };
};

WOPS.table_set_cells = (op, resolved) => {
  const t = tableOf(resolved, op.table);
  for (let r = 0; r < op.values.length; r++) writeRow(t, op.row + r, op.col, op.values[r]);
  return {};
};

/** 範囲の各セルに、外枠・内側・各辺の罫線を当てる。 */
function applyCellBorders(t, area, b) {
  const [sc, sr, ec, er] = area;
  for (let r = sr; r <= er; r++) {
    for (let c = sc; c <= ec; c++) {
      const cell = cellAt(t, c, r);
      const sides = {
        TopBorder: r === sr ? b.top || b.outline : b.inside_h || b.inner,
        BottomBorder: r === er ? b.bottom || b.outline : b.inside_h || b.inner,
        LeftBorder: c === sc ? b.left || b.outline : b.inside_v || b.inner,
        RightBorder: c === ec ? b.right || b.outline : b.inside_v || b.inner,
      };
      for (const [k, spec] of Object.entries(sides)) {
        if (spec !== undefined) cell.setPropertyValue(k, borderLineW(spec));
      }
    }
  }
}

WOPS.table_format_cells = (op, resolved) => {
  const t = tableOf(resolved, op.table);
  const [sc, sr, ec, er] = op.area;
  for (let r = sr; r <= er; r++) {
    for (let c = sc; c <= ec; c++) {
      const cell = cellAt(t, c, r);
      if (op.fill !== undefined) {
        if (op.fill === null) {
          cell.setPropertyValue("BackTransparent", true);
          cell.setPropertyValue("BackColor", long(-1));
        } else {
          cell.setPropertyValue("BackColor", long(hexToColor(op.fill)));
          cell.setPropertyValue("BackTransparent", false);
        }
      }
      if (op.font) {
        applyFont(cellCursor(cell), op.font);
        for (const p of cellParagraphs(cell)) applyFont(p, op.font);
      }
      if (op.align) {
        for (const p of cellParagraphs(cell)) p.setPropertyValue("ParaAdjust", I.style.ParagraphAdjust[ADJUST[op.align]]);
      }
      if (op.valign !== undefined) cell.setPropertyValue("VertOrient", short(op.valign));
    }
  }
  if (op.border) applyCellBorders(t, op.area, op.border);
  return {};
};

const TABLE_HORI = { left: 3, center: 2, right: 1, justify: 6 };

WOPS.table_props = (op, resolved) => {
  const t = tableOf(resolved, op.table);
  if (op.columns) {
    const sep = t.getPropertyValue("TableColumnSeparators");
    if (Array.from(sep).length !== op.columns.length - 1) {
      fail("bad_columns", `この表の列は ${Array.from(sep).length + 1} 本です（columns_mm の数を合わせてください。結合で列が揃わない表は列幅を変えられません）`);
    }
  }
  if (op.align !== undefined || op.width !== undefined || op.columns) {
    // 幅を決めるには「左揃え＋幅」などの固定配置にする（全幅のままだと幅を持てない）。
    const align = op.align || (op.width !== undefined || op.columns ? "center" : null);
    if (align) t.setPropertyValue("HoriOrient", short(TABLE_HORI[align]));
  }
  const total = op.width !== undefined ? op.width : op.columns ? op.columns.reduce((a, b) => a + b, 0) : null;
  if (total !== null) t.setPropertyValue("Width", long(total));
  if (op.columns) {
    // 区切りは新しい構造体で作り、型付きの sequence で渡す（JS の配列のままだと黙って無視される・実機で確認）。
    const sum = t.getPropertyValue("TableColumnRelativeSum");
    const all = op.columns.reduce((a, b) => a + b, 0);
    let acc = 0;
    const list = [];
    for (let i = 0; i < op.columns.length - 1; i++) {
      acc += op.columns[i];
      const sep = new I.text.TableColumnSeparator();
      sep.Position = Math.round((acc / all) * sum);
      sep.IsVisible = true;
      list.push(sep);
    }
    const type = uno.type.sequence(uno.type.struct(I.text.TableColumnSeparator));
    t.setPropertyValue("TableColumnSeparators", new uno.Any(type, list));
  }
  if (op.repeat_header !== undefined) {
    t.setPropertyValue("RepeatHeadline", op.repeat_header > 0);
    if (op.repeat_header > 0) t.setPropertyValue("HeaderRowCount", long(op.repeat_header));
  }
  if (op.split !== undefined) {
    const rows = t.getRows();
    for (let i = 0; i < rows.getCount(); i++) rows.getByIndex(i).setPropertyValue("IsSplitAllowed", op.split);
  }
  if (op.keep !== undefined) t.setPropertyValue("KeepTogether", op.keep);
  return {};
};

/** 数値に見えるセル（右揃えにする）。 */
function looksNumeric(s) {
  return /^[\s]*[-−▲△+]?[¥$￥]?[\d０-９][\d０-９,，.．]*\s*(%|％|円|万円|百万円|千円|億円|時間|件|人|台|pt)?[\s]*$/.test(s);
}

WOPS.table_style = (op, resolved) => {
  const t = tableOf(resolved, op.table);
  const { rows, cols } = tableShape(t);
  const col = op.colors;
  const thin = { style: 0, width: 9, color: col.line };
  const headLine = { style: 0, width: 35, color: col.head };
  const double = { style: 3, width: 53, color: col.head };
  const totalRow = op.total_row ? rows - 1 : -1;
  if (rows < op.header_rows + (op.total_row ? 1 : 0) + 1) fail("bad_table", "表には見出し・合計行のほかに 1 行以上の本体が要ります");
  for (let r = 0; r < rows; r++) {
    for (let c = 0; c < cols; c++) {
      let cell;
      try {
        cell = t.getCellByPosition(c, r);
      } catch (e) {
        continue; // 結合で無いセル。
      }
      if (!cell) continue;
      const head = r < op.header_rows;
      const total = r === totalRow;
      const band = op.banded && !head && !total && (r - op.header_rows) % 2 === 1;
      const fill = head ? col.head : total ? col.total : band ? col.band : null;
      if (fill === null) {
        cell.setPropertyValue("BackTransparent", true);
        cell.setPropertyValue("BackColor", long(-1));
      } else {
        cell.setPropertyValue("BackColor", long(hexToColor(fill)));
        cell.setPropertyValue("BackTransparent", false);
      }
      cell.setPropertyValue("VertOrient", short(2));
      for (const k of ["TopBorder", "BottomBorder", "LeftBorder", "RightBorder"]) cell.setPropertyValue(k, borderLineW(thin));
      if (head && r === op.header_rows - 1) cell.setPropertyValue("BottomBorder", borderLineW(headLine));
      if (total) cell.setPropertyValue("TopBorder", borderLineW(double));
      const font = head ? { bold: true, color: "#FFFFFF" } : total || (op.first_column && c === 0) ? { bold: true } : null;
      if (font) {
        applyFont(cellCursor(cell), font);
        for (const p of cellParagraphs(cell)) applyFont(p, font);
      }
      const align = head ? "center" : looksNumeric(cell.getString()) ? "right" : null;
      if (align) for (const p of cellParagraphs(cell)) p.setPropertyValue("ParaAdjust", I.style.ParagraphAdjust[ADJUST[align]]);
    }
  }
  if (op.header_rows > 0) {
    t.setPropertyValue("RepeatHeadline", true);
    t.setPropertyValue("HeaderRowCount", long(op.header_rows));
  }
  return {};
};
