// Shiki Office Engine — Calc の読み取り（#725）。
//
// 生データを返すことに徹する。A1 表記の解釈・既定値との比較・LLM 向けの整形は Rust 側
// （office::engine::calc::view）が持つ。座標はすべて 0 始まり（列・行）。

function colLetters(c) {
  let s = "";
  c += 1;
  while (c > 0) {
    const m = (c - 1) % 26;
    s = String.fromCharCode(65 + m) + s;
    c = Math.floor((c - 1) / 26);
  }
  return s;
}

function sheetNames() {
  return Array.from(DOC.getSheets().getElementNames());
}

/** シートを名前で引く。null は作業中（アクティブ）のシート。 */
function getSheet(name) {
  const sheets = DOC.getSheets();
  if (name === null || name === undefined) {
    try {
      return DOC.getCurrentController().getActiveSheet();
    } catch (e) {
      return sheets.getByIndex(0);
    }
  }
  if (!sheets.hasByName(name)) {
    fail("sheet_not_found", `シート「${name}」がありません`, { sheets: sheetNames() });
  }
  return sheets.getByName(name);
}

function addr(a) {
  return [a.StartColumn, a.StartRow, a.EndColumn, a.EndRow];
}

/** 使用範囲（値・書式のある最小矩形）。空のシートは null。 */
function usedArea(sheet) {
  const cur = sheet.createCursor();
  cur.gotoStartOfUsedArea(false);
  const s = cur.getRangeAddress();
  cur.gotoEndOfUsedArea(false);
  const e = cur.getRangeAddress();
  const area = [s.StartColumn, s.StartRow, e.EndColumn, e.EndRow];
  if (area[0] === area[2] && area[1] === area[3]) {
    const cell = sheet.getCellByPosition(area[0], area[1]);
    if (enumName(cell.getType()) === "EMPTY") return null;
  }
  return area;
}

/** 座標（1/100 mm）から、そのセル位置を含む列・行を探す（グラフのアンカー表示用）。 */
function cellAtPosition(sheet, x, y) {
  const cols = sheet.getColumns();
  const rows = sheet.getRows();
  let c = 0;
  let acc = 0;
  while (c < 16384) {
    const w = cols.getByIndex(c).getPropertyValue("Width");
    // 位置は 1/100 mm の丸めで数単位ずれる。境界ちょうどは次の列とみなす。
    if (acc + w > x + 10) break;
    acc += w;
    c++;
  }
  let r = 0;
  acc = 0;
  while (r < 1048576) {
    const h = rows.getByIndex(r).getPropertyValue("Height");
    if (acc + h > y + 10) break;
    acc += h;
    r++;
  }
  return [c, r];
}

function chartsOf(sheet) {
  const out = [];
  const charts = sheet.getCharts();
  const names = Array.from(charts.getElementNames());
  // 位置は図形（OLE2Shape）から取る。PersistName がグラフ名と一致する。
  const pos = {};
  const dp = sheet.getDrawPage();
  for (let i = 0; i < dp.getCount(); i++) {
    const shape = dp.getByIndex(i);
    try {
      const name = shape.getPropertyValue("PersistName");
      const p = shape.getPosition();
      const z = shape.getSize();
      pos[name] = [p.X, p.Y, z.Width, z.Height];
    } catch (e) {
      // グラフ以外の図形（画像など）は PersistName を持たない。
    }
  }
  for (const name of names) {
    STAGE = `グラフ「${name}」の読み取り`;
    const ch = charts.getByName(name);
    const eo = ch.getEmbeddedObject();
    const sheets = DOC.getSheets();
    const item = {
      name,
      ranges: Array.from(ch.getRanges()).map((r) => [sheets.getByIndex(r.Sheet).getName()].concat(addr(r))),
    };
    try {
      const diagram = eo.getDiagram();
      item.diagram = enumName(diagram.getDiagramType());
      for (const p of ["Vertical", "Stacked", "Percent", "Dim3D"]) {
        try {
          item[p] = diagram.getPropertyValue(p);
        } catch (e) {
          // 図の種類によって持たないプロパティがある。
        }
      }
    } catch (e) {
      item.diagram = null;
    }
    try {
      item.title = eo.getPropertyValue("HasMainTitle") ? eo.getTitle().getPropertyValue("String") : null;
      item.legend = eo.getPropertyValue("HasLegend");
    } catch (e) {
      // タイトル・凡例を持たない種類。
    }
    const rect = pos[name];
    if (rect) {
      item.rect_mm100 = rect;
      item.anchor = cellAtPosition(sheet, rect[0], rect[1]);
      item.anchor_end = cellAtPosition(sheet, rect[0] + rect[2], rect[1] + rect[3]);
    }
    out.push(item);
  }
  return out;
}

