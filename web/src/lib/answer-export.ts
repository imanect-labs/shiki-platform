/// チャットの回答の書き出し（#1048）。ノート・Word（ドライブ）・md・docx・pdf の素になる
/// Markdown とファイル名をここで組み立てる。
///
/// - 本文の `[n]` はそのまま残し、末尾に「出典」の一覧を付ける（書き出した先では引用チップに
///   ならないので、番号と出所の対応を文字で残す）。閲覧できない出典（withheld）は一覧に出さない。
/// - ```ui の図はノート・md ではそのまま残す（ノートは図として描ける）。Word は図を描けないので
///   プレースホルダへ置き換える（生の図の定義を Word に載せない）。
/// - 画像は md ではそのまま、ノート・Word ではリンクにする（ノートは画像を取り込まず、Word の変換側に
///   外部の画像を取りに行かせない）。

import type { Citation } from "@/lib/chat-api";
import { citationLocator, groupCitations, isWebCitation, safeWebUrl } from "@/lib/citation";

/// 出所の表示名を返す関数（チャットの出典一覧と同じもの・`MessageCitations.nameOf`）。
export type SourceName = (c: Pick<Citation, "node_id" | "web">) => string;

/// 回答の本文に「出典」の一覧を付けた Markdown。出典が無ければ本文だけ。
export function answerMarkdown(text: string, citations: readonly Citation[], nameOf: SourceName): string {
  const body = text.trimEnd();
  const sources = sourcesMarkdown(text, citations, nameOf);
  return sources ? `${body}\n\n${sources}\n` : `${body}\n`;
}

/// 「出典」の節（見出しと箇条書き）。本文で使った出所だけを、本文で最初に引いた順に並べる。
export function sourcesMarkdown(
  text: string,
  citations: readonly Citation[],
  nameOf: SourceName,
): string | null {
  const { docs, hasMarkers } = groupCitations(citations, text);
  if (docs.length === 0) return null;
  const lines = docs.map((d) => {
    const first = d.passages[0].citation;
    const name = escapeInline(nameOf(first));
    const url = isWebCitation(first) ? linkDestination(safeWebUrl(first.web.url)) : null;
    const label = url ? `[${name}](${url})` : name;
    // 箇所ごとの番号と場所（Web はページそのものなので場所を繰り返さない）。
    const places = d.passages
      .map((p) => {
        const loc = d.web ? "" : citationLocator(p.citation);
        const nums = hasMarkers ? p.ns.map((n) => `[${n}]`).join("") : "";
        return [nums, escapeInline(loc)].filter(Boolean).join(" ");
      })
      .filter(Boolean);
    return places.length > 0 ? `- ${label}（${places.join("、")}）` : `- ${label}`;
  });
  return `## 出典\n\n${lines.join("\n")}`;
}

/// Word 用の Markdown。```ui の図は Word に描けないため、プレースホルダに置き換える（引用・リストの
/// 中の図も、その入れ物の中に置く）。画像はリンクにする（変換側に外部の画像を取りに行かせない）。
export function docxMarkdown(md: string): string {
  const out: string[] = [];
  for (const block of scanFences(md)) {
    if (block.kind === "fence" && isFigureInfo(block.info)) {
      out.push(`${block.prefix}${FIGURE_PLACEHOLDER}`);
    } else if (block.kind === "fence") {
      out.push(...block.lines);
    } else {
      out.push(...block.lines.map(imageAsLink));
    }
  }
  return out.join("\n");
}

/// ノート用の Markdown。ノートの取り込みは画像を代替テキストにしてしまう（URL が消える）ので、
/// 画像はリンクにして元の画像へたどれるようにする。図（```ui）はノートで描けるのでそのまま。
export function noteMarkdown(md: string): string {
  return scanFences(md)
    .flatMap((block) => (block.kind === "fence" ? block.lines : block.lines.map(imageAsLink)))
    .join("\n");
}

const FIGURE_PLACEHOLDER = "（図はこのファイルに含まれません。チャットの回答で確認してください。）";

