/// PDF の抜粋。保存座標は crop box 相対の PDF ポイント（左下原点）。
/// 画面のスクロール・CSS 倍率・DPR・表示回転を保存値へ混ぜない。
import type { PDFDocumentProxy, PageViewport } from "pdfjs-dist";
import type { TextContent, TextItem } from "pdfjs-dist/types/src/display/api";
import { storageScopeGeneration } from "@/lib/user-storage";
import { getVersionBlocks, type DocBlock } from "@/lib/storage";
import type { SelectionContext } from "@/lib/selection-context";
import { boxToViewport, viewportDimensions } from "@/lib/pdf-geometry";

export type PdfRect = [number, number, number, number];
export const PDF_LOCATOR_LIMIT = 4096;
export type PdfExcerpt = {
  precision?: "line" | "cell";
  cells?: { row: number; column: number; bbox: PdfRect }[];
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
  const first = Math.floor((page - 1) / 16) * 16 + 1;
  const last = Math.min(doc.numPages, first + 15);
  const batch = (async () => {
    const blocks: DocBlock[] = [];
    let from: number | null = 0;
    while (from !== null) {
      assertPdfSource(doc);
      const data = await getVersionBlocks(source.id, source.version, {
        page: first,
        page_end: last,
        from,
        limit: 200,
      });
      blocks.push(...data.blocks);
      const next = data.next_from ?? null;
      if (next !== null && next <= from)
        throw new Error("解析結果のページングが進みませんでした");
      from = next;
    }
    assertPdfSource(doc);
    return blocks;
  })();
  for (let number = first; number <= last; number++) {
    if (source.pages.has(number)) continue;
    const result = batch.then((blocks) =>
      blocks.filter(
        (block) =>
          block.page === number || block.prov.some((p) => p.page === number),
      ),
    );
    source.pages.set(number, result);
    result.catch(() => {
      if (source.pages.get(number) === result) source.pages.delete(number);
    });
  }
  return source.pages.get(page)!;
}

