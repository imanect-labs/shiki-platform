/// pptx エクスポート（Task 11.4・design §4.8.3・PIT-42）。
///
/// スライド HTML を**実ブラウザで計測**し、変換可能サブセット（テキスト/リスト/表/画像/
/// 図形ボックス/背景）を pptxgenjs の**ネイティブシェイプ**へ 1:1 変換する（PowerPoint で
/// 完全に再編集できる）。変換不能な要素（CSS transform・SVG・複雑グラデ等）は**要素単位**で
/// PNG 化して埋め込む — スライド全体のラスタライズは契約違反（PIT-42）なのでしない。
///
/// 計測は 1280×720 の論理キャンバス（ビューア/エディタと同一）で行い、px→inch は /96。

import * as htmlToImage from "html-to-image";
import PptxGenJS from "pptxgenjs";

/// 論理キャンバス（px）と pptx レイアウト（inch・96dpi 換算）。
const W_PX = 1280;
const H_PX = 720;
const PX_PER_IN = 96;

/// エクスポート対象のスライド（親から受け取る形・Yjs 由来）。
export interface ExportSlide {
  id: string;
  html: string;
  notes: string;
  bg: Record<string, unknown> | null;
}

/// 変換レポート（保存ダイアログで品質を可視化する・PIT-42）。
export interface ExportReport {
  slides: number;
  texts: number;
  tables: number;
  images: number;
  shapes: number;
  /// ネイティブ変換できず要素単位で画像化した数。
  rasterized: number;
}

/// 計測ステージ用 CSS（キャンバス/ビューアの BASE_CSS と揃える・root クラス版）。
const STAGE_CSS = `
  .shiki-export-stage, .shiki-export-stage * { box-sizing: border-box; }
  .shiki-export-stage {
    position: fixed; left: -100000px; top: 0;
    width: ${W_PX}px; height: ${H_PX}px; overflow: hidden;
    font-family: "Hiragino Sans", "Noto Sans JP", "Yu Gothic", system-ui, sans-serif;
    color: #1a1a1a; background: #ffffff;
    display: flex; flex-direction: column; justify-content: center;
    padding: 72px 96px; line-height: 1.5;
  }
  .shiki-export-stage h1 { font-size: 64px; font-weight: 700; margin: 0 0 24px; letter-spacing: -0.01em; }
  .shiki-export-stage h2 { font-size: 44px; font-weight: 700; margin: 0 0 20px; }
  .shiki-export-stage h3 { font-size: 32px; font-weight: 600; margin: 0 0 16px; }
  .shiki-export-stage p, .shiki-export-stage li { font-size: 26px; margin: 0 0 12px; }
  .shiki-export-stage ul, .shiki-export-stage ol { margin: 0 0 12px; padding-left: 1.4em; }
  .shiki-export-stage table { border-collapse: collapse; font-size: 22px; }
  .shiki-export-stage td, .shiki-export-stage th { border: 1px solid #d4d4d4; padding: 8px 14px; text-align: left; }
  .shiki-export-stage th { background: #f5f5f4; }
  .shiki-export-stage img { max-width: 100%; }
  .shiki-export-stage blockquote { border-left: 4px solid #d4d4d4; margin: 0 0 12px; padding: 4px 0 4px 20px; color: #555; }
  .shiki-export-stage pre, .shiki-export-stage code { font-family: ui-monospace, "SFMono-Regular", monospace; font-size: 22px; }
`;

const px2in = (px: number) => px / PX_PER_IN;
/// CSS px フォントサイズ → pt（96dpi: 1px = 0.75pt）。
const px2pt = (px: number) => Math.round(px * 0.75 * 10) / 10;

/// rgb()/rgba() → pptxgenjs の hex（"RRGGBB"）。透明は null。
function cssColorToHex(value: string): string | null {
  const m = value.match(/rgba?\((\d+),\s*(\d+),\s*(\d+)(?:,\s*([\d.]+))?\)/);
  if (!m) return null;
  if (m[4] !== undefined && Number(m[4]) === 0) return null;
  const hex = (n: string) => Number(n).toString(16).padStart(2, "0");
  return `${hex(m[1])}${hex(m[2])}${hex(m[3])}`.toUpperCase();
}

