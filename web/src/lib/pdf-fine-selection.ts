import type { PageViewport } from "pdfjs-dist";
import type { TextContent, TextItem } from "pdfjs-dist/types/src/display/api";
import {
  rectFromViewport,
  intersect,
  blockRect,
  nativeRect,
  tableRows,
  PDF_LOCATOR_LIMIT,
  type PdfRect,
} from "./pdf-excerpts";
import { viewportDimensions } from "./pdf-geometry";
import type { DocBlock } from "./storage";

export type SelectionPrecision = "area" | "line" | "cell";
export type FineUnit = {
  bbox: PdfRect;
  text: string;
  row: number;
  column?: number;
  table?: number;
  kind: "line" | "cell";
};
export type FineContent = {
  precision: "line" | "cell";
  text: string;
  tables: string[];
  cells?: { row: number; column: number; bbox: PdfRect }[];
};
const MAX_UNITS = 128,
  MAX_GRID = 512,
  MAX_TEXT = 7600;
const compact = (s: string) => s.replace(/\s/g, "");
const union = (rects: PdfRect[]): PdfRect => [
  Math.min(...rects.map((r) => r[0])),
  Math.min(...rects.map((r) => r[1])),
  Math.max(...rects.map((r) => r[2])),
  Math.max(...rects.map((r) => r[3])),
];

