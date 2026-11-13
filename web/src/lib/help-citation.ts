/// 製品ヘルプの出典（`help_citation`・`[Hn]`）の本文側の扱い。
///
/// 社内文書の引用（`[n]`）とは番号の台帳を分けている（サーバの `HelpCitation` は社内文書の
/// Citation と台帳を共有しない）。本文の `[H1]` は [`linkifyCitations`](./citation.ts) が
/// `#help-cite-1` のリンクに変え、回答下の出典一覧と同じ番号で結びつける。
import type { HelpCitation } from "@/lib/chat-api";
import { HELP_CITE_HREF_PREFIX, splitByCode } from "@/lib/citation";

/// `#help-cite-n` から番号を取り出す（それ以外は `null`）。
export function helpCiteNumberFromHref(href: string): number | null {
  if (!href.startsWith(HELP_CITE_HREF_PREFIX)) return null;
  const n = Number.parseInt(href.slice(HELP_CITE_HREF_PREFIX.length), 10);
  return Number.isFinite(n) ? n : null;
}

/// 開いてよいヘルプの URL か（サーバが組み立てた公開ヘルプの https だけを通す）。
export function safeHelpUrl(url: string): string | null {
  try {
    const u = new URL(url);
    return u.protocol === "https:" ? u.toString() : null;
  } catch {
    return null;
  }
}

/// 番号の重複を除き、番号順に並べる（同じ箇所が 2 回届いても 1 件にする）。
export function uniqueHelpCitations(citations: readonly HelpCitation[]): HelpCitation[] {
  return [...new Map(citations.map((c) => [c.cite_id, c])).values()].sort((a, b) => a.cite_id - b.cite_id);
}

export type HelpPage = { key: string; title: string; url: string | null; passages: HelpCitation[] };

/// 出典をヘルプのページごとにまとめる（URL の `#見出し` を除いた単位・最初に引いた順）。
export function groupHelpPages(citations: readonly HelpCitation[]): HelpPage[] {
  const pages = new Map<string, HelpPage>();
  for (const c of uniqueHelpCitations(citations)) {
    const url = safeHelpUrl(c.url);
    const key = url ? url.split("#")[0] : `title:${c.title}`;
    const page = pages.get(key);
    if (page) page.passages.push(c);
    else pages.set(key, { key, title: c.title, url: url ? key : null, passages: [c] });
  }
  return [...pages.values()];
}

/// 本文で引いたヘルプの番号（コードの中の `[Hn]` は数えない）。
export function citedHelpIds(text: string): Set<number> {
  const ids = new Set<number>();
  for (const { code, s } of splitByCode(text)) {
    if (code) continue;
    for (const m of s.matchAll(/\[H(\d+)\]/g)) ids.add(Number.parseInt(m[1], 10));
  }
  return ids;
}
