// Shiki Office Engine — Calc への書き込み（#725）。
//
// 受け取る op は Rust（office::engine::calc::ops）が検証・正規化した後の形:
// - 座標は 0 始まりの数値（area = [列始, 行始, 列終, 行終]）、シート名は解決済み
// - 数式は Calc の API 文法（区切り ';'・シート参照 $Sheet.A1）に変換済み
// - 色は "#RRGGBB"、長さは 1/100 mm
// ここでは UNO を呼ぶことだけをする。1 回の apply は 1 つの undo 単位で、途中の失敗は丸ごと取り消す。

const EN_US = new I.lang.Locale("en", "US", "");

function rangeOf(op) {
  const sheet = getSheet(op.sheet);
  const [sc, sr, ec, er] = op.area;
  return sheet.getCellRangeByPosition(sc, sr, ec, er);
}

function numberFormatKey(code) {
  const fmts = DOC.getNumberFormats();
  let key = fmts.queryKey(code, EN_US, false);
  if (key === -1) {
    try {
      key = fmts.addNew(code, EN_US);
    } catch (e) {
      fail("invalid_number_format", `表示形式「${code}」を解釈できません`);
    }
  }
  return key;
}

function setCell(cell, v) {
  switch (v.t) {
    case "n":
      cell.setValue(v.v);
      break;
    case "s":
      cell.setString(v.v);
      break;
    case "f":
      cell.setFormula(v.v);
      break;
    case "b":
      cell.setFormula(v.v ? "=TRUE()" : "=FALSE()");
      break;
    case "d":
      cell.setValue(v.v);
      // 表示形式が「標準」のままなら日付として見えるようにする（数値のまま表示されるのを防ぐ）。
      if (cell.getPropertyValue("NumberFormat") === 0) {
        cell.setPropertyValue("NumberFormat", numberFormatKey(v.fmt || "yyyy/mm/dd"));
      }
      break;
    case "clear":
      cell.setString("");
      break;
    default:
      fail("invalid_value", `値の種類が不正です: ${v.t}`);
  }
}

const OPS = {};

OPS.set_values = (op) => {
  const sheet = getSheet(op.sheet);
  const [sc, sr] = op.area;
  let n = 0;
  for (let r = 0; r < op.values.length; r++) {
    const row = op.values[r];
    for (let c = 0; c < row.length; c++) {
      const v = row[c];
      if (v === null) continue;
      STAGE = `${op.sheet}!${colLetters(sc + c)}${sr + r + 1} への書き込み`;
      setCell(sheet.getCellByPosition(sc + c, sr + r), v);
      n++;
    }
  }
  return { cells: n };
};

/**
 * 罫線 1 本。値（線種・幅）は Rust が「Excel の罫線を読み込んだときと同じ値」に変換済み。
 * それ以外の値（例: 中線を 53 で引く）は xlsx へ書き出すと細線に丸められる（実機で確認）。
 */
function borderLine(spec) {
  const line = new I.table.BorderLine2();
  if (!spec || spec.none) {
    line.LineStyle = 0x7fff;
    return line;
  }
  line.Color = hexToColor(spec.color);
  line.LineStyle = spec.style;
  line.LineWidth = spec.width;
  line.OuterLineWidth = spec.outer;
  line.InnerLineWidth = spec.inner;
  line.LineDistance = spec.dist;
  return line;
}

/**
 * 文字色を設定する。既定の文字色がテーマ色（Excel 由来の xlsx は必ずそう）だと、CharColor だけを
 * 変えても複合色（CharComplexColor）にテーマ参照が残り、xlsx への書き出しでテーマ色に戻ってしまう
 * （実機で確認）。先に「未使用」型の複合色を入れてから RGB を入れる。
 */
function setCharColor(target, sheet, color) {
  if (color !== -1) {
    const unused = sheet.getCellByPosition(16383, 1048575).getPropertyValue("CellBackgroundComplexColor");
    if (unused && unused.getType() === 0) target.setPropertyValue("CharComplexColor", unused);
  }
  target.setPropertyValue("CharColor", long(color));
}