/// 要素の論理キャンバス内 rect（stage 相対・px）。
function rectOf(el: Element, stage: Element): { x: number; y: number; w: number; h: number } {
  const r = el.getBoundingClientRect();
  const s = stage.getBoundingClientRect();
  return { x: r.left - s.left, y: r.top - s.top, w: r.width, h: r.height };
}

/// ネイティブ変換できない（＝要素単位ラスタライズに落とす）要素か。
function needsRaster(el: HTMLElement): boolean {
  const cs = getComputedStyle(el);
  if (cs.transform !== "none") return true;
  if (cs.backgroundImage !== "none" && !el.matches("img")) return true; // グラデ/画像背景
  if (el.tagName === "SVG" || el.querySelector(":scope svg")) return true;
  return false;
}

/// テキストブロックのランを組み立てる（太字/斜体/色の単純な入れ子まで）。
function textRuns(el: HTMLElement): PptxGenJS.TextProps[] {
  const runs: PptxGenJS.TextProps[] = [];
  const walk = (node: Node, inherited: { bold: boolean; italic: boolean }) => {
    if (node.nodeType === Node.TEXT_NODE) {
      const text = node.textContent ?? "";
      if (text.trim().length > 0) {
        runs.push({ text, options: { bold: inherited.bold, italic: inherited.italic } });
      }
      return;
    }
    if (!(node instanceof HTMLElement)) return;
    const tag = node.tagName.toLowerCase();
    const next = {
      bold: inherited.bold || tag === "b" || tag === "strong",
      italic: inherited.italic || tag === "i" || tag === "em",
    };
    if (tag === "br") {
      runs.push({ text: "", options: { breakLine: true } });
      return;
    }
    node.childNodes.forEach((child) => walk(child, next));
  };
  walk(el, { bold: false, italic: false });
  return runs.length > 0 ? runs : [{ text: el.textContent ?? "" }];
}

type Slide = ReturnType<PptxGenJS["addSlide"]>;

/// テキスト系ブロックをネイティブテキストとして追加する。
function addTextBlock(slide: Slide, el: HTMLElement, stage: Element, report: ExportReport) {
  const { x, y, w, h } = rectOf(el, stage);
  if (w <= 0 || h <= 0) return;
  const cs = getComputedStyle(el);
  const color = cssColorToHex(cs.color) ?? "1A1A1A";
  const fontSize = px2pt(parseFloat(cs.fontSize));
  const bold = Number(cs.fontWeight) >= 600;
  const align = (["left", "center", "right", "justify"] as const).find((a) => a === cs.textAlign);
  slide.addText(textRuns(el), {
    x: px2in(x),
    y: px2in(y),
    w: px2in(w),
    h: px2in(h),
    fontSize,
    bold,
    color,
    align: align ?? "left",
    valign: "top",
    margin: 0,
    fontFace: "Yu Gothic",
  });
  report.texts += 1;
}

/// リスト（ul/ol）を bullet 付きネイティブテキストとして追加する。
function addListBlock(slide: Slide, el: HTMLElement, stage: Element, report: ExportReport) {
  const { x, y, w, h } = rectOf(el, stage);
  if (w <= 0 || h <= 0) return;
  const ordered = el.tagName.toLowerCase() === "ol";
  const items = Array.from(el.querySelectorAll(":scope > li"));
  if (items.length === 0) return;
  const cs = getComputedStyle(items[0]);
  const runs: PptxGenJS.TextProps[] = items.map((li, i) => ({
    text: (li.textContent ?? "").trim(),
    options: {
      bullet: ordered ? { type: "number" } : true,
      breakLine: i < items.length - 1,
    },
  }));
  slide.addText(runs, {
    x: px2in(x),
    y: px2in(y),
    w: px2in(w),
    h: px2in(h),
    fontSize: px2pt(parseFloat(cs.fontSize)),
    color: cssColorToHex(cs.color) ?? "1A1A1A",
    valign: "top",
    margin: 0,
    fontFace: "Yu Gothic",
  });
  report.texts += 1;
}

