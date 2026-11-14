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

/// 1 件の回答で自動で差し込む図の上限（本文が図で埋まらないように）。
export const MAX_PLACED_HELP_FIGURES = 4;

const LIST_ITEM = /^(\s*)(\d{1,3}[.)]|[-*+])(\s+)\S/;
const FENCE = /^\s*(```|~~~)/;

/// 本文で引いたヘルプ（`[Hn]`）の図のうち、モデルが本文に入れなかったものを、その番号を最初に
/// 引いた手順（番号付き・箇条書きの項目。無ければ段落）の下に差し込む。
///
/// 図を入れるかをモデルに任せると、図のある手順を引いても入れない回がある。図の中の番号（①など）を
/// 本文が使っていれば、図が無いと読めない。表示のときだけ足し、保存した本文は変えない。
/// 生成中は呼ばない（モデルがこれから図を書くかもしれず、差し込んだ図が後から消えて位置が飛ぶ）。
///
/// `texts` は 1 件の回答の本文の区切り（ツールを挟んだ前後）。図は 1 回答で 1 度だけ出す。
/// 表・見出し・コードの中には差し込まない。URL はサーバが組んだもので、表示は回答の画像と同じく
/// 代理取得を通る（{@link helpImageTexts} が出所に含める）。
export function placeHelpFigures(
  texts: readonly string[],
  citations: readonly HelpCitation[],
  max = MAX_PLACED_HELP_FIGURES,
): string[] {
  const all = texts.join("\n");
  // 重複とみなすのは、本文に画像として埋め込んだ図だけ（リンクやコードに書いた URL では図は出ない）。
  const embedded = embeddedImageUrls(all);
  const byId = new Map(citations.map((c) => [c.cite_id, c]));
  const planned = new Set<string>();
  // 区切りごとの差し込み: 行番号 → 足す行。
  const inserts = texts.map(() => new Map<number, string[]>());
  const lines = texts.map((t) => t.split("\n"));
  const order = citedInOrder(all);
  for (const id of order) {
    const c = byId.get(id);
    if (!c) continue;
    const figures = (c.images ?? []).filter(
      (i) => /^https:\/\/[^\s()<>]+$/.test(i.url) && !embedded.has(i.url) && !planned.has(i.url),
    );
    if (figures.length === 0) continue;
    const at = findAnchor(lines, id);
    if (!at) continue;
    const take = figures.slice(0, Math.max(0, max - planned.size));
    if (take.length === 0) break;
    take.forEach((f) => planned.add(f.url));
    const [seg, end, indent] = at;
    const md = take.map((f) => `${" ".repeat(indent ?? 0)}![${figureAlt(f.alt)}](${f.url})`);
    // 段落の後ろなら独立した段落にする。項目の中なら字下げして項目に含める。
    const add = indent === null ? ["", ...md, ...(lines[seg][end + 1]?.trim() ? [""] : [])] : md;
    const prev = inserts[seg].get(end) ?? [];
    inserts[seg].set(end, [...prev, ...add]);
  }
  return lines.map((ls, seg) => {
    if (inserts[seg].size === 0) return texts[seg];
    return ls.flatMap((l, i) => [l, ...(inserts[seg].get(i) ?? [])]).join("\n");
  });
}

/// 本文で引いたヘルプの番号を、最初に出た順に（コードの中は数えない）。
function citedInOrder(text: string): number[] {
  const out: number[] = [];
  for (const { code, s } of splitByCode(text)) {
    if (code) continue;
    for (const m of s.matchAll(/\[H(\d+)\]/g)) {
      const n = Number.parseInt(m[1], 10);
      if (!out.includes(n)) out.push(n);
    }
  }
  return out;
}

/// 本文に画像として埋め込んだ URL（`![説明](URL)`・コードの中は数えない）。
function embeddedImageUrls(text: string): Set<string> {
  const out = new Set<string>();
  for (const { code, s } of splitByCode(text)) {
    if (code) continue;
    for (const m of s.matchAll(/!\[[^\]]*\]\(\s*<?([^\s()<>]+)>?/g)) out.add(m[1]);
  }
  return out;
}

/// その行が、コードの外で `marker` を引いているか。
function citesOutsideCode(line: string, marker: string): boolean {
  return splitByCode(line).some(({ code, s }) => !code && s.includes(marker));
}

const BLOCK_START = /^\s*(\||#|>)/;

/// `[Hn]` を最初に引いた手順の、差し込む位置（区切り・その項目/段落の最後の行・字下げ）。
/// 字下げが `null` なら段落の後ろ。番号付き・箇条書きの項目を段落より優先する（手順の下に置く）。
/// 項目の継続行（字下げした 2 行目以降）で引いたときも、その項目の下に置く。
function findAnchor(lines: string[][], id: number): [number, number, number | null] | null {
  const marker = `[H${id}]`;
  let paragraph: [number, number, number | null] | null = null;
  for (let seg = 0; seg < lines.length; seg++) {
    const ls = lines[seg];
    let fenced = false;
    // いま中にいる項目の、本文の字下げ（項目の外なら null）。
    let itemIndent: number | null = null;
    const endOf = (i: number) => {
      let end = i;
      while (
        end + 1 < ls.length &&
        ls[end + 1].trim() &&
        !LIST_ITEM.test(ls[end + 1]) &&
        !FENCE.test(ls[end + 1]) &&
        !BLOCK_START.test(ls[end + 1])
      )
        end++;
      return end;
    };
    for (let i = 0; i < ls.length; i++) {
      const l = ls[i];
      if (FENCE.test(l)) {
        fenced = !fenced;
        continue;
      }
      if (fenced) continue;
      const item = LIST_ITEM.exec(l);
      if (item) itemIndent = item[1].length + item[2].length + item[3].length;
      else if (l.trim()) {
        const indent = l.length - l.trimStart().length;
        const lazy = i > 0 && ls[i - 1].trim() !== "";
        if (itemIndent !== null && indent < itemIndent && !lazy) itemIndent = null;
      }
      if (BLOCK_START.test(l) || !citesOutsideCode(l, marker)) continue;
      if (itemIndent !== null) return [seg, endOf(i), itemIndent];
      paragraph ??= [seg, endOf(i), null];
    }
  }
  return paragraph;
}

/// 図の説明を Markdown の画像の説明に入れられる形にする（括弧・改行で記法を壊さない）。
function figureAlt(alt: string): string {
  return alt.replace(/[\[\]\r\n]/g, " ").trim();
}