function applyBorders(range, b) {
  const tb = range.getPropertyValue("TableBorder2");
  const set = (side, valid, spec) => {
    tb[side] = borderLine(spec);
    tb[valid] = true;
  };
  for (const k of ["TopLine", "BottomLine", "LeftLine", "RightLine", "HorizontalLine", "VerticalLine"]) {
    tb["Is" + k + "Valid"] = false;
  }
  if (b.outline !== undefined) {
    for (const k of ["TopLine", "BottomLine", "LeftLine", "RightLine"]) set(k, "Is" + k + "Valid", b.outline);
  }
  if (b.inner !== undefined) {
    for (const k of ["HorizontalLine", "VerticalLine"]) set(k, "Is" + k + "Valid", b.inner);
  }
  const sides = { top: "TopLine", bottom: "BottomLine", left: "LeftLine", right: "RightLine", inside_h: "HorizontalLine", inside_v: "VerticalLine" };
  for (const [k, side] of Object.entries(sides)) {
    if (b[k] !== undefined) set(side, "Is" + side + "Valid", b[k]);
  }
  tb.IsDistanceValid = false;
  range.setPropertyValue("TableBorder2", tb);
}

const HORI = { general: "STANDARD", left: "LEFT", center: "CENTER", right: "RIGHT", justify: "BLOCK" };
const VERT = { top: 1, center: 2, bottom: 3 };

function applyFormat(range, op, sheet) {
  if (op.number_format !== undefined) {
    STAGE = "表示形式";
    range.setPropertyValue("NumberFormat", numberFormatKey(op.number_format));
  }
  const f = op.font;
  if (f) {
    STAGE = "フォント";
    if (f.name !== undefined) {
      // 和文は Asian 側のフォントで描かれる。両方そろえないと日本語だけ別書体になる。
      range.setPropertyValue("CharFontName", f.name);
      range.setPropertyValue("CharFontNameAsian", f.name);
    }
    if (f.size !== undefined) {
      range.setPropertyValue("CharHeight", f.size);
      range.setPropertyValue("CharHeightAsian", f.size);
    }
    if (f.bold !== undefined) {
      const w = f.bold ? 150 : 100;
      range.setPropertyValue("CharWeight", w);
      range.setPropertyValue("CharWeightAsian", w);
    }
    if (f.italic !== undefined) {
      const p = f.italic ? I.awt.FontSlant.ITALIC : I.awt.FontSlant.NONE;
      range.setPropertyValue("CharPosture", p);
      range.setPropertyValue("CharPostureAsian", p);
    }
    if (f.underline !== undefined) range.setPropertyValue("CharUnderline", short(f.underline ? 1 : 0));
    if (f.strike !== undefined) range.setPropertyValue("CharStrikeout", short(f.strike ? 1 : 0));
    if (f.color !== undefined) setCharColor(range, sheet, hexToColor(f.color));
  }
  if (op.fill !== undefined) {
    STAGE = "塗りつぶし";
    if (op.fill === null) {
      range.setPropertyValue("IsCellBackgroundTransparent", true);
    } else {
      range.setPropertyValue("CellBackColor", long(hexToColor(op.fill)));
    }
  }
  const a = op.align;
  if (a) {
    STAGE = "配置";
    if (a.horizontal !== undefined) {
      range.setPropertyValue("HoriJustify", I.table.CellHoriJustify[HORI[a.horizontal]]);
    }
    if (a.vertical !== undefined) range.setPropertyValue("VertJustify", long(VERT[a.vertical]));
    if (a.wrap !== undefined) range.setPropertyValue("IsTextWrapped", a.wrap);
    if (a.shrink !== undefined) range.setPropertyValue("ShrinkToFit", a.shrink);
    if (a.indent !== undefined) range.setPropertyValue("ParaIndent", short(a.indent));
    if (a.rotate !== undefined) range.setPropertyValue("RotateAngle", long(a.rotate));
  }
  if (op.borders) {
    STAGE = "罫線";
    applyBorders(range, op.borders);
  }
}

OPS.format = (op) => {
  const sheet = getSheet(op.sheet);
  for (const [sc, sr, ec, er] of op.areas) applyFormat(sheet.getCellRangeByPosition(sc, sr, ec, er), op, sheet);
  return {};
};

OPS.merge = (op) => {
  const range = rangeOf(op);
  range.merge(op.merge);
  if (op.merge && op.center) {
    range.setPropertyValue("HoriJustify", I.table.CellHoriJustify.CENTER);
    range.setPropertyValue("VertJustify", long(VERT.center));
  }
  return {};
};

OPS.column_width = (op) => {
  const cols = getSheet(op.sheet).getColumns();
  for (let c = op.from; c <= op.to; c++) {
    const col = cols.getByIndex(c);
    if (op.width === null) col.setPropertyValue("OptimalWidth", true);
    else col.setPropertyValue("Width", long(op.width));
  }
  return {};
};

