/// 引用箇所を原本のエディタ・ビューアで探すための手がかり（ディープリンク・#508）。
///
/// 原本のバイト位置には戻さない。エディタ（ノートの TipTap + Yjs、Collabora）は共同編集で
/// 内容が変わるので、オフセットではなく**一節の本文**で探す（W3C TextQuoteSelector の考え方）。
///
/// - URL には一節の先頭（`find`）と末尾（`end`）だけを載せる（長い一節で URL を膨らませない）。
///   ノートは先頭から末尾までをハイライトする。Collabora は先頭を検索する。
/// - 見つからなければ見出し（`h`）まで移る。
/// - PDF は版・ページ・枠（`v` / `page` / `box`）で開く（座標が確実に取れる唯一の形式）。
import type { Citation } from "@/lib/chat-api";

/// `find` の長さ（文字）。1 段落に収まりやすく、Collabora の検索にも通る長さにする。
const FIND_CHARS = 48;
/// `end` の長さ（文字）。
const END_CHARS = 24;

/// 引用の一節（位置情報の無い旧データは抜粋で代える）。
function quoteText(c: Citation): string {
  return (c.quote?.exact ?? c.snippet ?? "").trim();
}

/// 一節の最初の段落の先頭（検索語）。段落をまたぐ文字列は Collabora の検索に当たらないため。
export function findPhrase(text: string, max = FIND_CHARS): string {
  const first = text.split(/\n+/).map((s) => s.trim()).find((s) => s.length > 0) ?? "";
  return Array.from(first).slice(0, max).join("");
}

/// 一節の最後の段落の末尾。
function endPhrase(text: string): string {
  const parts = text.split(/\n+/).map((s) => s.trim()).filter((s) => s.length > 0);
  const last = parts[parts.length - 1] ?? "";
  const chars = Array.from(last);
  return chars.slice(Math.max(0, chars.length - END_CHARS)).join("");
}

/// 原本を開く URL に足すクエリ（ノート・Office・PDF で共通）。
export function citeLocatorQuery(c: Citation): string {
  const sp = new URLSearchParams();
  const text = quoteText(c);
  const find = findPhrase(text);
  if (find) sp.set("find", find);
  const end = endPhrase(text);
  if (end && end !== find) sp.set("end", end);
  const heading = (c.heading_path ?? []).at(-1);
  if (heading) sp.set("h", heading);
  if (typeof c.version === "number") sp.set("v", String(c.version));
  const page = c.boxes?.[0]?.page ?? c.page;
  if (page != null) sp.set("page", String(page));
  for (const b of (c.boxes ?? []).slice(0, 8)) {
    sp.append("box", [b.page, ...b.bbox.map((x) => Math.round(x * 10) / 10), b.origin === "top_left" ? "t" : "b"].join(","));
  }
  return sp.toString();
}

/// PDF の枠（`box` クエリ）を読み戻す。
export type CiteBox = { page: number; bbox: [number, number, number, number]; topLeft: boolean };

export function parseCiteBoxes(values: readonly string[]): CiteBox[] {
  const out: CiteBox[] = [];
  for (const v of values) {
    const [page, l, t, r, b, o] = v.split(",");
    const nums = [page, l, t, r, b].map(Number);
    if (nums.some((n) => !Number.isFinite(n))) continue;
    out.push({ page: nums[0], bbox: [nums[1], nums[2], nums[3], nums[4]], topLeft: o === "t" });
  }
  return out;
}

/// 本文（エディタのテキストを 1 文字ずつ並べたもの）の中で一節を探す。
///
/// 空白・改行の違いは無視する（エディタの段落区切りとパーサの正規化が一致しないため）。
/// 戻り値は `chars` の添字の範囲 `[start, end)`。`end` が見つからなければ `find` の範囲だけ。
export function locatePhrase(
  chars: readonly string[],
  find: string,
  end?: string | null,
): [number, number] | null {
  const idx: number[] = [];
  let flat = "";
  chars.forEach((ch, i) => {
    if (/\s/.test(ch)) return;
    flat += ch;
    for (let k = 0; k < ch.length; k++) idx.push(i);
  });
  const norm = (s: string) => s.replace(/\s+/g, "");
  const f = norm(find);
  if (!f) return null;
  const at = flat.indexOf(f);
  if (at < 0) return null;
  let stop = at + f.length;
  const e = end ? norm(end) : "";
  if (e) {
    const endAt = flat.indexOf(e, at);
    // 一節が 1 チャンク（〜600 字）を大きく超えることはない。遠すぎる一致は別の箇所とみなす。
    if (endAt >= 0 && endAt - at < 2000) stop = Math.max(stop, endAt + e.length);
  }
  return [idx[at], idx[stop - 1] + 1];
}
