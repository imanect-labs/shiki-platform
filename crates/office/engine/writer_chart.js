// Shiki Office Engine — Writer のグラフ（#731）。
//
// Word のグラフは、内部データを持つ埋め込みの chart（TextEmbeddedObject）として入れる。docx へは
// Word のグラフ（word/charts/chartN.xml・値はその中に持つ）として書き出される（画像ではない。
// 26.04.5 はデータの埋め込みブックを付けないので、Word の「データの編集」は使えない・実測）。
// 種類と見た目は Calc のグラフと同じ shapeChart（chart.js）で決める。

/** chart の埋め込みオブジェクトの CLSID（LibreOffice の Chart2）。 */
const CHART_CLSID = "12dcae26-281f-416f-a234-c3086127382e";

/**
 * グラフを入れる。データは項目（categories）× 系列（series）で、欠けた値（null）は空のまま。
 * 位置は insert と同じ（anchor の前後か、文書の先頭・末尾）。グラフは専用の段落に文字として置き、中央に揃える。
 */
WOPS.insert_chart = (op, resolved) => {
  STAGE = "グラフの配置";
  let para;
  if (op.anchor !== null) para = emptyParagraphAt(refOf(resolved, op.anchor, op.before ? "before" : "after"), op.before);
  else if (op.at === "start") para = emptyParagraphAt(bodyBlocks()[0], true);
  else para = emptyParagraphAt(null, false);
  para.setPropertyValue("ParaAdjust", I.style.ParagraphAdjust.CENTER);
  const text = DOC.getText();
  const obj = DOC.createInstance("com.sun.star.text.TextEmbeddedObject");
  obj.setPropertyValue("CLSID", CHART_CLSID);
  obj.setPropertyValue("AnchorType", I.text.TextContentAnchorType.AS_CHARACTER);
  obj.setPropertyValue("Width", long(op.size[0]));
  obj.setPropertyValue("Height", long(op.size[1]));
  text.insertTextContent(text.createTextCursorByRange(para.getStart()), obj, false);
  STAGE = "グラフのデータ";
  const chart = obj.getEmbeddedObject();
  // 内部データ（Writer のグラフはセル範囲を持たない）。行 = 項目、列 = 系列。
  chart.createInternalDataProvider(false);
  const data = chart.getData();
  const rows = op.categories.map((_, r) => op.series.map((s) => (s.values[r] === null || s.values[r] === undefined ? NaN : s.values[r])));
  data.setData(rows);
  data.setRowDescriptions(op.categories);
  data.setColumnDescriptions(op.series.map((s) => s.name));
  // 新しい chart は図も系列も持たない。図を置き、内部データ全体（"all"）を系列として読ませる
  // （データを入れただけでは系列は 0 本のまま・実測）。1 列目は系列名、各行の頭は項目名。
  chart.setDiagram(chart.createInstance(DIAGRAMS[op.kind]));
  chart.setArguments([
    pv("CellRangeRepresentation", "all"),
    pv("DataRowSource", I.chart.ChartDataRowSource.COLUMNS),
    pv("FirstCellAsLabel", true),
    pv("HasCategories", true),
  ]);
  STAGE = "グラフの種類と体裁";
  shapeChart(chart, op, chart.getNumberFormats());
  return { chart: obj.getName() };
};

/** 段落の中の埋め込みグラフを「［グラフ: 題］」の形で並べる（読み取りの概要用・無ければ ""）。 */
function chartLabelsOf(p) {
  let out = "";
  try {
    const en = p.createEnumeration();
    while (en.hasMoreElements()) {
      const r = en.nextElement();
      if (r.getPropertyValue("TextPortionType") !== "Frame") continue;
      const contents = r.createContentEnumeration("com.sun.star.text.TextContent");
      while (contents.hasMoreElements()) {
        const c = contents.nextElement();
        let title = null;
        try {
          if (c.getPropertyValue("CLSID").toLowerCase() !== CHART_CLSID) continue;
          const chart = c.getEmbeddedObject();
          if (chart.getPropertyValue("HasMainTitle")) title = chart.getTitle().getPropertyValue("String");
        } catch (e) {
          // グラフ以外（画像・枠）か、題を持たないグラフ。
          continue;
        }
        out += title ? `［グラフ: ${title}］` : "［グラフ］";
      }
    }
  } catch (e) {
    return "";
  }
  return out;
}