OPS.row_height = (op) => {
  const rows = getSheet(op.sheet).getRows();
  for (let r = op.from; r <= op.to; r++) {
    const row = rows.getByIndex(r);
    if (op.height === null) row.setPropertyValue("OptimalHeight", true);
    else row.setPropertyValue("Height", long(op.height));
  }
  return {};
};

OPS.hide = (op) => {
  const sheet = getSheet(op.sheet);
  const list = op.axis === "rows" ? sheet.getRows() : sheet.getColumns();
  for (let i = op.from; i <= op.to; i++) list.getByIndex(i).setPropertyValue("IsVisible", !op.hidden);
  return {};
};

OPS.insert = (op) => {
  const sheet = getSheet(op.sheet);
  const list = op.axis === "rows" ? sheet.getRows() : sheet.getColumns();
  list.insertByIndex(op.at, op.count);
  return {};
};

OPS.delete = (op) => {
  const sheet = getSheet(op.sheet);
  const list = op.axis === "rows" ? sheet.getRows() : sheet.getColumns();
  list.removeByIndex(op.at, op.count);
  return {};
};

OPS.clear = (op) => {
  rangeOf(op).clearContents(op.flags | 0);
  return {};
};

OPS.sort = (op) => {
  const range = rangeOf(op);
  const fields = op.keys.map((k) => {
    const f = new I.table.TableSortField();
    f.Field = k.field;
    f.IsAscending = k.ascending;
    f.IsCaseSensitive = false;
    return f;
  });
  const desc = [
    propertyValue("SortFields", new uno.Any(uno.type.sequence(uno.type.struct(I.table.TableSortField)), fields)),
    propertyValue("ContainsHeader", op.has_header),
    propertyValue("IsSortColumns", false),
  ];
  range.sort(desc);
  return {};
};

OPS.autofilter = (op) => {
  const sheet = getSheet(op.sheet);
  op.sheet = sheet.getName();
  const unnamed = DOC.getPropertyValue("UnnamedDatabaseRanges");
  const index = sheet.getRangeAddress().Sheet;
  if (op.enabled) {
    const [sc, sr, ec, er] = op.area;
    const a = new I.table.CellRangeAddress();
    a.Sheet = index;
    a.StartColumn = sc;
    a.StartRow = sr;
    a.EndColumn = ec;
    a.EndRow = er;
    unnamed.setByTable(a);
    unnamed.getByTable(index).setPropertyValue("AutoFilter", true);
  } else if (unnamed.hasByTable(index)) {
    unnamed.getByTable(index).setPropertyValue("AutoFilter", false);
  }
  return {};
};

OPS.add_sheet = (op) => {
  const sheets = DOC.getSheets();
  if (sheets.hasByName(op.name)) fail("sheet_exists", `シート「${op.name}」は既にあります`);
  const pos = op.position === null ? sheets.getCount() : Math.min(op.position, sheets.getCount());
  sheets.insertNewByName(op.name, pos | 0);
  return {};
};

OPS.rename_sheet = (op) => {
  if (DOC.getSheets().hasByName(op.name)) fail("sheet_exists", `シート「${op.name}」は既にあります`);
  getSheet(op.sheet).setName(op.name);
  return {};
};

OPS.delete_sheet = (op) => {
  getSheet(op.sheet);
  if (DOC.getSheets().getCount() <= 1) fail("last_sheet", "最後の 1 枚のシートは削除できません");
  DOC.getSheets().removeByName(op.sheet);
  return {};
};

OPS.move_sheet = (op) => {
  getSheet(op.sheet);
  DOC.getSheets().moveByName(op.sheet, op.position | 0);
  return {};
};

/**
 * 固定枠。`.uno:FreezePanes` は「選択セルの左上で固定／固定中なら解除」を切り替える命令なので、
 * 固定中なら一度解除してから目的のセルを選んで固定する（AI の view の選択を使う・人の選択は動かない）。
 * view の設定なので undo の対象にならない（一括適用が失敗しても固定枠は戻らない）。
 */
