/// 引用（doc_search のソース）から Drive ビューアへのディープリンクを組み立てる。
/// ビューアは `?page`(PDF) と `?hl`(本文ハイライト) を解釈して該当箇所を開く。
import type { Citation } from "@/lib/chat-api";

/// 引用 → ビューア URL（`/drive/file/{node_id}?page=&hl=&chunk=`）。
export function citationHref(c: {
  node_id: string;
  page?: number | null;
  snippet?: string;
  chunk_id?: string;
}): string {
  const params = new URLSearchParams();
  if (c.page != null) params.set("page", String(c.page));
  // ハイライトは長すぎると一致しにくいので先頭の特徴的な部分だけ渡す。
  if (c.snippet) {
    const hl = c.snippet.replace(/\s+/g, " ").trim().slice(0, 60);
    if (hl) params.set("hl", hl);
  }
  if (c.chunk_id) params.set("chunk", c.chunk_id);
  const qs = params.toString();
  return `/drive/file/${c.node_id}${qs ? `?${qs}` : ""}`;
}

/// 本文中の `[n]` 引用マーカーを、対応する引用へのリンクに変換する（Markdown 用）。
/// 範囲外の番号やマッチしない `[n]` はそのまま残す。リンク先は `citationHref`。
export function linkifyCitations(text: string, citations: Citation[]): string {
  if (citations.length === 0) return text;
  return text.replace(/\[(\d+)\]/g, (match, digits) => {
    const n = Number.parseInt(digits, 10);
    const c = citations[n - 1];
    if (!c) return match;
    // リンクテキストは番号のみ（Markdown の a レンダラが上付きチップに描画する）。
    return `[${n}](${citationHref(c)})`;
  });
}