function conditionalFormatsOf(sheet, limit) {
  const out = [];
  const list = sheet.getPropertyValue("ConditionalFormats").getConditionalFormats();
  for (const f of list) {
    if (out.length >= limit) break;
    const item = { ranges: Array.from(f.getPropertyValue("Range").getRangeAddresses()).map(addr), entries: [] };
    for (let i = 0; i < f.getCount(); i++) {
      const en = f.getByIndex(i);
      const e = { type: en.getType() };
      for (const p of ["Operator", "Formula1", "Formula2", "StyleName", "Color", "DateType"]) {
        try {
          const v = en.getPropertyValue(p);
          e[p] = typeof v === "object" && v !== null ? enumName(v) : v;
        } catch (x) {
          // 種類ごとに持つプロパティが違う。
        }
      }
      if (e.StyleName) e.style = cellStyleSummary(e.StyleName);
      item.entries.push(e);
    }
    out.push(item);
  }
  return out;
}

function cellStyleSummary(name) {
  try {
    const st = DOC.getStyleFamilies().getByName("CellStyles").getByName(name);
    return {
      CharColor: st.getPropertyValue("CharColor"),
      CellBackColor: st.getPropertyValue("CellBackColor"),
      CharWeight: st.getPropertyValue("CharWeight"),
    };
  } catch (e) {
    return null;
  }
}

function pageSetupOf(sheet) {
  const out = {};
  try {
    const st = DOC.getStyleFamilies().getByName("PageStyles").getByName(sheet.getPropertyValue("PageStyle"));
    for (const p of [
      "IsLandscape", "Width", "Height", "ScaleToPagesX", "ScaleToPagesY", "PageScale",
      "LeftMargin", "RightMargin", "TopMargin", "BottomMargin", "CenterHorizontally",
      "CenterVertically", "PrintGrid", "HeaderIsOn", "FooterIsOn",
    ]) {
      out[p] = st.getPropertyValue(p);
    }
  } catch (e) {
    // ページスタイルが読めなくても本体の読み取りは続ける。
  }
  try {
    out.print_areas = Array.from(sheet.getPrintAreas()).map(addr);
    out.title_rows = sheet.getPrintTitleRows() ? addr(sheet.getTitleRows()) : null;
  } catch (e) {
    // 印刷範囲を持たない。
  }
  return out;
}

function freezeOf(sheet) {
  try {
    const ctl = DOC.getCurrentController();
    if (ctl.getActiveSheet().getName() !== sheet.getName()) return null;
    if (!ctl.hasFrozenPanes()) return null;
    return [ctl.getSplitColumn(), ctl.getSplitRow()];
  } catch (e) {
    return null;
  }
}

function sheetSummary(sheet, index) {
  STAGE = `シート「${sheet.getName()}」の概要`;
  const item = {
    name: sheet.getName(),
    index,
    used: usedArea(sheet),
    visible: sheet.getPropertyValue("IsVisible"),
    tab_color: sheet.getPropertyValue("TabColor"),
    charts: chartsOf(sheet),
    conditional_format_count: sheet.getPropertyValue("ConditionalFormats").getLength(),
    freeze: freezeOf(sheet),
  };
  return item;
}