OPS.freeze = (op) => {
  const ctl = DOC.getCurrentController();
  const sheet = getSheet(op.sheet);
  ctl.setActiveSheet(sheet);
  const dh = I.frame.DispatchHelper.create(uno.componentContext);
  if (ctl.hasFrozenPanes()) dh.executeDispatch(ctl.getFrame(), ".uno:FreezePanes", "", 0, []);
  if (op.cell) {
    ctl.select(sheet.getCellByPosition(op.cell[0], op.cell[1]));
    dh.executeDispatch(ctl.getFrame(), ".uno:FreezePanes", "", 0, []);
  }
  return { outside_undo: true };
};

OPS.sheet_props = (op) => {
  const sheet = getSheet(op.sheet);
  if (op.tab_color !== undefined) sheet.setPropertyValue("TabColor", long(hexToColor(op.tab_color)));
  if (op.visible !== undefined) sheet.setPropertyValue("IsVisible", op.visible);
  return {};
};

/** 条件付き書式の見た目を表すセルスタイルを用意する（同じ見た目は同じスタイルを使い回す）。 */
function conditionStyle(look, sheet) {
  const name = "Shiki " + [look.font_color || "-", look.fill || "-", look.bold ? "b" : "-"].join(" ");
  const styles = DOC.getStyleFamilies().getByName("CellStyles");
  if (!styles.hasByName(name)) {
    const st = DOC.createInstance("com.sun.star.style.CellStyle");
    styles.insertByName(name, st);
    if (look.font_color) setCharColor(st, sheet, hexToColor(look.font_color));
    if (look.fill) {
      st.setPropertyValue("CellBackColor", long(hexToColor(look.fill)));
      st.setPropertyValue("IsCellBackgroundTransparent", false);
    }
    if (look.bold) {
      st.setPropertyValue("CharWeight", 150);
      st.setPropertyValue("CharWeightAsian", 150);
    }
  }
  return name;
}

OPS.conditional_format = (op) => {
  const sheet = getSheet(op.sheet);
  const style = conditionStyle(op.look || {}, sheet);
  for (const [sc, sr, ec, er] of op.areas) {
    const range = sheet.getCellRangeByPosition(sc, sr, ec, er);
    const entries = range.getPropertyValue("ConditionalFormat");
    const props = [
      propertyValue("Operator", I.sheet.ConditionOperator[op.operator]),
      propertyValue("Formula1", op.formula1 || ""),
      propertyValue("StyleName", style),
    ];
    if (op.formula2 !== undefined && op.formula2 !== null) props.push(propertyValue("Formula2", op.formula2));
    // 相対参照は範囲の左上を基準に解釈される。
    const base = new I.table.CellAddress();
    base.Sheet = sheet.getRangeAddress().Sheet;
    base.Column = sc;
    base.Row = sr;
    props.push(propertyValue("SourcePosition", base));
    entries.addNew(props);
    range.setPropertyValue("ConditionalFormat", entries);
  }
  return { style };
};

OPS.clear_conditional_formats = (op) => {
  const range = rangeOf(op);
  const entries = range.getPropertyValue("ConditionalFormat");
  entries.clear();
  range.setPropertyValue("ConditionalFormat", entries);
  return {};
};

OPS.data_validation = (op) => {
  const sheet = getSheet(op.sheet);
  for (const [sc, sr, ec, er] of op.areas) {
    const range = sheet.getCellRangeByPosition(sc, sr, ec, er);
    const v = range.getPropertyValue("Validation");
    v.setPropertyValue("Type", I.sheet.ValidationType[op.type]);
    v.setPropertyValue("Operator", I.sheet.ConditionOperator[op.operator || "NONE"]);
    v.setPropertyValue("Formula1", op.formula1 || "");
    v.setPropertyValue("Formula2", op.formula2 || "");
    v.setPropertyValue("IgnoreBlankCells", op.allow_blank !== false);
    if (op.type === "LIST") v.setPropertyValue("ShowList", short(op.show_list === false ? 0 : 1));
    if (op.input_message) {
      v.setPropertyValue("ShowInputMessage", true);
      v.setPropertyValue("InputTitle", op.input_title || "");
      v.setPropertyValue("InputMessage", op.input_message);
    }
    v.setPropertyValue("ShowErrorMessage", true);
    v.setPropertyValue("ErrorTitle", op.error_title || "");
    v.setPropertyValue("ErrorMessage", op.error_message || "");
    v.setPropertyValue("ErrorAlertStyle", I.sheet.ValidationAlertStyle[op.error_style || "STOP"]);
    range.setPropertyValue("Validation", v);
  }
  return {};
};

