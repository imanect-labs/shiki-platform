/// チャットの回答の書き出し（#1048）。ノート・Word（ドライブ）・md・docx・pdf の素になる
/// Markdown とファイル名をここで組み立てる。
///
/// - 本文の `[n]` はそのまま残し、末尾に「出典」の一覧を付ける（書き出した先では引用チップに
///   ならないので、番号と出所の対応を文字で残す）。閲覧できない出典（withheld）は一覧に出さない。
/// - ```ui の図はノート・md ではそのまま残す（ノートは図として描ける）。Word は図を描けないので
///   プレースホルダへ置き換える（生の図の定義を Word に載せない）。
/// - 画像は md ではそのまま、ノート・Word ではリンクにする（ノートは画像を取り込まず、Word の変換側に
///   外部の画像を取りに行かせない）。

import type { Code, Heading, Image, Nodes, Root } from "mdast";
import { toString } from "mdast-util-to-string";
import remarkCjkFriendly from "remark-cjk-friendly";
import remarkGfm from "remark-gfm";
import remarkParse from "remark-parse";
import { unified } from "unified";

import type { Citation } from "@/lib/chat-api";
import { citationLocator, groupCitations, isWebCitation, safeWebUrl } from "@/lib/citation";

/// 出所の表示名を返す関数（チャットの出典一覧と同じもの・`MessageCitations.nameOf`）。
export type SourceName = (c: Pick<Citation, "node_id" | "web">) => string;

/// 回答の本文に「出典」の一覧を付けた Markdown。出典が無ければ本文だけ。
export function answerMarkdown(text: string, citations: readonly Citation[], nameOf: SourceName): string {
  const sources = sourcesMarkdown(text, citations, nameOf);
  if (!sources) return `${text.trimEnd()}\n`;
  // 本文の終わりのフェンスが閉じていない（生成の停止など）と、後ろに足す出典まで図・コードに入る。
  return `${closeTrailingFence(text.trimEnd())}\n\n${sources}\n`;
}

/// 本文の最後のコードブロックが閉じていなければ、開きと同じフェンスで閉じる（入れ物の記号も付ける）。
function closeTrailingFence(body: string): string {
  let last: Code | null = null;
  walk(parse(body), (node) => {
    if (node.type === "code" && node.position?.end.offset === body.length) last = node;
  });
  const code = last as Code | null;
  const start = code?.position?.start.offset;
  if (!code || start === undefined) return body;
  const lineStart = body.lastIndexOf("\n", start - 1) + 1;
  const prefix = body.slice(lineStart, start);
  const open = /^(`{3,}|~{3,})/.exec(body.slice(start));
  if (!open) return body; // 字下げのコードブロック（閉じが無い）
  const lines = body.slice(start).split("\n");
  const tail = lines[lines.length - 1].replace(/^[ \t>]*/, "");
  const closed =
    lines.length > 1 && new RegExp(`^${open[1][0] === "`" ? "`" : "~"}{${open[1].length},}[ \\t]*$`).test(tail);
  if (closed) return body;
  // 引用の中なら `>` を、リストの中なら記号の分の字下げを付けて閉じる。
  return `${body}\n${prefix.replace(/[^>\s]/g, " ")}${open[1]}`;
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
  const edits: Edit[] = [];
  walk(parse(md), (node) => {
    if (node.type === "code" && isFigure(node)) edits.push(editOf(node, FIGURE_PLACEHOLDER));
    if (node.type === "image") edits.push(editOf(node, imageLink(node)));
  });
  return applyEdits(md, edits);
}

/// ノート用の Markdown。ノートの取り込みは画像を代替テキストにしてしまう（URL が消える）ので、
/// 画像はリンクにして元の画像へたどれるようにする。図（```ui）はノートで描けるのでそのまま。
export function noteMarkdown(md: string): string {
  const edits: Edit[] = [];
  walk(parse(md), (node) => {
    if (node.type === "image") edits.push(editOf(node, imageLink(node)));
  });
  return applyEdits(md, edits);
}

const FIGURE_PLACEHOLDER = "（図はこのファイルに含まれません。チャットの回答で確認してください。）";

/// チャットの表示と同じ解析（remark・GFM・日本語の強調）。図・画像・見出しの判定を表示とずらさない。
/// 手で行を読むと、引用・リスト・字下げのコードブロックの規則（CommonMark）を取り違える。
const parser = unified().use(remarkParse).use(remarkGfm).use(remarkCjkFriendly).freeze();

function parse(md: string): Root {
  return parser.parse(md);
}

function walk(node: Nodes, visit: (node: Nodes) => void): void {
  visit(node);
  if ("children" in node) for (const child of node.children) walk(child, visit);
}

/// 本文の範囲（解析の位置）を別の文字に差し替える。
type Edit = { start: number; end: number; text: string };

function editOf(node: Nodes, text: string): Edit {
  return { start: node.position?.start.offset ?? 0, end: node.position?.end.offset ?? 0, text };
}

/// 後ろの差し替えから当てる（前の位置がずれない）。位置の無い・重なる差し替えは当てない。
function applyEdits(md: string, edits: Edit[]): string {
  let out = md;
  let limit = Number.POSITIVE_INFINITY;
  for (const e of [...edits].sort((x, y) => y.start - x.start)) {
    if (e.end <= e.start || e.end > limit) continue;
    out = out.slice(0, e.start) + e.text + out.slice(e.end);
    limit = e.start;
  }
  return out;
}

/// 図として描かれるフェンスか（ui / yui、`ui bar`・`yui:Bar` のような種類付きも）。
function isFigure(node: Code): boolean {
  return /^(?:ui|yui)(?:$|:)/i.test((node.lang ?? "").trim());
}

/// `![代替](URL "題")` を `[代替](URL)` にする（代替が空なら「画像」）。
function imageLink(node: Image): string {
  const alt = escapeInline(node.alt?.trim() || "画像");
  return `[${alt}](${linkDestination(node.url) ?? ""})`;
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

/// 最上位の最初の見出し（`#`〜`###`）の文字。強調・コード・リンクの記法は解析で落ち、引用番号
/// （`[1]`）だけを消す（`file_name`・`10~20件` のように名前に含まれる記号は残る）。
function firstHeading(text: string): string | null {
  const h = parse(text).children.find((n): n is Heading => n.type === "heading" && n.depth <= 3);
  return h ? toString(h).replace(/\[\d+\]/g, "").trim() : null;
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