/// 一度に最大4窓（64ページ）。キー入力で中断した後は新しい窓を要求しない。
export async function prefetchPdfBlocks(
  doc: PDFDocumentProxy,
  first: number,
  last: number,
  signal: AbortSignal,
) {
  const requests: Promise<unknown>[] = [];
  for (let page = first; page <= last && !signal.aborted; page += 16) {
    requests.push(pdfBlocks(doc, page).catch(() => []));
  }
  await Promise.all(requests);
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

/// ページ内の本文と位置。charspan は Docling/Python と同じ code point 単位。
/// 古い跨ページブロックは本文をページへ推測配分せず、native 文字層または画像を利用する。
function blockFragments(
  block: DocBlock,
  page: number,
  height: number,
): { text: string; rect: PdfRect; hasEOL: boolean }[] {
  const mine = block.prov.filter((p) => p.page === page);
  const chars = Array.from(block.text);
  const spans = mine.filter(
    (p) =>
      p.charspan &&
      p.charspan.length === 2 &&
      p.charspan[0] >= 0 &&
      p.charspan[1] > p.charspan[0] &&
      p.charspan[1] <= chars.length,
  );
  if (block.type !== "table" && spans.length) {
    spans.sort((a, b) => a.charspan![0] - b.charspan![0]);
    return spans.map((p, index) => {
      const end = p.charspan![1];
      const next = spans[index + 1]?.charspan?.[0];
      const gap = next != null ? chars.slice(end, next).join("") : "";
      return {
        text:
          chars.slice(p.charspan![0], end).join("") +
          (/^\s*$/.test(gap) ? gap : ""),
        rect: blockRect(p, height),
        hasEOL: next == null || !/^\s*$/.test(gap),
      };
    });
  }
  if (!mine.length || block.prov.some((p) => p.page !== page)) return [];
  const boxes = mine.map((p) => blockRect(p, height));
  return [
    {
      text: block.text,
      hasEOL: true,
      rect: [
        Math.min(...boxes.map((r) => r[0])),
        Math.min(...boxes.map((r) => r[1])),
        Math.max(...boxes.map((r) => r[2])),
        Math.max(...boxes.map((r) => r[3])),
      ],
    },
  ];
}

/// TextItem の局所軸から矩形を求め、原本内の縦書き・回転文字にも適用する。
export function nativeRect(
  item: TextItem,
  pageX: number,
  pageY: number,
  style?: TextContent["styles"][string],
): PdfRect {
  const tx = item.transform;
  const fontHeight = Math.hypot(tx[2], tx[3]) || Math.hypot(tx[0], tx[1]) || 1;
  const horizontalLength = Math.hypot(tx[0], tx[1]) || 1;
  const hx = tx[0] / horizontalLength,
    hy = tx[1] / horizontalLength;
  const vx = tx[2] / fontHeight,
    vy = tx[3] / fontHeight;
  const vertical = item.dir === "ttb" || style?.vertical;
  const ux = vertical ? -vx : hx,
    uy = vertical ? -vy : hy;
  const crossX = vertical ? hx : vx,
    crossY = vertical ? hy : vy;
  const advance = vertical ? item.height : item.width;
  const thickness = (vertical ? item.width : item.height) || fontHeight;
  const ascent =
    style?.ascent ?? (style?.descent != null ? 1 + style.descent : 0.8);
  const low = (ascent - 1) * thickness,
    high = ascent * thickness;
  const x = tx[4] - pageX,
    y = tx[5] - pageY;
  const points = [
    [x + crossX * low, y + crossY * low],
    [x + crossX * high, y + crossY * high],
    [x + ux * advance + crossX * low, y + uy * advance + crossY * low],
    [x + ux * advance + crossX * high, y + uy * advance + crossY * high],
  ];
  return [
    Math.min(...points.map((p) => p[0])),
    Math.min(...points.map((p) => p[1])),
    Math.max(...points.map((p) => p[0])),
    Math.max(...points.map((p) => p[1])),
  ];
}

// 合成OCR片は検索のオフセット専用。正確な文字位置を持たないのでTextLayerへ描かない。
const ocrRects = new WeakMap<TextItem, PdfRect>();
export function syntheticOcrRect(item: TextItem): PdfRect | undefined {
  return ocrRects.get(item);
}

const nativeOrigins = new WeakMap<TextItem, TextItem>();
export function nativeTextOrigin(item: TextItem): TextItem {
  return nativeOrigins.get(item) ?? item;
}

/// 保存本文と完全一致するnativeは正確な文字位置を使い、差のあるブロックは保存本文と枠を使う。
export function withOcrContent(
  native: TextContent,
  blocks: DocBlock[],
  page: number,
  viewport: PageViewport,
): TextContent {
  const dims = viewportDimensions(viewport);
  const nativeItems = native.items.filter((item): item is TextItem => "str" in item);
  const rects = new Map(nativeItems.map((item) => [item, nativeRect(item, dims.pageX, dims.pageY, native.styles[item.fontName])]));
  const compact = (text: string) => text.replace(/\s+/g, "");
  const removed = new Set<TextItem>();
  const owners = new Map<TextItem, { block: number; part: number }>();
  const eol = new Map<TextItem, boolean>();
  const syntheticBlocks = new Set<number>();
  const items: TextItem[] = [];
  const prepared = blocks.map((block) => {
    const fragments = blockFragments(block, page, dims.pageHeight).map((fragment) => ({
      ...fragment,
      text: block.type === "table" ? tableRows(fragment.text).map((row) => row.join(" ")).join("\n") : fragment.text,
    })).filter((fragment) => fragment.text.trim());
    const canonical = fragments.map((fragment) => fragment.text).join("");
    return { block, fragments, canonical };
  });
  const candidatesFor = (fragments: (typeof prepared)[number]["fragments"]) => nativeItems.filter((item) => item.str.length > 0 && fragments.some((fragment) => intersect(rects.get(item)!, fragment.rect)));
  // 広いOCR枠が別段落と重なっても、その段落に完全一致するnativeを削除しない。
  // 候補配列はブロックごとに手放す（重なる枠で文字片数×ブロック数のメモリを保持しない）。
  const exactOwners = new Map<TextItem, number>();
  const exactBlocks = new Set<number>();
  for (const { block, fragments, canonical } of prepared) {
    const candidates = candidatesFor(fragments);
    if (candidates.map((item) => item.str).join("") !== canonical) continue;
    exactBlocks.add(block.ordinal);
    for (const item of candidates) if (!exactOwners.has(item)) exactOwners.set(item, block.ordinal);
  }
  for (const { block, fragments, canonical } of prepared) {
    if (!fragments.length) continue;
    const candidates = candidatesFor(fragments);
    if (exactBlocks.has(block.ordinal)) {
      candidates.forEach((item, index) => {
        if (!owners.has(item)) owners.set(item, { block: block.ordinal, part: 0 });
        eol.set(item, index === candidates.length - 1);
      });
      continue;
    }
    syntheticBlocks.add(block.ordinal);
    // 部分的なnativeとOCRを一行へ推測連結しない。保存本文を使い、nativeの描画は原本のまま維持する。
    for (const item of candidates) {
      if (exactOwners.has(item) && exactOwners.get(item) !== block.ordinal) continue;
      const [l, b, r, t] = rects.get(item)!;
      const contained = fragments.some(({ rect }) => l >= rect[0] - 1 && b >= rect[1] - 1 && r <= rect[2] + 1 && t <= rect[3] + 1);
      if (!contained && !compact(canonical).includes(compact(item.str))) continue;
      removed.add(item);
      if (!owners.has(item)) owners.set(item, { block: block.ordinal, part: Math.max(0, fragments.findIndex((fragment) => intersect(rects.get(item)!, fragment.rect))) });
    }
    fragments.forEach((fragment, part) => {
      const [l, b, r, t] = fragment.rect;
      const fontSize = Math.min(14, Math.max(4, t - b));
      const item: TextItem = { str: fragment.text, dir: "ltr", transform: [fontSize, 0, 0, fontSize, l + dims.pageX, t - fontSize + dims.pageY], width: r - l, height: fontSize, fontName: "ocr", hasEOL: fragment.hasEOL };
      ocrRects.set(item, fragment.rect);
      owners.set(item, { block: block.ordinal, part });
      items.push(item);
    });
  }
  // 保存本文の段落境界で区切るので、同じ段落の途中に原本の空EOLを挟まない。
  let previousOwner: { block: number; part: number } | undefined;
  for (const item of nativeItems) {
    if (item.str.trim()) previousOwner = owners.get(item);
    else if (previousOwner && (!item.str || syntheticBlocks.has(previousOwner.block))) {
      removed.add(item);
      if (!owners.has(item)) owners.set(item, previousOwner);
    }
  }
  const original = native.items.filter(
    (item): item is TextItem => "str" in item,
  );
  const anchors = original.map((item) => owners.get(item));
  const tail =
    blocks.reduce((max, block) => Math.max(max, block.ordinal), -1) + 1;
  const beforeOwners: typeof anchors = [],
    afterOwners: typeof anchors = [];
  let nearest: (typeof anchors)[number];
  for (let i = 0; i < anchors.length; i++) {
    beforeOwners[i] = nearest;
    if (anchors[i]) nearest = anchors[i];
  }
  nearest = undefined;
  for (let i = anchors.length - 1; i >= 0; i--) {
    afterOwners[i] = nearest;
    if (anchors[i]) nearest = anchors[i];
  }
  const ordered = original.flatMap((item, index) => {
    if (removed.has(item)) return [];
    let rank = owners.get(item);
    if (!rank) {
      const before = beforeOwners[index];
      const after = afterOwners[index];
      if (
        before &&
        after &&
        before.block === after.block &&
        before.part === after.part
      )
        rank = before;
      else if (before) rank = { block: before.block, part: before.part + 0.5 };
      else if (after) rank = { block: after.block, part: after.part - 0.5 };
      else rank = { block: tail, part: 0 };
    }
    return [{ item, rank, index }];
  });
  for (const item of items)
    ordered.push({
      item,
      rank: owners.get(item)!,
      index: original.length + ordered.length,
    });
  ordered.sort(
    (a, b) =>
      a.rank.block - b.rank.block ||
      a.rank.part - b.rank.part ||
      a.index - b.index,
  );
  return {
    ...native,
    items: [
      ...native.items.filter((item) => !("str" in item)),
      ...ordered.map(({ item }) => {
        const hasEOL = eol.get(item);
        if (hasEOL === undefined || hasEOL === item.hasEOL) return item;
        const adjusted = { ...item, hasEOL };
        nativeOrigins.set(adjusted, item);
        return adjusted;
      }),
    ],
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
  const located = matched.flatMap((block) =>
    blockFragments(block, pageNumber, viewportDimensions(viewport).pageHeight)
      .filter((fragment) => intersect(bbox, fragment.rect))
      .map((fragment) => ({ block, text: fragment.text, rect: fragment.rect })),
  );
  const dims = viewportDimensions(viewport);
  let nativeLine = 0;
  const nativeContent = await nativePdfText(doc, pageNumber);
  const native = nativeContent.items
    .flatMap((item) => {
      if (!("str" in item)) return [];
      const line = nativeLine;
      // pdf.js は str が空で幅・高さ0のEOLマーカーも返す。矩形で落とす前に行番号へ反映する。
      if (item.hasEOL) nativeLine++;
      return item.str.length
        ? [
            {
              item,
              rect: nativeRect(
                item,
                dims.pageX,
                dims.pageY,
                nativeContent.styles[item.fontName],
              ),
              line,
            },
          ]
        : [];
    })
    .filter(({ rect }) => intersect(bbox, rect))
    // 保存済みの文字で覆われない領域は、同じ選択内でも原本の文字層から補う。
    .filter(
      ({ rect }) =>
        !located.some(
          (entry) =>
            rect[0] >= entry.rect[0] - 1 &&
            rect[1] >= entry.rect[1] - 1 &&
            rect[2] <= entry.rect[2] + 1 &&
            rect[3] <= entry.rect[3] + 1,
        ),
    );
  // Doclingのordinalが正の読み順。座標でソートすると2段組の左右が混ざる。
  const tailOrder =
    matched.reduce((max, block) => Math.max(max, block.ordinal), -1) + 1;
  const entries = [
    ...located.map((entry) => ({
      ...entry,
      group: entry.block,
      order: entry.block.ordinal,
      native: false,
      line: 0,
    })),
    ...native.map(({ item, rect, line }) => {
      // 旧形式のブロックもprovenanceで位置付ける。未対応の原本文字も同じ本文ストリームとして扱う。
      const owner = matched.find((block) =>
        block.prov.some(
          (p) =>
            p.page === pageNumber &&
            intersect(rect, blockRect(p, dims.pageHeight)),
        ),
      );
      return {
        text: item.str,
        rect,
        group: owner,
        order: owner?.ordinal ?? tailOrder,
        native: true,
        line,
      };
    }),
  ].sort((a, b) => a.order - b.order);
  if (!located.length && !native.length && failure) throw failure;
  // 段落・表の途中を切り落とさない。抽出粒度はプレビューに明示する。
  return {
    id: crypto.randomUUID(),
    page: pageNumber,
    bbox,
    text: entries
      .map((entry, index) => {
        const previous = entries[index - 1];
        let separator = "";
        if (previous) {
          if (previous.group !== entry.group) separator = "\n\n";
          else if (previous.native && entry.native)
            separator =
              entry.line === previous.line
                ? ""
                : "\n".repeat(
                    Math.min(2, Math.max(1, entry.line - previous.line)),
                  );
          else if (previous.native !== entry.native) separator = "\n";
        }
        return separator + entry.text;
      })
      .join("")
      .trim(),
    tables: located
      .filter((entry) => entry.block.type === "table")
      .map((entry) => entry.text),
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
        ...(e.precision ? { precision: e.precision } : {}),
        ...(e.cells ? { cells: e.cells } : {}),
      })),
    },
  };
}