function namedRanges() {
  const out = [];
  const names = DOC.getPropertyValue("NamedRanges");
  for (const n of Array.from(names.getElementNames())) {
    try {
      out.push({ name: n, content: names.getByName(n).getContent() });
    } catch (e) {
      out.push({ name: n, content: null });
    }
  }
  return out;
}

/** シート名だけ（グラフの挿入で空いている名前を選ぶため・#731）。概要のような各シートの読み取りはしない。 */
HANDLERS["calc.sheet_names"] = () => {
  requireKind("calc");
  return { sheets: sheetNames() };
};

HANDLERS["calc.overview"] = () => {
  requireKind("calc");
  const sheets = DOC.getSheets();
  const out = { sheets: [], names: namedRanges(), active: null };
  for (let i = 0; i < sheets.getCount(); i++) out.sheets.push(sheetSummary(sheets.getByIndex(i), i));
  try {
    out.active = DOC.getCurrentController().getActiveSheet().getName();
  } catch (e) {
    out.active = null;
  }
  return out;
};

/** 書式として読むプロパティ（Rust 側が既定値と比べて要約する）。 */
const FORMAT_PROPS = [
  "CharFontName", "CharFontNameAsian", "CharHeight", "CharWeight", "CharPosture", "CharUnderline",
  "CharStrikeout", "CharColor", "CellBackColor", "IsCellBackgroundTransparent", "HoriJustify",
  "VertJustify", "IsTextWrapped", "ShrinkToFit", "RotateAngle", "ParaIndent", "CellStyle",
];
const BORDER_PROPS = ["TopBorder2", "BottomBorder2", "LeftBorder2", "RightBorder2"];

function readFormatGroup(ranges) {
  const props = {};
  for (const p of FORMAT_PROPS) {
    try {
      const v = ranges.getPropertyValue(p);
      props[p] = typeof v === "object" && v !== null ? enumName(v) : v;
    } catch (e) {
      // 一部のプロパティは範囲の集合からは読めない。
    }
  }
  try {
    const key = ranges.getPropertyValue("NumberFormat");
    props.NumberFormat = DOC.getNumberFormats().getByKey(key).getPropertyValue("FormatString");
  } catch (e) {
    props.NumberFormat = null;
  }
  for (const p of BORDER_PROPS) {
    try {
      const b = ranges.getPropertyValue(p);
      props[p] = { w: b.OuterLineWidth + b.InnerLineWidth + b.LineDistance, style: b.LineStyle, color: b.Color };
    } catch (e) {
      // 罫線なし。
    }
  }
  try {
    const v = ranges.getPropertyValue("Validation");
    const type = enumName(v.getPropertyValue("Type"));
    if (type !== "ANY") {
      props.Validation = {
        type,
        operator: enumName(v.getPropertyValue("Operator")),
        formula1: v.getPropertyValue("Formula1"),
        formula2: v.getPropertyValue("Formula2"),
        show_list: v.getPropertyValue("ShowList"),
        ignore_blank: v.getPropertyValue("IgnoreBlankCells"),
        error_message: v.getPropertyValue("ShowErrorMessage") ? v.getPropertyValue("ErrorMessage") : null,
      };
    }
  } catch (e) {
    // 入力規則なし。
  }
  return props;
}

/**
 * 矩形の中身を読む。req: { sheet, area: [sc, sr, ec, er], max_cells, formats, merges }
 * area は Rust が使用範囲と上限に合わせて切り詰め済み。
 */
