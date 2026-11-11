/// PDF の抜粋。保存座標は crop box 相対の PDF ポイント（左下原点）。
/// 画面のスクロール・CSS 倍率・DPR・表示回転を保存値へ混ぜない。
import type { PDFDocumentProxy, PageViewport } from "pdfjs-dist";
import type { TextContent, TextItem } from "pdfjs-dist/types/src/display/api";
import { storageScopeGeneration } from "@/lib/user-storage";
import { getVersionBlocks, type DocBlock } from "@/lib/storage";
import type { SelectionContext } from "@/lib/selection-context";
import { boxToViewport, viewportDimensions } from "@/lib/pdf-geometry";

export type PdfRect = [number, number, number, number];
export type PdfExcerpt = {
  id: string;
  page: number;
  bbox: PdfRect;
  text: string;
  tables: string[];
  blockOrdinals: number[];
};

const nativeTexts = new WeakMap<
  PDFDocumentProxy,
  Map<number, Promise<TextContent>>
>();
export function nativePdfText(
  doc: PDFDocumentProxy,
  page: number,
): Promise<TextContent> {
  let pages = nativeTexts.get(doc);
  if (!pages) {
    pages = new Map();
    nativeTexts.set(doc, pages);
  }
  let result = pages.get(page);
  if (!result) {
    result = doc.getPage(page).then((p) => p.getTextContent());
    pages.set(page, result);
    result.catch(() => pages.delete(page));
  }
  return result;
}

const sources = new WeakMap<
  PDFDocumentProxy,
  {
    id: string;
    version: number;
    generation: number;
    pages: Map<number, Promise<DocBlock[]>>;
  }
>();
export function bindPdfSource(
  doc: PDFDocumentProxy,
  id: string,
  version: number,
) {
  if (!sources.has(doc))
    sources.set(doc, {
      id,
      version,
      generation: storageScopeGeneration(),
      pages: new Map(),
    });
}
export function assertPdfSource(doc: PDFDocumentProxy) {
  const source = sources.get(doc);
  if (source && source.generation !== storageScopeGeneration())
    throw new Error("ログイン状態が変わりました。PDF を開き直してください。");
}
export function pdfBlocks(
  doc: PDFDocumentProxy,
  page: number,
): Promise<DocBlock[]> {
  assertPdfSource(doc);
  const source = sources.get(doc);
  if (!source) return Promise.resolve([]);
  const cached = source.pages.get(page);
  if (cached) return cached;
  const result = (async () => {
    const blocks: DocBlock[] = [];
    let from: number | null = 0;
    while (from !== null) {
      const data = await getVersionBlocks(source.id, source.version, {
        page,
        from,
        limit: 200,
      });
      blocks.push(...data.blocks);
      from = data.next_from ?? null;
    }
    return blocks;
  })();
  source.pages.set(page, result);
  result.catch(() => source.pages.delete(page));
  return result;
}
export function resetPdfBlocks(doc: PDFDocumentProxy, page: number) {
  sources.get(doc)?.pages.delete(page);
}

export function rectFromViewport(
  viewport: PageViewport,
  a: [number, number],
  b: [number, number],
): PdfRect {
  const [x1, y1] = viewport.convertToPdfPoint(...a);
  const [x2, y2] = viewport.convertToPdfPoint(...b);
  const { pageX, pageY } = viewportDimensions(viewport);
  return [
    Math.min(x1, x2) - pageX,
    Math.min(y1, y2) - pageY,
    Math.max(x1, x2) - pageX,
    Math.max(y1, y2) - pageY,
  ];
}
export function intersect(a: PdfRect, b: PdfRect): boolean {
  return (
    Math.min(a[2], b[2]) - Math.max(a[0], b[0]) > 0.1 &&
    Math.min(a[3], b[3]) - Math.max(a[1], b[1]) > 0.1
  );
}
export function blockRect(
  prov: DocBlock["prov"][number],
  height: number,
): PdfRect {
  const [l, t, r, b] = prov.bbox;
  const ys = prov.origin === "top_left" ? [height - t, height - b] : [t, b];
  return [Math.min(l, r), Math.min(...ys), Math.max(l, r), Math.max(...ys)];
}