/// GFM の表構造だけを取り出す。ヘッダ直後以外の --- はデータとして残す。
export function tableRows(markdown: string): string[][] {
  return markdown
    .trim()
    .split("\n")
    .filter((line) => line.includes("|"))
    .map((line, index) => {
      const cells = line
        .trim()
        .replace(/^\|/, "")
        .replace(/\|$/, "")
        .split(/(?<!\\)\|/);
      if (index === 1 && cells.every((cell) => /^:?-{3,}:?$/.test(cell.trim())))
        return null;
      return cells.map((cell) =>
        cell
          .trim()
          .replace(/\\\|/g, "|")
          .replace(/<br\s*\/?\s*>/gi, "\n"),
      );
    })
    .filter((row): row is string[] => row !== null);
}

/// PDF の文字は数式として実行しない。符号付きの数値は数値のまま保存する。
function csvText(cell: string): string {
  const probe = cell.replace(/^[\s\x00-\x1f]+/, "");
  const number = /^[+-]?(?:\d+(?:\.\d*)?|\.\d+)(?:[eE][+-]?\d+)?$/.test(probe);
  return (/^[=+@-]/.test(probe) && !number) || /^[\t\r\n]/.test(cell)
    ? "'" + cell
    : cell;
}

export function tableCsv(markdown: string): string {
  return (
    "\uFEFF" +
    tableRows(markdown)
      .map((row) =>
        row
          .map((cell) => '"' + csvText(cell).replace(/"/g, '""') + '"')
          .join(","),
      )
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