/// 表をネイティブテーブルとして追加する。
function addTableBlock(slide: Slide, el: HTMLTableElement, stage: Element, report: ExportReport) {
  const { x, y, w } = rectOf(el, stage);
  const rows: PptxGenJS.TableRow[] = [];
  for (const tr of Array.from(el.querySelectorAll("tr"))) {
    const cells: PptxGenJS.TableCell[] = [];
    for (const cell of Array.from(tr.children)) {
      if (!(cell instanceof HTMLTableCellElement)) continue;
      const isHeader = cell.tagName.toLowerCase() === "th";
      cells.push({
        text: (cell.textContent ?? "").trim(),
        options: {
          bold: isHeader,
          fill: isHeader ? { color: "F5F5F4" } : undefined,
          colspan: cell.colSpan > 1 ? cell.colSpan : undefined,
          rowspan: cell.rowSpan > 1 ? cell.rowSpan : undefined,
          border: { type: "solid", color: "D4D4D4", pt: 0.75 },
          fontSize: 16,
        },
      });
    }
    if (cells.length > 0) rows.push(cells);
  }
  if (rows.length === 0) return;
  slide.addTable(rows, { x: px2in(x), y: px2in(y), w: px2in(w), fontFace: "Yu Gothic" });
  report.tables += 1;
}

/// data: URL 画像をネイティブ画像として追加する。
function addImageBlock(slide: Slide, el: HTMLImageElement, stage: Element, report: ExportReport) {
  const src = el.getAttribute("src") ?? "";
  if (!src.startsWith("data:image/")) return; // ドライブ参照は親側で data 化されている前提
  const { x, y, w, h } = rectOf(el, stage);
  slide.addImage({ data: src, x: px2in(x), y: px2in(y), w: px2in(w), h: px2in(h) });
  report.images += 1;
}

/// 装飾ボックス（背景色/枠線/角丸を持つコンテナ）を図形として追加する。
function addBoxShape(slide: Slide, el: HTMLElement, stage: Element, report: ExportReport): boolean {
  const cs = getComputedStyle(el);
  const fill = cssColorToHex(cs.backgroundColor);
  const borderW = parseFloat(cs.borderTopWidth) || 0;
  const line = borderW > 0 ? cssColorToHex(cs.borderTopColor) : null;
  if (!fill && !line) return false;
  const { x, y, w, h } = rectOf(el, stage);
  if (w <= 0 || h <= 0) return false;
  const radius = parseFloat(cs.borderTopLeftRadius) || 0;
  const pptx = slide as unknown as { _slideLayout?: unknown };
  void pptx;
  slide.addShape(radius > 0 ? "roundRect" : "rect", {
    x: px2in(x),
    y: px2in(y),
    w: px2in(w),
    h: px2in(h),
    fill: fill ? { color: fill } : { transparency: 100, color: "FFFFFF" },
    line: line ? { color: line, width: borderW * 0.75 } : { type: "none" },
    rectRadius: radius > 0 ? Math.min(px2in(radius), px2in(Math.min(w, h)) / 2) : undefined,
  });
  report.shapes += 1;
  return true;
}

/// 要素単位のラスタライズ（変換不能フォールバック・全体画像化はしない）。
async function addRasterized(
  slide: Slide,
  el: HTMLElement,
  stage: Element,
  report: ExportReport,
) {
  const { x, y, w, h } = rectOf(el, stage);
  if (w <= 0 || h <= 0) return;
  try {
    const dataUrl = await htmlToImage.toPng(el, { pixelRatio: 2 });
    slide.addImage({ data: dataUrl, x: px2in(x), y: px2in(y), w: px2in(w), h: px2in(h) });
    report.rasterized += 1;
  } catch {
    // 画像化も失敗した要素は落とす（テキストがあれば最後の手段でテキスト化）。
    const text = (el.textContent ?? "").trim();
    if (text) {
      addTextBlock(slide, el, stage, report);
    }
  }
}

