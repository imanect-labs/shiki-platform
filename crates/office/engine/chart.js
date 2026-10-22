// Shiki Office Engine — グラフの種類と見た目（Calc と Writer で共有・#725 / #731）。
//
// Calc のグラフ（セル範囲のデータ）も Writer の埋め込みグラフ（内部データ）も、中身は同じ
// chart の文書（旧 API の XChartDocument）なので、種類・題・凡例・配色をここで一度だけ決める。
// 受け取る op は Rust が正規化した形（kind・title・legend_position・colors・stacked・percent・
// data_labels・y_number_format・series_in_rows・x_title・y_title）。

const DIAGRAMS = {
  column: "com.sun.star.chart.BarDiagram",
  bar: "com.sun.star.chart.BarDiagram",
  line: "com.sun.star.chart.LineDiagram",
  area: "com.sun.star.chart.AreaDiagram",
  pie: "com.sun.star.chart.PieDiagram",
  doughnut: "com.sun.star.chart.DonutDiagram",
  scatter: "com.sun.star.chart.XYDiagram",
};

/** 表示形式の文字列 → その文書（`fmts` は XNumberFormats）のキー。無ければ足す。 */
function formatKeyIn(fmts, code) {
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

/**
 * データを入れ終えたグラフ（eo）に、種類・題・凡例・軸の題・見た目を付ける。
 * `fmts` は縦軸の表示形式を登録する先（Calc は表計算の文書、Writer はグラフ自身）。
 */
function shapeChart(eo, op, fmts) {
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
    t.setPropertyValue("CharHeight", float(13));
    t.setPropertyValue("CharWeight", float(150));
  }
  eo.setPropertyValue("HasLegend", op.legend_position !== "none");
  STAGE = "グラフの見た目";
  styleChart(eo, d, op, fmts);
  // 円・ドーナツは軸を持たないので、軸の題（単位つきの円の図の単位など）は付けない。
  // 26.04.5 では付けても落ちないが、見えない題を文書に残さない。
  const pie = op.kind === "pie" || op.kind === "doughnut";
  if (op.x_title && !pie) {
    d.setPropertyValue("HasXAxisTitle", true);
    d.getXAxisTitle().setPropertyValue("String", op.x_title);
  }
  if (op.y_title && !pie) {
    d.setPropertyValue("HasYAxisTitle", true);
    d.getYAxisTitle().setPropertyValue("String", op.y_title);
  }
}

/** Office 現行版の既定に近い配色（系列の色を指定しないとき）。 */
const CHART_COLORS = ["#4472C4", "#ED7D31", "#A5A5A5", "#FFC000", "#5B9BD5", "#70AD47", "#264478", "#9E480E"];

/**
 * グラフの見た目を整える（LibreOffice の既定は灰色の背景・枠つき凡例で古く見える）。
 * 背景は透明、目盛線は薄い灰色、凡例は枠なし、系列は指定の色（無ければ既定の配色）。
 */
function styleChart(eo, d, op, fmts) {
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
    // 解釈できない表示形式は誤りとして止める（下の try に入れると、縦軸の無い種類と一緒に黙って
    // 捨てられる）。
    const key = formatKeyIn(fmts, op.y_number_format);
    try {
      const axis = d.getYAxis();
      axis.setPropertyValue("LinkNumberFormatToSource", false);
      axis.setPropertyValue("NumberFormat", key);
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
