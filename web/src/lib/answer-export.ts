/// チャットの回答の書き出し（#1048）。ノート・Word（ドライブ）・md・docx・pdf の素になる
/// Markdown とファイル名をここで組み立てる。
///
/// - 本文の `[n]` はそのまま残し、末尾に「出典」の一覧を付ける（書き出した先では引用チップに
///   ならないので、番号と出所の対応を文字で残す）。閲覧できない出典（withheld）は一覧に出さない。
/// - ```ui の図はノート・md ではそのまま残す（ノートは図として描ける）。Word は図を描けないので
///   プレースホルダへ置き換える（生の図の定義を Word に載せない）。

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

/// Word 用の Markdown。```ui の図は Word に描けないため、プレースホルダの引用に置き換える。
/// 図として描かれるフェンス（``` / ~~~ と ui / yui。種類の付いた ```ui bar・```yui:Bar も）を対象にする。
export function docxMarkdown(md: string): string {
  const out: string[] = [];
  for (const block of scanFences(md)) {
    if (block.kind === "fence" && isFigureInfo(block.info)) {
      out.push("> 図はこのファイルに含まれません。チャットの回答で確認してください。");
    } else {
      out.push(...block.lines);
    }
  }
  return out.join("\n");
}

type FenceBlock = { kind: "text"; lines: string[] } | { kind: "fence"; info: string; lines: string[] };

/// 本文を「フェンスの外の行」と「フェンス（開きから閉じまで）」に分ける（CommonMark のとおり、
/// 閉じは開きと同じ文字で同じ長さ以上・情報文字列なし。閉じが無ければ末尾まで）。
function scanFences(md: string): FenceBlock[] {
  const blocks: FenceBlock[] = [];
  let open: { ch: string; len: number; block: Extract<FenceBlock, { kind: "fence" }> } | null = null;
  let text: string[] = [];
  for (const line of md.split("\n")) {
    if (open) {
      open.block.lines.push(line);
      const close = /^ {0,3}(`{3,}|~{3,})[ \t]*$/.exec(line);
      if (close && close[1][0] === open.ch && close[1].length >= open.len) open = null;
      continue;
    }
    const m = /^ {0,3}(`{3,}|~{3,})(.*)$/.exec(line);
    // バッククォートのフェンスの情報文字列にはバッククォートを含められない（インラインのコード）。
    if (m && !(m[1][0] === "`" && m[2].includes("`"))) {
      if (text.length > 0) blocks.push({ kind: "text", lines: text });
      text = [];
      const block = { kind: "fence" as const, info: m[2].trim(), lines: [line] };
      blocks.push(block);
      open = { ch: m[1][0], len: m[1].length, block };
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