HANDLERS["calc.read_range"] = (req) => {
  requireKind("calc");
  const sheet = getSheet(req.sheet);
  const used = usedArea(sheet);
  // 要求範囲を使用範囲に切り詰める（空の大範囲を総なめしない）。
  if (used === null) return { sheet: sheet.getName(), area: null, cells: [], truncated: false };
  const sc = Math.max(req.area[0], used[0]);
  const sr = Math.max(req.area[1], used[1]);
  const ec = Math.min(req.area[2], used[2]);
  let er = Math.min(req.area[3], used[3]);
  if (sc > ec || sr > er) return { sheet: sheet.getName(), area: null, cells: [], truncated: false, used };
  // 走査する行も上限で絞る（返すセル数だけでなく、数式の配列・書式・結合・列幅の走査量を抑える。
  // 大きな使用範囲に列全体を指定しても kit を長く止めない）。
  const width = ec - sc + 1;
  const maxRows = Math.max(1, Math.floor(Math.max(req.max_cells * 4, 2000) / width));
  let scanCapped = false;
  if (er - sr + 1 > maxRows) {
    er = sr + maxRows - 1;
    scanCapped = true;
  }
  const out = { sheet: sheet.getName(), area: [sc, sr, ec, er], used, cells: [], truncated: scanCapped };
  STAGE = "セルの読み取り";
  const range = sheet.getCellRangeByPosition(sc, sr, ec, er);
  const formulas = range.getFormulaArray();
  let count = 0;
  for (let r = sr; r <= er; r++) {
    for (let c = sc; c <= ec; c++) {
      const f = formulas[r - sr][c - sc];
      if (f === "") continue;
      if (count >= req.max_cells) {
        out.truncated = true;
        break;
      }
      const cell = sheet.getCellByPosition(c, r);
      const type = enumName(cell.getType());
      const item = [c, r, type === "FORMULA" ? "f" : type === "VALUE" ? "n" : "s", null, cell.getString()];
      if (type === "FORMULA") {
        item[3] = f;
        const err = cell.getError();
        if (err !== 0) item.push(err);
      } else if (type === "VALUE") {
        item[3] = cell.getValue();
      }
      out.cells.push(item);
      count++;
    }
    if (out.truncated) break;
  }
  if (req.formats) {
    STAGE = "書式の読み取り";
    // 比較の基準: 使用範囲の外（右下の隣）の空セル＝文書の既定の書式。
    const dc = Math.min(used[2] + 1, 16383);
    const dr = Math.min(used[3] + 1, 1048575);
    out.default_format = readFormatGroup(sheet.getCellRangeByPosition(dc, dr, dc, dr));
    out.formats = [];
    const groups = range.getUniqueCellFormatRanges();
    for (let i = 0; i < groups.getCount(); i++) {
      const g = groups.getByIndex(i);
      out.formats.push({ ranges: Array.from(g.getRangeAddresses()).map(addr), props: readFormatGroup(g) });
    }
  }
  if (req.merges) {
    STAGE = "結合セルの読み取り";
    out.merges = [];
    for (let r = sr; r <= er; r++) {
      for (let c = sc; c <= ec; c++) {
        if (!sheet.getCellRangeByPosition(c, r, c, r).getIsMerged()) continue;
        const cur = sheet.createCursorByRange(sheet.getCellRangeByPosition(c, r, c, r));
        cur.collapseToMergedArea();
        out.merges.push(addr(cur.getRangeAddress()));
      }
    }
  }
  STAGE = "列幅・行高の読み取り";
  out.columns = [];
  const cols = sheet.getColumns();
  for (let c = sc; c <= ec; c++) {
    const col = cols.getByIndex(c);
    out.columns.push([c, col.getPropertyValue("Width"), col.getPropertyValue("IsVisible")]);
  }
  out.rows = [];
  const rows = sheet.getRows();
  for (let r = sr; r <= er; r++) {
    const row = rows.getByIndex(r);
    const optimal = row.getPropertyValue("OptimalHeight");
    const visible = row.getPropertyValue("IsVisible");
    if (!optimal || !visible) out.rows.push([r, row.getPropertyValue("Height"), visible]);
  }
  if (req.sheet_features) {
    STAGE = "シート全体の付帯情報";
    out.conditional_formats = conditionalFormatsOf(sheet, 50);
    out.charts = chartsOf(sheet);
    out.page = pageSetupOf(sheet);
    out.freeze = freezeOf(sheet);
  }
  return out;
};
