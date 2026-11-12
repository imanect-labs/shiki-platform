import type { PDFDocumentProxy } from "pdfjs-dist";
import { assertPdfSource } from "./pdf-excerpts";

/// 0始まりの順序を検証。空・重複・範囲外を暗黙補正せず、操作をそのまま確かめられる形にする。
export function validatePageOrder(order: number[], count: number) {
  if (!order.length) throw new Error("少なくとも1ページ残してください。");
  if (
    new Set(order).size !== order.length ||
    order.some((i) => !Number.isInteger(i) || i < 0 || i >= count)
  )
    throw new Error("ページの順序が不正です。最初からやり直してください。");
}
export function moveBefore<T>(items: T[], from: T, to: T): T[] {
  const a = items.indexOf(from),
    b = items.indexOf(to);
  if (a < 0 || b < 0 || a === b) return items;
  const next = [...items];
  next.splice(a, 1);
  next.splice(b, 0, from);
  return next;
}
/// 並べ替えだけなら同じページ参照を使い、フォーム・しおりを維持する。
/// 削除を含む場合は抽出と同様、新しいPDFへ残すページ内容だけをコピーする。
export async function organizePdf(doc: PDFDocumentProxy, order: number[]) {
  assertPdfSource(doc);
  validatePageOrder(order, doc.numPages);
  const { PDFDocument } = await import("pdf-lib");
  const source = await PDFDocument.load(await doc.getData());
  let output = source;
  if (order.length === doc.numPages) {
    const pages = [...source.getPages()];
    for (let i = pages.length - 1; i >= 0; i--) source.removePage(i);
    for (const index of order) source.addPage(pages[index]);
  } else {
    output = await PDFDocument.create();
    const { copyKeptPages } = await import("./pdf-page-copy");
    for (const p of await copyKeptPages(source, output, order))
      output.addPage(p);
  }
  const bytes = await output.save();
  assertPdfSource(doc);
  return bytes;
}
/// 入力順を保って結合する。全入力の権限世代を保存の前後で照合する。
export async function mergePdfs(docs: PDFDocumentProxy[]) {
  if (docs.length < 2 || docs.length > 20)
    throw new Error("結合するPDFを追加してください。");
  if (docs.reduce((n, d) => n + d.numPages, 0) > 1000)
    throw new Error("一度に結合できるのは合計1,000ページまでです。");
  const { PDFDocument } = await import("pdf-lib");
  const output = await PDFDocument.create();
  let total = 0;
  for (const doc of docs) {
    docs.forEach(assertPdfSource);
    const bytes = await doc.getData();
    total += bytes.byteLength;
    if (total > 100 * 1024 * 1024)
      throw new Error("一度に結合できるPDFは合計100MBまでです。");
    const source = await PDFDocument.load(bytes);
    for (const page of await output.copyPages(source, source.getPageIndices()))
      output.addPage(page);
  }
  const bytes = await output.save();
  docs.forEach(assertPdfSource);
  return bytes;
}

export function parsePageRange(input: string, count: number): number[] {
  const pages: number[] = [];
  for (const part of input.split(/[,、]/)) {
    const m = part.trim().match(/^(\d+)(?:\s*[-–]\s*(\d+))?$/);
    if (!m) throw new Error("ページは「1-3, 5」のように入力してください。");
    const a = Number(m[1]),
      b = Number(m[2] ?? m[1]);
    if (a < 1 || b < a || b > count)
      throw new Error(`1〜${count} のページを指定してください。`);
    for (let p = a; p <= b; p++) if (!pages.includes(p - 1)) pages.push(p - 1);
  }
  return pages;
}
