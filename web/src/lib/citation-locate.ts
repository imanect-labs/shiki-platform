/// 引用箇所を原本のエディタ・ビューアで探すための手がかり（ディープリンク・#508）。
///
/// 原本のバイト位置には戻さない。エディタ（ノートの TipTap + Yjs、Collabora）は共同編集で
/// 内容が変わるので、オフセットではなく**一節の本文**で探す（W3C TextQuoteSelector の考え方）。
///
/// - **文書の本文は URL に載せない**（アクセスログ・履歴・共有リンクに機密の断片が残るため）。
///   URL には引用の識別子（`cite` = chunk_id）だけを載せ、一節の先頭（find）・末尾（end）・
///   見出し（h）はクリック時に localStorage へ置いて渡す。リンクだけを共有された人は、
///   一節を知らないまま文書の先頭で開く（本文は権限のある画面でしか見えない）。
/// - ノートは先頭から末尾までをハイライトし、見つからなければ見出しまで移る。
///   Collabora は先頭を検索する。
/// - PDF は版・ページ・枠（`v` / `page` / `box`。数値だけ）で開く。
import * as React from "react";

import type { Citation } from "@/lib/chat-api";

/// `find` の長さ（文字）。1 段落に収まりやすく、Collabora の検索にも通る長さにする。
const FIND_CHARS = 48;
/// `end` の長さ（文字）。
const END_CHARS = 24;

/// 引用の一節（位置情報の無い旧データは抜粋で代える）。
function quoteText(c: Citation): string {
  // 引用では quote.exact を空で送る（本文は snippet と同じ）。
  return (c.quote?.exact || c.snippet || "").trim();
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

/// 引用箇所を探す手がかり（本文の断片なので URL には載せない）。
export type CiteLocator = { find: string | null; end: string | null; heading: string | null };

export function citeLocator(c: Citation): CiteLocator {
  const text = quoteText(c);
  const find = findPhrase(text) || null;
  const end = endPhrase(text);
  return { find, end: end && end !== find ? end : null, heading: (c.heading_path ?? []).at(-1) ?? null };
}

const STORE_PREFIX = "shiki:cite:";
/// 受け渡しの有効期間。開いた直後に読むだけなので短くてよい。
const STORE_TTL_MS = 10 * 60_000;

/// クリック時に手がかりを置く（開いた先の画面が `cite` で読む）。
export function stashCiteLocator(c: Citation): void {
  try {
    const now = Date.now();
    // 古い受け渡しを掃除する（溜めない）。
    for (let i = localStorage.length - 1; i >= 0; i--) {
      const k = localStorage.key(i);
      if (!k?.startsWith(STORE_PREFIX)) continue;
      const at = Number(JSON.parse(localStorage.getItem(k) ?? "{}").at ?? 0);
      if (now - at > STORE_TTL_MS) localStorage.removeItem(k);
    }
    localStorage.setItem(STORE_PREFIX + c.chunk_id, JSON.stringify({ ...citeLocator(c), at: now }));
  } catch {
    /* 保存できない（プライベートモード等）なら、文書の先頭で開くだけ */
  }
}

/// `cite` の手がかりを読む（無い・期限切れなら null）。描画後に読む（SSR では触らない）。
export function useCiteLocator(key: string | null): CiteLocator | null {
  const [loc, setLoc] = React.useState<CiteLocator | null>(null);
  React.useEffect(() => {
    if (!key) {
      setLoc(null);
      return;
    }
    try {
      const raw = JSON.parse(localStorage.getItem(STORE_PREFIX + key) ?? "null");
      setLoc(raw && Date.now() - Number(raw.at ?? 0) <= STORE_TTL_MS ? raw : null);
    } catch {
      setLoc(null);
    }
  }, [key]);
  return loc;
}

/// 原本を開く URL に足すクエリ（ノート・Office・PDF で共通・本文は含めない）。
export function citeLocatorQuery(c: Citation): string {
  const sp = new URLSearchParams();
  sp.set("cite", c.chunk_id);
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