/** Calc の数式中のシート名（英数と _ 以外を含めば引用符で囲む）。 */
function calcSheetRef(name) {
  return /^[A-Za-z_][A-Za-z0-9_]*$/.test(name) ? name : "'" + name.replace(/'/g, "''") + "'";
}

OPS.define_name = (op) => {
  const names = DOC.getPropertyValue("NamedRanges");
  if (names.hasByName(op.name)) names.removeByName(op.name);
  if (op.area !== null) {
    const sheet = getSheet(op.sheet);
    const [sc, sr, ec, er] = op.area;
    const cell = (c, r) => "$" + colLetters(c) + "$" + (r + 1);
    const ref = sc === ec && sr === er ? cell(sc, sr) : cell(sc, sr) + ":" + cell(ec, er);
    const content = "$" + calcSheetRef(sheet.getName()) + "." + ref;
    names.addNewByName(op.name, content, new I.table.CellAddress(), 0);
  }
  return {};
};

const DIAGRAMS = {
  column: "com.sun.star.chart.BarDiagram",
  bar: "com.sun.star.chart.BarDiagram",
  line: "com.sun.star.chart.LineDiagram",
  area: "com.sun.star.chart.AreaDiagram",
  pie: "com.sun.star.chart.PieDiagram",
  doughnut: "com.sun.star.chart.DonutDiagram",
  scatter: "com.sun.star.chart.XYDiagram",
};

OPS.add_chart = (op) => {
  const sheet = getSheet(op.sheet);
  const charts = sheet.getCharts();
  if (charts.hasByName(op.name)) fail("chart_exists", `グラフ「${op.name}」は既にあります`);
  STAGE = "グラフの配置";
  const anchor = sheet.getCellByPosition(op.anchor[0], op.anchor[1]).getPropertyValue("Position");
  const rect = new I.awt.Rectangle();
  rect.X = anchor.X;
  rect.Y = anchor.Y;
  rect.Width = op.size[0];
  rect.Height = op.size[1];
  const ranges = op.ranges.map(([s, sc, sr, ec, er]) => {
    const a = new I.table.CellRangeAddress();
    // データのシート名が無ければ、グラフを置くシートのデータ。
    a.Sheet = (s === null ? sheet : getSheet(s)).getRangeAddress().Sheet;
    a.StartColumn = sc;
    a.StartRow = sr;
    a.EndColumn = ec;
    a.EndRow = er;
    return a;
  });
  charts.addNewByName(op.name, rect, ranges, op.first_row_header, op.first_col_header);
  // 図形の名前にもグラフ名を書く（xlsx へ保存して開き直すとグラフ名は図形の名前から復元される）。
  const dp = sheet.getDrawPage();
  for (let i = 0; i < dp.getCount(); i++) {
    const shape = dp.getByIndex(i);
    try {
      if (shape.getPropertyValue("PersistName") === op.name) shape.setPropertyValue("Name", op.name);
    } catch (e) {
      // グラフ以外の図形。
    }
  }
  STAGE = "グラフの種類と体裁";
  const eo = charts.getByName(op.name).getEmbeddedObject();
  const diagram = eo.createInstance(DIAGRAMS[op.kind]);
  eo.setDiagram(diagram);
  const d = eo.getDiagram();
  if (op.kind === "bar") d.setPropertyValue("Vertical", true);
  if (op.kind === "column") d.setPropertyValue("Vertical", false);
  if (op.stacked) d.setPropertyValue("Stacked", true);
  if (op.percent) d.setPropertyValue("Percent", true);
  if (op.series_in_rows) d.setPropertyValue("DataRowSource", I.chart.ChartDataRowSource.ROWS);
  eo.setPropertyValue("HasMainTitle", !!op.title);
  if (op.title) {
    const t = eo.getTitle();
    t.setPropertyValue("String", op.title);
    t.setPropertyValue("CharHeight", 13);
    t.setPropertyValue("CharWeight", 150);
  }
  eo.setPropertyValue("HasLegend", op.legend_position !== "none");
  STAGE = "グラフの見た目";
  styleChart(eo, d, op);
  if (op.x_title) {
    d.setPropertyValue("HasXAxisTitle", true);
    d.getXAxisTitle().setPropertyValue("String", op.x_title);
  }
  if (op.y_title) {
    d.setPropertyValue("HasYAxisTitle", true);
    d.getYAxisTitle().setPropertyValue("String", op.y_title);
  }
  return {};
};

/** Office 現行版の既定に近い配色（系列の色を指定しないとき）。 */
const CHART_COLORS = ["#4472C4", "#ED7D31", "#A5A5A5", "#FFC000", "#5B9BD5", "#70AD47", "#264478", "#9E480E"];

/**
 * グラフの見た目を整える（LibreOffice の既定は灰色の背景・枠つき凡例で古く見える）。
 * 背景は透明、目盛線は薄い灰色、凡例は枠なし、系列は指定の色（無ければ既定の配色）。
 */
function styleChart(eo, d, op) {
  const NONE_LINE = I.drawing.LineStyle.NONE;
  try {
    eo.getArea().setPropertyValue("LineStyle", NONE_LINE);
  } catch (e) {
    // 外枠を持たない種類。
  }
  try {
    const wall = d.getWall();
    wall.setPropertyValue("FillStyle", I.drawing.FillStyle.NONE);
    wall.setPropertyValue("LineStyle", NONE_LINE);
  } catch (e) {
    // 円・ドーナツは壁を持たない。
  }
  try {
    const grid = d.getYMainGrid();
    grid.setPropertyValue("LineColor", long(0xd9d9d9));
  } catch (e) {
    // 目盛線を持たない種類。
  }
  if (op.legend_position !== "none") {
    const legend = eo.getLegend();
    legend.setPropertyValue("LineStyle", NONE_LINE);
    legend.setPropertyValue("FillStyle", I.drawing.FillStyle.NONE);
    const pos = { bottom: "BOTTOM", right: "RIGHT", top: "TOP", left: "LEFT" }[op.legend_position || "bottom"];
    legend.setPropertyValue("Alignment", I.chart.ChartLegendPosition[pos]);
  }
  if (op.y_number_format) {
    try {
      const axis = d.getYAxis();
      axis.setPropertyValue("LinkNumberFormatToSource", false);
      axis.setPropertyValue("NumberFormat", numberFormatKey(op.y_number_format));
    } catch (e) {
      // 縦軸を持たない種類（円など）。
    }
  }
  // 系列の数（列方向なら列見出しの数）。
  let series = 0;
  try {
    const data = eo.getData();
    series = op.series_in_rows ? data.getRowDescriptions().length : data.getColumnDescriptions().length;
  } catch (e) {
    series = 0;
  }
  const colors = op.colors && op.colors.length ? op.colors : CHART_COLORS;
  const pie = op.kind === "pie" || op.kind === "doughnut";
  if (pie) {
    // 円・ドーナツは 1 系列の各点（項目）に色を付ける。
    let points = 0;
    try {
      const data = eo.getData();
      points = op.series_in_rows ? data.getColumnDescriptions().length : data.getRowDescriptions().length;
    } catch (e) {
      points = 0;
    }
    for (let i = 0; i < points; i++) {
      try {
        d.getDataPointProperties(i, 0).setPropertyValue("FillColor", long(hexToColor(colors[i % colors.length])));
      } catch (e) {
        // 点の数が合わない場合は既定の色のまま。
      }
    }
  }
  for (let i = 0; i < series; i++) {
    const props = d.getDataRowProperties(i);
    const c = long(hexToColor(colors[i % colors.length]));
    if (!pie) {
      try {
        props.setPropertyValue("Color", c);
        props.setPropertyValue("FillColor", c);
        props.setPropertyValue("LineColor", c);
      } catch (e) {
        // 種類により持たないプロパティがある。
      }
      if (op.kind === "line" || op.kind === "scatter") {
        try {
          props.setPropertyValue("LineWidth", long(60));
        } catch (e) {
          // 線の幅を持たない種類。
        }
      }
    }
    if (op.data_labels) {
      try {
        // com.sun.star.chart.ChartDataCaption: VALUE=1, PERCENT=2, TEXT=4。円は「項目名と割合」。
        props.setPropertyValue("DataCaption", long(pie ? 2 | 4 : 1));
      } catch (e) {
        // データラベルを持たない種類。
      }
    }
  }
}

OPS.delete_chart = (op) => {
  const charts = getSheet(op.sheet).getCharts();
  if (!charts.hasByName(op.name)) fail("chart_not_found", `グラフ「${op.name}」がありません`);
  charts.removeByName(op.name);
  return {};
};

OPS.page_setup = (op) => {
  const sheet = getSheet(op.sheet);
  const st = DOC.getStyleFamilies().getByName("PageStyles").getByName(sheet.getPropertyValue("PageStyle"));
  if (op.paper) {
    const [w, h] = op.paper;
    st.setPropertyValue("Width", long(w));
    st.setPropertyValue("Height", long(h));
  }
  if (op.landscape !== undefined) {
    const w = st.getPropertyValue("Width");
    const h = st.getPropertyValue("Height");
    st.setPropertyValue("IsLandscape", op.landscape);
    // 用紙の縦横は寸法の入れ替えで表す（IsLandscape だけでは向きが変わらない）。
    if (op.landscape !== w > h) {
      st.setPropertyValue("Width", long(h));
      st.setPropertyValue("Height", long(w));
    }
  }
  if (op.fit) {
    st.setPropertyValue("ScaleToPagesX", short(op.fit[0]));
    st.setPropertyValue("ScaleToPagesY", short(op.fit[1]));
  }
  if (op.scale !== undefined) st.setPropertyValue("PageScale", short(op.scale));
  if (op.margins) {
    const [t, r, b, l] = op.margins;
    st.setPropertyValue("TopMargin", long(t));
    st.setPropertyValue("RightMargin", long(r));
    st.setPropertyValue("BottomMargin", long(b));
    st.setPropertyValue("LeftMargin", long(l));
  }
  if (op.center_horizontally !== undefined) st.setPropertyValue("CenterHorizontally", op.center_horizontally);
  for (const [key, on, prop] of [["header", "HeaderIsOn", "RightPageHeaderContent"], ["footer", "FooterIsOn", "RightPageFooterContent"]]) {
    const hf = op[key];
    if (hf === undefined) continue;
    if (hf === null) {
      st.setPropertyValue(on, false);
      continue;
    }
    st.setPropertyValue(on, true);
    // 左右ページ共通にする（既定は奇数ページの内容だけを使う）。
    try {
      st.setPropertyValue(key === "header" ? "HeaderIsShared" : "FooterIsShared", true);
    } catch (e) {
      // 古い版は持たない。
    }
    const content = st.getPropertyValue(prop);
    for (const [part, getter] of [["left", "getLeftText"], ["center", "getCenterText"], ["right", "getRightText"]]) {
      const text = content[getter]();
      text.setString("");
      for (const piece of hf[part] || []) {
        const cur = text.createTextCursor();
        cur.gotoEnd(false);
        if (piece.t === "text") {
          text.insertString(cur, piece.v, false);
        } else {
          const kinds = { page: "PageNumber", pages: "PageCount", date: "DateTime", sheet: "SheetName", file: "FileName" };
          const field = DOC.createInstance("com.sun.star.text.TextField." + kinds[piece.t]);
          // 日時フィールドは既定だと時刻になる。日付として出す。
          if (piece.t === "date") field.setPropertyValue("IsDate", true);
          text.insertTextContent(cur, field, false);
        }
      }
    }
    st.setPropertyValue(prop, content);
  }
  if (op.gridlines !== undefined) st.setPropertyValue("PrintGrid", op.gridlines);
  if (op.print_area !== undefined) {
    if (op.print_area === null) {
      sheet.setPrintAreas([]);
    } else {
      const [sc, sr, ec, er] = op.print_area;
      sheet.setPrintAreas([sheet.getCellRangeByPosition(sc, sr, ec, er).getRangeAddress()]);
    }
  }
  if (op.repeat_rows !== undefined) {
    if (op.repeat_rows === null) {
      sheet.setPrintTitleRows(false);
    } else {
      const [from, to] = op.repeat_rows;
      sheet.setTitleRows(sheet.getCellRangeByPosition(0, from, 0, to).getRangeAddress());
      sheet.setPrintTitleRows(true);
    }
  }
  return {};
};

HANDLERS["calc.apply"] = (req) => {
  requireKind("calc");
  const results = [];
  atomically(req.label, () => {
    for (let i = 0; i < req.ops.length; i++) {
      const op = req.ops[i];
      const fn = OPS[op.op];
      STAGE = `${i + 1} 件目（${op.op}）`;
      if (!fn) fail("unknown_op", `未知の操作です: ${op.op}`, { op_index: i });
      try {
        results.push(Object.assign({ op: op.op }, fn(op)));
      } catch (e) {
        const d = describeError(e);
        d.op_index = i;
        throw new EngineError(d.code, d.message, d);
      }
    }
  });
  return { results };
};
