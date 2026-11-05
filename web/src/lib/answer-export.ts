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
    const url = isWebCitation(first) ? safeWebUrl(first.web.url) : null;
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
export function docxMarkdown(md: string): string {
  // 3 個以上のバックティック＋`ui`（種類の付いた ```ui bar・```UI 棒 も含む）、本文、同数の閉じ。
  const fence = /^(`{3,})[ \t]*ui\b[^\n]*\n[\s\S]*?\n\1[ \t]*$/gim;
  return md.replace(fence, "> 図はこのファイルに含まれません。チャットの回答で確認してください。");
}

const MAX_NAME = 60;

/// 書き出すファイルの名前（拡張子なし）。回答の最初の見出し → 会話の題名 → 「回答_日付」の順。
export function answerFileName(text: string, threadTitle: string | null, now = new Date()): string {
  const heading = firstHeading(text);
  const base = heading ?? threadTitle?.trim() ?? "";
  const clean = sanitizeFileName(base);
  if (clean) return clean;
  const y = now.getFullYear();
  const m = String(now.getMonth() + 1).padStart(2, "0");
  const d = String(now.getDate()).padStart(2, "0");
  return `回答_${y}-${m}-${d}`;
}

/// コードブロックの外にある最初の見出し（`#`〜`###`）の文字。強調などの記法は落とす。
function firstHeading(text: string): string | null {
  let fence: string | null = null;
  for (const line of text.split("\n")) {
    const open = /^\s*(`{3,}|~{3,})/.exec(line);
    if (open) {
      if (!fence) fence = open[1][0];
      else if (open[1][0] === fence) fence = null;
      continue;
    }
    if (fence) continue;
    const h = /^\s{0,3}#{1,3}\s+(.+?)\s*#*\s*$/.exec(line);
    if (h) return h[1].replace(/[*_`~]|\[(\d+)\]/g, "").replace(/\[([^\]]*)\]\([^)]*\)/g, "$1").trim();
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
