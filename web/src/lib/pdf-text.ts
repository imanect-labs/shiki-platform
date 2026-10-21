/// PDF の文字（テキスト層・文書内検索）。
///
/// ページの文字は pdf.js の `getTextContent` から取り、テキスト層の描画と検索で**同じ結果**を
/// 使う（テキスト層の i 番目の span と、検索が数える i 番目の文字片を一致させるため）。
/// 取得は重いので、文書ごと・ページごとに覚えておく。
import type { PDFDocumentProxy } from "pdfjs-dist";
import type { TextContent, TextItem } from "pdfjs-dist/types/src/display/api";

const contents = new WeakMap<PDFDocumentProxy, Map<number, Promise<TextContent>>>();

/// ページの文字（キャッシュ付き）。
export function pageTextContent(doc: PDFDocumentProxy, pageNumber: number): Promise<TextContent> {
  let byPage = contents.get(doc);
  if (!byPage) {
    byPage = new Map();
    contents.set(doc, byPage);
  }
  let hit = byPage.get(pageNumber);
  if (!hit) {
    hit = doc.getPage(pageNumber).then((p) => p.getTextContent());
    hit.catch(() => byPage.delete(pageNumber));
    byPage.set(pageNumber, hit);
  }
  return hit;
}

/// 文字片（テキスト層の span 1 つ）が、ページ全体の文字列のどこにあるか。
export type TextSegment = { start: number; end: number };

/// ページの検索用文字列と、文字片ごとの位置。行末（hasEOL）には空白を 1 つ挟む
/// （行をまたぐ語句も「空白区切り」で探せるように）。空白は文字片に属さない。
export type PageText = { text: string; segments: TextSegment[] };

export function buildPageText(content: TextContent): PageText {
  let text = "";
  const segments: TextSegment[] = [];
  for (const item of content.items) {
    if (!("str" in item)) continue;
    const t = item as TextItem;
    const start = text.length;
    text += normalize(t.str);
    segments.push({ start, end: text.length });
    if (t.hasEOL) text += " ";
  }
  return { text, segments };
}

/// 照合用に 1 文字ずつ正規化する（全角英数→半角・大文字→小文字）。長さが変わる変換は
/// しない（元の文字位置へ戻せなくなるため）。
export function normalize(s: string): string {
  let out = "";
  for (const ch of s) {
    const n = ch.normalize("NFKC");
    // 長さ（UTF-16 単位）が変わる変換は採らない（合字の分解・サロゲートペアの変化など）。
    const one = n.length === ch.length ? n : ch;
    const low = one.toLowerCase();
    out += low.length === one.length ? low : one;
  }
  return out;
}

/// 1 件の一致（ページ内の文字位置）。
export type PdfMatch = { page: number; start: number; end: number };

/// ページ文字列の中の一致位置（重ならないように左から順に）。
export function findInText(text: string, query: string): Array<[number, number]> {
  const q = normalize(query.trim());
  if (!q) return [];
  const out: Array<[number, number]> = [];
  let from = 0;
  for (;;) {
    const i = text.indexOf(q, from);
    if (i < 0) break;
    out.push([i, i + q.length]);
    from = i + q.length;
  }
  return out;
}

/// 一致範囲を、文字片ごとの [片番号, 片内の開始, 片内の終了] に分ける（テキスト層で範囲を作る用）。
/// 行末の空白にだけ掛かる部分は捨てる。
export function matchPieces(
  segments: readonly TextSegment[],
  start: number,
  end: number,
): Array<[number, number, number]> {
  const out: Array<[number, number, number]> = [];
  for (let i = 0; i < segments.length; i++) {
    const s = segments[i];
    if (s.end <= start) continue;
    if (s.start >= end) break;
    const a = Math.max(start, s.start) - s.start;
    const b = Math.min(end, s.end) - s.start;
    if (b > a) out.push([i, a, b]);
  }
  return out;
}

/// 文書全体を探す。ページ順に読み、途中経過を `onProgress` で返す（大きな PDF でも
/// 最初の一致をすぐ出すため）。`signal` で打ち切れる。
export async function searchDocument(
  doc: PDFDocumentProxy,
  query: string,
  opts: { signal: AbortSignal; onProgress: (matches: PdfMatch[], scanned: number) => void },
): Promise<PdfMatch[]> {
  const matches: PdfMatch[] = [];
  for (let page = 1; page <= doc.numPages; page++) {
    if (opts.signal.aborted) break;
    const { text } = buildPageText(await pageTextContent(doc, page));
    for (const [start, end] of findInText(text, query)) matches.push({ page, start, end });
    // 数ページごとに途中経過を返す（毎ページだと再描画が多い）。
    if (page % 8 === 0 || page === doc.numPages) opts.onProgress([...matches], page);
  }
  return matches;
}
