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

/// ヘルプの図を、出所の本文と同じ形（`![](url)`）にする（lib/answer-images の出所に足す）。
/// 図の URL はサーバが同梱コーパスの相対パスと設定の origin から組んだもの。抜粋の文に書かれた
/// 画像は数えない。表示は回答の画像の代理取得（サーバ側でも同じ出所を判定する）を通る。
export function helpImageTexts(citations: readonly HelpCitation[]): string[] {
  return citations.flatMap((c) => (c.images ?? []).map((i) => `![](${i.url})`));
}

/// 回答のリンクを確認なしで開いてよいヘルプのホスト（#1143）。URL はサーバが組んだもので、
/// モデルや文書が書いた文字列ではない。
export function helpLinkHosts(citations: readonly HelpCitation[]): string[] {
  const out = new Set<string>();
  for (const c of citations) {
    const url = safeHelpUrl(c.url);
    if (url) out.add(new URL(url).host.toLowerCase().replace(/\.$/, "").replace(/^www\./, ""));
  }
  return [...out];
}

/// 回答で引いたヘルプのページに対応する Shiki の画面（パスの重複を除き、引いた順）。
export function helpAppLinks(citations: readonly HelpCitation[]): { path: string; label: string }[] {
  const out = new Map<string, { path: string; label: string }>();
  for (const c of citations) {
    for (const l of c.app_links ?? []) {
      // サーバの検証と同じく、同一オリジンの静的なパスだけ。
      if (/^\/(?:[a-z0-9-]+(?:\/[a-z0-9-]+)*)?$/.test(l.path) && !out.has(l.path)) out.set(l.path, l);
    }
  }
  return [...out.values()];
}