/// `![代替](URL "題")` を `[代替](URL)` にする（代替が空なら「画像」）。インラインコードの中は触らない。
function imageAsLink(line: string): string {
  return line
    .split(/(`+[^`]*`+)/)
    .map((part, i) =>
      i % 2 === 1
        ? part
        : part.replace(
            /!\[([^\]]*)\]\(\s*(<[^>]*>|[^\s)]+)(?:\s+(?:"[^"]*"|'[^']*'))?\s*\)/g,
            (_m, alt: string, url: string) => `[${alt.trim() || "画像"}](${url})`,
          ),
    )
    .join("");
}

type FenceBlock =
  | { kind: "text"; lines: string[] }
  | { kind: "fence"; info: string; prefix: string; lines: string[] };

/// 行頭の入れ物（引用の `>`・リストの記号・字下げ）。フェンスはこの後ろに置かれていても開きになる。
const CONTAINER = String.raw`((?:[ \t]*>[ \t]?|[ \t]*(?:[-*+]|\d{1,9}[.)])[ \t]+)*[ \t]*)`;
const OPEN_FENCE = new RegExp(`^${CONTAINER}(\`{3,}|~{3,})(.*)$`);
/// 閉じの字下げは問わない（リストの中のフェンスは、記号の行より後ろの行で字下げして書かれる）。
const CLOSE_FENCE = /^[ \t]*(`{3,}|~{3,})[ \t]*$/;
const QUOTE_MARKER = /^[ \t]{0,3}>[ \t]?/;

/// フェンスを開いた行の入れ物。引用の深さと、リストの中なら本文の字下げ（記号の後ろの桁）。
type Container = { quotes: number; listIndent: number };

function containerOf(prefix: string): Container {
  let quotes = 0;
  let listIndent = 0;
  let rest = prefix;
  for (;;) {
    const q = QUOTE_MARKER.exec(rest);
    if (q) {
      quotes += 1;
      listIndent = 0;
      rest = rest.slice(q[0].length);
      continue;
    }
    const l = /^[ \t]*(?:[-*+]|\d{1,9}[.)])[ \t]+/.exec(rest);
    if (l) {
      listIndent += l[0].length;
      rest = rest.slice(l[0].length);
      continue;
    }
    return { quotes, listIndent };
  }
}

/// 入れ物の中の行なら、入れ物の記号を外した残りを返す。入れ物を抜けた行（`>` が足りない・リストの
/// 字下げに届かない）なら null（そのフェンスはこの行の手前で閉じる・CommonMark のとおり）。
function insideContainer(line: string, c: Container): string | null {
  let rest = line;
  for (let i = 0; i < c.quotes; i++) {
    const q = QUOTE_MARKER.exec(rest);
    if (!q) return null;
    rest = rest.slice(q[0].length);
  }
  if (c.listIndent > 0 && rest.trim() !== "") {
    const indent = /^[ \t]*/.exec(rest)?.[0] ?? "";
    if (indent.replace(/\t/g, "    ").length < c.listIndent) return null;
    return rest.slice(indent.length);
  }
  return rest;
}

/// 本文を「フェンスの外の行」と「フェンス（開きから閉じまで）」に分ける（CommonMark のとおり、
/// 閉じは開きと同じ文字で同じ長さ以上・情報文字列なし。閉じが無ければ入れ物の終わりか本文の末尾まで）。
/// 引用・リストの中のフェンスも拾う（チャットはそれも図として描く）。
function scanFences(md: string): FenceBlock[] {
  const blocks: FenceBlock[] = [];
  let open: {
    ch: string;
    len: number;
    container: Container;
    block: Extract<FenceBlock, { kind: "fence" }>;
  } | null = null;
  let text: string[] = [];
  for (const line of md.split("\n")) {
    if (open) {
      const inner = insideContainer(line, open.container);
      if (inner !== null) {
        open.block.lines.push(line);
        const close = CLOSE_FENCE.exec(inner);
        if (close && close[1][0] === open.ch && close[1].length >= open.len) open = null;
        continue;
      }
      // 入れ物を抜けた: フェンスはここで閉じ、この行は外の行として読み直す。
      open = null;
    }
    const m = OPEN_FENCE.exec(line);
    // バッククォートのフェンスの情報文字列にはバッククォートを含められない（インラインのコード）。
    if (m && !(m[2][0] === "`" && m[3].includes("`"))) {
      if (text.length > 0) blocks.push({ kind: "text", lines: text });
      text = [];
      const block = { kind: "fence" as const, info: m[3].trim(), prefix: m[1], lines: [line] };
      blocks.push(block);
      open = { ch: m[2][0], len: m[2].length, container: containerOf(m[1]), block };
      continue;
    }
    text.push(line);
  }
  if (text.length > 0) blocks.push({ kind: "text", lines: text });
  return blocks;
}

/// 図として描かれるフェンスの情報文字列か（ui / yui、`ui bar`・`yui:Bar` のような種類付きも）。
function isFigureInfo(info: string): boolean {
  return /^(?:ui|yui)(?:$|[\s:])/i.test(info);
}

/// リンク先に入れる URL の、Markdown のリンク記法を閉じてしまう文字を符号化する
/// （`https://a/x)[偽](https://b` のような URL で、出典の一覧に別のリンクを差し込ませない）。
function linkDestination(url: string | null): string | null {
  return url ? url.replace(/[()[\]<>\s]/g, (c) => `%${c.charCodeAt(0).toString(16).toUpperCase().padStart(2, "0")}`) : null;
}

const MAX_NAME = 60;

/// 書き出すファイルの名前（拡張子なし）。回答の最初の見出し → 会話の題名 → 「回答_日付」の順。
export function answerFileName(text: string, threadTitle: string | null, now = new Date()): string {
  // 見出しが記法だけ（`## [1]` など）で空になったら、会話の題名へ進む。
  const clean = sanitizeFileName(firstHeading(text) ?? "") || sanitizeFileName(threadTitle ?? "");
  if (clean) return clean;
  const y = now.getFullYear();
  const m = String(now.getMonth() + 1).padStart(2, "0");
  const d = String(now.getDate()).padStart(2, "0");
  return `回答_${y}-${m}-${d}`;
}

/// コードブロックの外にある最初の見出し（`#`〜`###`）の文字。強調・コード・リンク・引用番号の
/// 記法だけを落とす（`file_name`・`10~20件` のように名前に含まれる記号は残す）。
function firstHeading(text: string): string | null {
  for (const block of scanFences(text)) {
    if (block.kind !== "text") continue;
    for (const line of block.lines) {
      const h = /^ {0,3}#{1,3}[ \t]+(.+?)(?:[ \t]+#+)?[ \t]*$/.exec(line);
      if (!h) continue;
      return h[1]
        .replace(/\[([^\]]*)\]\([^)]*\)/g, "$1")
        .replace(/\[\d+\]/g, "")
        .replace(/(\*\*|__|~~)(.+?)\1/g, "$2")
        .replace(/(^|[^\w*])\*(\S(?:.*?\S)?)\*(?!\w)/g, "$1$2")
        .replace(/`+/g, "")
        .trim();
    }
  }
  return null;
}

/// ファイル名に使えない文字を落とし、長すぎる名前を切る。
function sanitizeFileName(name: string): string {
  const s = name
    .replace(/[\\/:*?"<>|\u0000-\u001f]/g, " ")
    .replace(/\s+/g, " ")
    .trim()
    .replace(/^\.+/, "");
  return Array.from(s).slice(0, MAX_NAME).join("").trim();
}

/// 一覧の 1 行に入れる文字の Markdown 記法を無効化する（ファイル名の `*`・`[` などで崩さない）。
function escapeInline(s: string): string {
  return s.replace(/([\\`*_[\]<>#|])/g, "\\$1").replace(/\n+/g, " ");
}
