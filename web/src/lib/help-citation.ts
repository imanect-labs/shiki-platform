/// 製品ヘルプの出典（`help_citation`・`[Hn]`）の本文側の扱い。
///
/// 社内文書の引用（`[n]`・[`linkifyCitations`](./citation.ts)）とは番号の台帳を分けている
/// （サーバの `HelpCitation` は社内文書の Citation と台帳を共有しない）。本文の `[H1]` を
/// チップ用のリンクに変え、回答下の出典一覧と同じ番号で結びつける。
import type { HelpCitation } from "@/lib/chat-api";
import { splitByCode } from "@/lib/citation";

export const HELP_CITE_HREF_PREFIX = "#help-cite-";

/// 本文中の `[Hn]` の並び（`[H1][H3]` は 1 まとまり）。既存の Markdown リンクの一部は除く
/// （`[H1](url)`・`![H1](x)`・参照リンク `[本文][H1]`・参照定義 `[H1]: url`）。
const HELP_CITE_RUN = /(?<![\]!])(?:\[H\d+\])+(?![(\[:])/g;

/// 本文中の `[Hn]` を出典チップ用のリンクに変える（Markdown 用）。対応する出典が無い番号は
/// そのまま残す。コード（フェンス・インラインコード）の中は変えない。
export function linkifyHelpCitations(text: string, citations: readonly HelpCitation[]): string {
  if (citations.length === 0) return text;
  const ids = new Set(citations.map((c) => c.cite_id));
  return splitByCode(text)
    .map(({ code, s }) =>
      code
        ? s
        : s.replace(HELP_CITE_RUN, (run: string) =>
            run.replace(/\[H(\d+)\]/g, (match, digits: string) => {
              const n = Number.parseInt(digits, 10);
              return ids.has(n) ? `[H${n}](${HELP_CITE_HREF_PREFIX}${n})` : match;
            }),
          ),
    )
    .join("");
}

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