const TEXT_TAGS = new Set(["h1", "h2", "h3", "h4", "h5", "h6", "p", "blockquote", "pre"]);
const CONTAINER_TAGS = new Set(["div", "section", "article", "main", "header", "footer", "figure"]);

/// 要素を分類して pptx シェイプへ落とす（コンテナは再帰）。
async function walkElement(
  slide: Slide,
  el: HTMLElement,
  stage: Element,
  report: ExportReport,
): Promise<void> {
  const tag = el.tagName.toLowerCase();
  if (needsRaster(el)) {
    await addRasterized(slide, el, stage, report);
    return;
  }
  if (TEXT_TAGS.has(tag)) {
    addTextBlock(slide, el, stage, report);
    return;
  }
  if (tag === "ul" || tag === "ol") {
    addListBlock(slide, el, stage, report);
    return;
  }
  if (tag === "table") {
    addTableBlock(slide, el as unknown as HTMLTableElement, stage, report);
    return;
  }
  if (tag === "img") {
    addImageBlock(slide, el as unknown as HTMLImageElement, stage, report);
    return;
  }
  if (CONTAINER_TAGS.has(tag)) {
    // 装飾（背景/枠線）は図形として敷き、子は個別に変換する。
    addBoxShape(slide, el, stage, report);
    for (const child of Array.from(el.children)) {
      if (child instanceof HTMLElement) {
        await walkElement(slide, child, stage, report);
      }
    }
    return;
  }
  // 未知タグ: テキストがあればテキストとして保全。
  if ((el.textContent ?? "").trim().length > 0) {
    addTextBlock(slide, el, stage, report);
  }
}

/// デッキ全体を pptx（Blob）へ変換する。
export async function exportPptx(
  slides: ExportSlide[],
  title: string,
  sanitize: (html: string) => string,
): Promise<{ blob: Blob; report: ExportReport }> {
  const pptx = new PptxGenJS();
  pptx.defineLayout({ name: "SHIKI_16x9", width: px2in(W_PX), height: px2in(H_PX) });
  pptx.layout = "SHIKI_16x9";
  pptx.title = title;

  const style = document.createElement("style");
  style.textContent = STAGE_CSS;
  document.head.appendChild(style);
  const stage = document.createElement("div");
  stage.className = "shiki-export-stage";
  document.body.appendChild(stage);

  const report: ExportReport = {
    slides: slides.length,
    texts: 0,
    tables: 0,
    images: 0,
    shapes: 0,
    rasterized: 0,
  };
  try {
    await document.fonts.ready;
    for (const s of slides) {
      stage.innerHTML = sanitize(s.html);
      // 画像のロードを待つ（サイズ計測に必要）。
      await Promise.all(
        Array.from(stage.querySelectorAll("img")).map((img) =>
          img.decode().catch(() => undefined),
        ),
      );
      const slide = pptx.addSlide();
      const bgColor = typeof s.bg?.color === "string" ? cssColorToHex(String(s.bg.color)) : null;
      const bgHex =
        bgColor ?? (typeof s.bg?.color === "string" ? String(s.bg.color).replace("#", "") : null);
      if (bgHex && /^[0-9a-fA-F]{6}$/.test(bgHex)) {
        slide.background = { color: bgHex.toUpperCase() };
      }
      for (const child of Array.from(stage.children)) {
        if (child instanceof HTMLElement) {
          await walkElement(slide, child, stage, report);
        }
      }
      if (s.notes.trim().length > 0) {
        slide.addNotes(s.notes);
      }
    }
    const blob = (await pptx.write({ outputType: "blob" })) as Blob;
    return { blob, report };
  } finally {
    stage.remove();
    style.remove();
  }
}