/// OCR は既存の文字層が覆っていない領域だけ補う。通常の PDF 文字は重複させない。
export function withOcrContent(
  native: TextContent,
  blocks: DocBlock[],
  page: number,
  viewport: PageViewport,
): TextContent {
  const nativeItems = native.items.filter(
    (t): t is TextItem => "str" in t && !!t.str.trim(),
  );
  const nativeRects = nativeItems
    .filter((t): t is TextItem => "str" in t && !!t.str.trim())
    .map((t) => {
      const [x, y] = [
        t.transform[4] - viewportDimensions(viewport).pageX,
        t.transform[5] - viewportDimensions(viewport).pageY,
      ];
      return [x, y, x + t.width, y + t.height] as PdfRect;
    });
  const items: TextItem[] = [];
  for (const block of blocks) {
    const prov = block.prov.find((p) => p.page === page);
    if (!prov || !block.text.trim()) continue;
    const rect = blockRect(prov, viewportDimensions(viewport).pageHeight);
    const compact = (text: string) => text.replace(/\s+/g, "");
    const known = nativeItems
      .filter((_, i) => intersect(nativeRects[i], rect))
      .map((item) => item.str)
      .join("");
    // 矩形が少し重なるだけでは全文を捨てない（見出しだけ文字層があるスキャン等）。
    if (known && compact(known).includes(compact(block.text))) continue;
    const [l, b, r, t] = rect;
    const fontSize = Math.min(14, Math.max(4, t - b));
    items.push({
      str: block.text,
      dir: "ltr",
      transform: [
        fontSize,
        0,
        0,
        fontSize,
        l + viewportDimensions(viewport).pageX,
        t - fontSize + viewportDimensions(viewport).pageY,
      ],
      width: r - l,
      height: fontSize,
      fontName: "ocr",
      hasEOL: true,
    });
  }
  return {
    ...native,
    items: [...native.items, ...items],
    styles: {
      ...native.styles,
      ocr: { fontFamily: "sans-serif", ascent: 1, descent: 0, vertical: false },
    },
  };
}

export async function extractRegion(
  doc: PDFDocumentProxy,
  pageNumber: number,
  bbox: PdfRect,
): Promise<PdfExcerpt> {
  assertPdfSource(doc);
  const page = await doc.getPage(pageNumber);
  const viewport = page.getViewport({ scale: 1 });
  let failure: unknown;
  const blocks = await pdfBlocks(doc, pageNumber).catch((e: unknown) => {
    failure = e;
    return [];
  });
  const matched = blocks.filter((b) =>
    b.prov.some(
      (p) =>
        p.page === pageNumber &&
        intersect(bbox, blockRect(p, viewportDimensions(viewport).pageHeight)),
    ),
  );
  const native = matched.length
    ? []
    : (await nativePdfText(doc, pageNumber)).items
        .filter((item): item is TextItem => "str" in item && !!item.str.trim())
        .filter((item) => {
          const tx = item.transform;
          const length = Math.hypot(tx[0], tx[1]) || 1;
          const ux = tx[0] / length,
            uy = tx[1] / length;
          const x = tx[4] - viewportDimensions(viewport).pageX,
            y = tx[5] - viewportDimensions(viewport).pageY;
          const points = [
            [x, y],
            [x + ux * item.width, y + uy * item.width],
            [x - uy * item.height, y + ux * item.height],
            [
              x + ux * item.width - uy * item.height,
              y + uy * item.width + ux * item.height,
            ],
          ];
          return intersect(bbox, [
            Math.min(...points.map((p) => p[0])),
            Math.min(...points.map((p) => p[1])),
            Math.max(...points.map((p) => p[0])),
            Math.max(...points.map((p) => p[1])),
          ]);
        });
  if (!matched.length && !native.length && failure) throw failure;
  // 段落・表の途中を切り落とさない。抽出粒度はプレビューに明示する。
  return {
    id: crypto.randomUUID(),
    page: pageNumber,
    bbox,
    text: matched.length
      ? matched.map((b) => b.text).join("\n\n")
      : native
          .map((t) => t.str + (t.hasEOL ? "\n" : " "))
          .join("")
          .trim(),
    tables: matched.filter((b) => b.type === "table").map((b) => b.text),
    blockOrdinals: matched.map((b) => b.ordinal),
  };
}