/// 文字の進行方向は実DOM Range、行高は原本のフォント寸法を使う。倍率によるCSSの丸めを保存値へ混ぜない。
export function measureFineUnits(
  container: HTMLElement,
  divs: HTMLElement[],
  native: TextContent,
  viewport: PageViewport,
  blocks: DocBlock[],
): FineUnit[] {
  if (native.items.length > 20000) return [];
  const root = container.getBoundingClientRect();
  if (!root.width || !root.height) return [];
  const dims = viewportDimensions(viewport);
  const pieces: {
    item: TextItem;
    node: ChildNode;
    bbox: PdfRect;
    row: number;
  }[] = [];
  const units: FineUnit[] = [];
  let index = 0,
    row = 0;
  let line: typeof pieces = [];
  const finish = () => {
    if (line.length) {
      units.push({
        kind: "line",
        row,
        text: line.map((p) => p.item.str).join(""),
        bbox: union(line.map((p) => p.bbox)),
      });
      line = [];
    }
    row++;
  };
  for (const item of native.items) {
    if (!("str" in item)) continue;
    const node = divs[index++]?.firstChild;
    if (node?.nodeType === Node.TEXT_NODE && item.str) {
      const bbox = nativeRect(
        item,
        dims.pageX,
        dims.pageY,
        native.styles[item.fontName],
      );
      const piece = { item, node, bbox, row };
      pieces.push(piece);
      if (item.str.trim() || line.length) line.push(piece);
    }
    if (item.hasEOL) finish();
  }
  finish();
  const measure = (
    p: (typeof pieces)[number],
    start: number,
    end: number,
  ): PdfRect => {
    if (start === 0 && end === p.item.str.length) return p.bbox;
    const range = document.createRange();
    range.setStart(p.node, start);
    range.setEnd(p.node, end);
    const r = range.getBoundingClientRect();
    const bbox = rectFromViewport(
      viewport,
      [
        ((r.left - root.left) * viewport.width) / root.width,
        ((r.top - root.top) * viewport.height) / root.height,
      ],
      [
        ((r.right - root.left) * viewport.width) / root.width,
        ((r.bottom - root.top) * viewport.height) / root.height,
      ],
    );
    const tx = p.item.transform;
    if (Math.abs(tx[1]) < 0.001 && p.item.dir !== "ttb") {
      bbox[1] = p.bbox[1];
      bbox[3] = p.bbox[3];
    } else if (Math.abs(tx[0]) < 0.001 || p.item.dir === "ttb") {
      bbox[0] = p.bbox[0];
      bbox[2] = p.bbox[2];
    }
    return bbox;
  };
  // 表のMarkdownでセル境界と列を確定してから文字断片を集約する。TextItemそのものをセルとはみなさない。
  let budget = 20_000_000;
  for (const block of blocks.filter((b) => b.type === "table").slice(0, 200)) {
    const rects = block.prov
      .filter((p) => p.page === Number(container.dataset.page))
      .map((p) => blockRect(p, dims.pageHeight));
    const mine = pieces.filter((p) => rects.some((r) => intersect(r, p.bbox)));
    const text = mine.map((p) => compact(p.item.str)).join("");
    if (text.length > 100000 || block.text.length > 100000) continue;
    let offset = 0;
    const segments = mine.map((p) => {
      const offsets: number[] = [];
      for (let i = 0; i < p.item.str.length; i++)
        if (!/\s/.test(p.item.str[i])) offsets.push(i);
      const start = offset;
      offset += offsets.length;
      return { p, start, end: offset, offsets };
    });
    const rows = tableRows(block.text);
    const candidates = rows
      .flatMap((cells, row) =>
        cells.map((value, column) => ({
          value,
          row,
          column,
          needle: compact(value),
        })),
      )
      .filter((c) => c.needle);
    if (candidates.length > 2000 || candidates.length * text.length > budget)
      continue;
    budget -= candidates.length * text.length;
    const counts = new Map<string, number>();
    for (const c of candidates)
      counts.set(c.needle, (counts.get(c.needle) ?? 0) + 1);
    const located: { start: number; end: number; unit: FineUnit }[] = [];
    for (const cell of [...candidates].sort(
      (a, b) => b.needle.length - a.needle.length,
    )) {
      // 同じ値が複数ある等、対応を一意に確かめられないセルは描かない。
      if (counts.get(cell.needle) !== 1) continue;
      const start = text.indexOf(cell.needle),
        end = start + cell.needle.length;
      if (start < 0 || text.indexOf(cell.needle, start + 1) >= 0) continue;
      const matches = segments.filter(
        (s) => s.end > s.start && s.start < end && s.end > start,
      );
      if (!matches.length) continue;
      if (
        matches.some((part, i) => {
          if (!i) return false;
          const prev = matches[i - 1].p,
            next = part.p;
          const horizontal =
            Math.abs(prev.item.transform[0]) >=
              Math.abs(prev.item.transform[1]) && prev.item.dir !== "ttb";
          const [a, b, c, d] = prev.bbox,
            [e, f, g, h] = next.bbox;
          const size = Math.max(
            1,
            Math.hypot(prev.item.transform[2], prev.item.transform[3]),
            Math.hypot(next.item.transform[2], next.item.transform[3]),
          );
          return horizontal
            ? Math.abs((b + d - f - h) / 2) > size * 0.5 ||
                Math.min(Math.abs(e - c), Math.abs(a - g)) > size * 1.25
            : Math.abs((a + c - e - g) / 2) > size * 0.5 ||
                Math.min(Math.abs(f - d), Math.abs(b - h)) > size * 1.25;
        })
      )
        continue;
      const first = matches[0],
        last = matches[matches.length - 1];
      const firstOffset = first.offsets[start - first.start],
        lastOffset = last.offsets[end - 1 - last.start] + 1;
      if (
        (firstOffset > 0 && !/\s/.test(first.p.item.str[firstOffset - 1])) ||
        (lastOffset < last.p.item.str.length &&
          !/\s/.test(last.p.item.str[lastOffset]))
      )
        continue;
      const bbox = union(
        matches.map((s) =>
          measure(
            s.p,
            s.offsets[Math.max(0, start - s.start)],
            s.offsets[Math.min(s.offsets.length - 1, end - 1 - s.start)] + 1,
          ),
        ),
      );
      located.push({
        start,
        end,
        unit: {
          kind: "cell",
          text: cell.value,
          row: cell.row,
          column: cell.column,
          table: block.ordinal,
          bbox,
        },
      });
    }

    // 同じ文字位置を複数セルへ対応させられる場合は、競合する候補をまとめて除外する。
    const ordered = located.sort((a, b) => a.start - b.start || b.end - a.end);
    let group: typeof located = [];
    let end = -1;
    const finish = () => {
      if (group.length === 1) units.push(group[0].unit);
      group = [];
    };
    for (const candidate of ordered) {
      if (candidate.start >= end) {
        finish();
        end = candidate.end;
      } else end = Math.max(end, candidate.end);
      group.push(candidate);
    }
    finish();
  }
  return units.sort(
    (a, b) =>
      a.kind.localeCompare(b.kind) ||
      (a.table ?? 0) - (b.table ?? 0) ||
      a.row - b.row ||
      (a.column ?? 0) - (b.column ?? 0),
  );
}
export function fineContent(
  units: FineUnit[],
  bbox: PdfRect,
  precision: "line" | "cell",
): FineContent | null {
  const found = units.filter(
    (u) =>
      u.kind === precision &&
      (u.bbox[0] + u.bbox[2]) / 2 >= bbox[0] &&
      (u.bbox[0] + u.bbox[2]) / 2 <= bbox[2] &&
      (u.bbox[1] + u.bbox[3]) / 2 >= bbox[1] &&
      (u.bbox[1] + u.bbox[3]) / 2 <= bbox[3],
  );
  if (!found.length) return null;
  const tooLarge = () =>
    new Error("選択範囲が大きすぎます。範囲を小さくしてください。");
  if (
    found.length > MAX_UNITS ||
    found.reduce((n, u) => n + u.text.length, 0) > MAX_TEXT
  )
    throw tooLarge();
  if (precision === "line")
    return { precision, text: found.map((u) => u.text).join("\n"), tables: [] };
  if (
    new Set(found.map((u) => u.table)).size !== 1 ||
    found.some((u) => u.column === undefined)
  )
    throw new Error(
      "一度に1つの表から、対応を確認できたセルを選んでください。",
    );
  const minRow = Math.min(...found.map((u) => u.row)),
    maxRow = Math.max(...found.map((u) => u.row)),
    minCol = Math.min(...found.map((u) => u.column!)),
    maxCol = Math.max(...found.map((u) => u.column!));
  const height = maxRow - minRow + 1,
    width = maxCol - minCol + 1;
  if (
    height * width > MAX_GRID ||
    found.reduce((n, u) => n + u.text.length * 2, 0) +
      (height + 2) * width * 6 >
      MAX_TEXT
  )
    throw tooLarge();
  const grid = Array.from({ length: height }, () =>
    Array<string>(width).fill(""),
  );
  for (const u of found) grid[u.row - minRow][u.column! - minCol] = u.text;
  const escape = (s: string) => s.replace(/\|/g, "\\|").replace(/\n/g, "<br>");
  const markdown = `| ${Array.from({ length: width }, (_, i) => `列${minCol + i + 1}`).join(" | ")} |\n| ${Array(width).fill("---").join(" | ")} |\n${grid.map((row) => `| ${row.map(escape).join(" | ")} |`).join("\n")}`;
  const cells = found.map((u) => ({
    row: u.row,
    column: u.column!,
    bbox: u.bbox.map((v) => Math.round(v * 100) / 100) as PdfRect,
  }));
  // ページ・版・外枠などの出典メタデータにも余裕を残す。全抜粋の合計は選択確定時に検査する。
  if (JSON.stringify(cells).length > PDF_LOCATOR_LIMIT - 512) throw tooLarge();
  return {
    precision,
    text: grid.map((r) => r.join("\t")).join("\n"),
    tables: [markdown],
    cells,
  };
}