export function excerptContext(
  nodeId: string,
  name: string,
  version: number,
  excerpts: PdfExcerpt[],
): SelectionContext {
  const ordered = [...excerpts].sort(
    (a, b) => a.page - b.page || b.bbox[3] - a.bbox[3] || a.bbox[0] - b.bbox[0],
  );
  return {
    kind: "pdf_selection",
    node_id: nodeId,
    excerpt: ordered
      .map(
        (e, i) =>
          `[抜粋${i + 1} / ${name} / v${version} / p.${e.page}]\n${e.text || "（文字を抽出できない領域）"}`,
      )
      .join("\n\n"),
    locator: {
      version,
      regions: ordered.map((e) => ({
        page: e.page,
        bbox: e.bbox,
        origin: "bottom_left",
        blocks: e.blockOrdinals,
      })),
    },
  };
}

/// GFM のエスケープされた | とセル内改行を保って CSV へ変換。
export function tableCsv(markdown: string): string {
  const rows = markdown
    .trim()
    .split("\n")
    .filter((l) => l.includes("|"));
  return (
    "\uFEFF" +
    rows
      .map((line, index) => {
        const cells = line
          .trim()
          .replace(/^\|/, "")
          .replace(/\|$/, "")
          .split(/(?<!\\)\|/);
        if (
          index === 1 &&
          cells.every((cell) => /^:?-{3,}:?$/.test(cell.trim()))
        )
          return null;
        return cells
          .map(
            (c) =>
              '"' +
              c
                .trim()
                .replace(/\\\|/g, "|")
                .replace(/<br\s*\/?\s*>/gi, "\n")
                .replace(/"/g, '""') +
              '"',
          )
          .join(",");
      })
      .filter((row) => row !== null)
      .join("\r\n")
  );
}
export function saveBlob(blob: Blob, name: string) {
  const url = URL.createObjectURL(blob);
  const a = document.createElement("a");
  a.href = url;
  a.download = name;
  a.click();
  setTimeout(() => URL.revokeObjectURL(url), 30_000);
}
export async function regionPng(
  doc: PDFDocumentProxy,
  excerpt: PdfExcerpt,
  maxSide = 1600,
): Promise<Blob> {
  assertPdfSource(doc);
  const page = await doc.getPage(excerpt.page);
  const base = page.getViewport({ scale: 1 });
  const source = boxToViewport(excerpt.bbox, false, base);
  const scale = Math.min(
    2,
    maxSide / Math.max(1, source.width),
    maxSide / Math.max(1, source.height),
  );
  const viewport = page.getViewport({ scale });
  const rect = boxToViewport(excerpt.bbox, false, viewport);
  const canvas = document.createElement("canvas");
  canvas.width = Math.max(1, Math.ceil(rect.width));
  canvas.height = Math.max(1, Math.ceil(rect.height));
  if (canvas.width * canvas.height > 16_000_000)
    throw new Error("範囲が大きすぎます。範囲を小さくしてください。");
  const ctx = canvas.getContext("2d");
  if (!ctx) throw new Error("画像を作成できませんでした");
  await page.render({
    canvasContext: ctx,
    viewport,
    transform: [1, 0, 0, 1, -rect.left, -rect.top],
  }).promise;
  assertPdfSource(doc);
  return new Promise((resolve, reject) =>
    canvas.toBlob(
      (b) => (b ? resolve(b) : reject(new Error("画像を保存できませんでした"))),
      "image/png",
    ),
  );
}

export function resetDocumentBlocks(doc: PDFDocumentProxy) {
  sources.get(doc)?.pages.clear();
}
