/// チャットの回答・会話の書き出し（#1048・#1071・会話全体）。ノート・Word（ドライブ）・md・docx・pdf の
/// 素になる Markdown とファイル名をここで組み立てる。
///
/// - 本文の `[n]` はそのまま残し、回答ごとに「出典」の一覧を付ける（書き出した先では引用チップに
///   ならないので、番号と出所の対応を文字で残す）。閲覧できない出典（withheld）は一覧に出さない。
///   番号は回答ごとに 1 から振られるので、会話全体でも一覧は回答の直後に置く（番号を取り違えない）。
/// - ```ui の図はノート・md ではそのまま残す（ノートは図として描ける）。Word は図を描けないので
///   プレースホルダへ置き換える（生の図の定義を Word に載せない）。
/// - 画像は md ではそのまま、ノート・Word ではリンクにする（ノートは画像を取り込まず、Word の変換側に
///   外部の画像を取りに行かせない）。
///
/// - Web ページの訳文（translation ブロック）は、回答の本文の後・出典の前に置く。外部ページ由来なので、
///   書き出した先でも会話の `page` モードと同じ扱いにする（[`translationMarkdown`]）。
///
/// 解析はチャットの表示と同じ（remark・GFM・日本語の強調・数式）。数式の解析器は表示と同じく押した
/// ときだけ読み込むので、ここの関数は非同期にしている。

import type { Code, Heading, Image, Link, Nodes, Root } from "mdast";
import { toString } from "mdast-util-to-string";
import remarkCjkFriendly from "remark-cjk-friendly";
import remarkGfm from "remark-gfm";
import remarkParse from "remark-parse";
import { unified, type Processor } from "unified";

import type { Citation } from "@/lib/chat-api";
import { citationLocator, groupCitations, isWebCitation, safeWebUrl } from "@/lib/citation";
import { langLabel, translationTitle, type Translation } from "@/lib/translation";

/// 出所の表示名を返す関数（チャットの出典一覧と同じもの・`MessageCitations.nameOf`）。
export type SourceName = (c: Pick<Citation, "node_id" | "web">) => string;

/// 回答の本文に、訳文と「出典」の一覧を付けた Markdown。どちらも無ければ本文だけ。
export async function answerMarkdown(
  text: string,
  citations: readonly Citation[],
  nameOf: SourceName,
  translations: readonly Translation[] = [],
): Promise<string> {
  const sources = sourcesMarkdown(text, citations, nameOf);
  if (!sources && translations.length === 0) return `${text.trimEnd()}\n`;
  // 本文の終わりのフェンスが閉じていない（生成の停止など）と、後ろに足す訳文・出典まで図・コードに入る。
  const parts = text.trim() ? [await closeTrailingFence(text.trimEnd())] : [];
  for (const t of translations) parts.push(await translationMarkdown(t, 2));
  if (sources) parts.push(sources);
  return `${parts.join("\n\n")}\n`;
}

/// 訳文の節（題名の見出し・出典と機械翻訳の注記・訳文）。`depth` は題名の見出しの深さで、訳文の中の
/// 見出しはその下へ下げる（ページの見出しで、書き出しの「質問」「回答」「出典」の節を装わせない）。
///
/// 訳文は外部ページ由来なので、会話の `page` モードと同じ扱いにして書き出す:
/// - 図・表・ボタンになるフェンス（```ui・```yui・```data_grid）は言語名を外して素のコードにする
///   （ノートは ```ui を図として描くので、ページの内容がノートの図・操作にならないように）。
/// - リンクには行き先のホストを添える（リンクの文字で行き先を装わせない）。
/// - 閉じていないフェンス・HTML は閉じる（後ろの訳文・出典を吸い込ませない）。
export async function translationMarkdown(t: Translation, depth: number): Promise<string> {
  const title = translationTitle(t);
  const partLabel = t.parts > 1 ? `（${t.part}/${t.parts}）` : "";
  const head: string[] = [];
  if (title) head.push(`${"#".repeat(depth)} ${escapeInline(title)}${partLabel}`);
  const url = linkDestination(safeWebUrl(t.sourceUrl));
  const source = url ? `[${escapeInline(t.host)}](${url})` : escapeInline(t.host);
  const notes = [
    t.failedChunks > 0 ? "一部は翻訳できなかったため、原文のまま載せています。" : null,
    t.sourceTruncated && t.part >= t.parts ? "ページが非常に長いため、取得できた範囲までの訳です。" : null,
  ].filter(Boolean);
  head.push(`出典: ${source} の機械翻訳（${escapeInline(langLabel(t.targetLang))}）${title ? "" : partLabel}${notes.length ? `。${notes.join("")}` : ""}`);
  const body = await closeTrailingFence((await pageMarkdown(t.markdown, depth)).trim());
  return body ? `${head.join("\n\n")}\n\n${body}` : head.join("\n\n");
}

/// 外部ページ由来の Markdown を書き出し用に直す（[`translationMarkdown`] の規則）。
/// 差し替えは重ならないようにする（見出しは記号の部分だけ・リンクはリンクの範囲だけを差し替える）。
async function pageMarkdown(md: string, depth: number): Promise<string> {
  const edits: Edit[] = [];
  const tree = await parse(md);
  // ページで一番上の見出しを、題名のすぐ下の段にそろえる（ページの多くは h2 から始まる。そのまま下げると
  // 段が深くなりすぎ、H3 までのノートでは見出しでなくなる）。
  let top = 6;
  walk(tree, (node) => {
    if (node.type === "heading") top = Math.min(top, node.depth);
  });
  walk(tree, (node) => {
    if (node.type === "heading") edits.push(...demotedHeading(md, node, depth + 1 - top));
    else if (node.type === "code" && isSpecialFence(node)) {
      const edit = plainFence(md, node);
      if (edit) edits.push(edit);
    } else if (node.type === "link") {
      const edit = linkWithHost(md, node);
      if (edit) edits.push(edit);
    }
  });
  return applyEdits(md, edits);
}

/// 見出しを `shift` 段下げる（6 段で止める）。中身の前の記号（`## `）を差し替え、後ろの記号
/// （閉じの `##`・setext の下線）を落とす。中身は元の文字のまま（記法を保つ）。
function demotedHeading(md: string, node: Heading, shift: number): Edit[] {
  const start = node.position?.start.offset;
  const end = node.position?.end.offset;
  if (start === undefined || end === undefined) return [];
  const marks = "#".repeat(Math.min(6, Math.max(1, node.depth + shift)));
  const first = node.children[0]?.position?.start.offset;
  const last = node.children[node.children.length - 1]?.position?.end.offset;
  if (first === undefined || last === undefined) return [{ start, end, text: marks }];
  // 中身の前が空（setext 見出し）なら差し替える範囲が無いので、中身の先頭の 1 単位ごと書き直す。
  const prefix: Edit =
    first > start
      ? { start, end: first, text: `${marks} ` }
      : { start, end: start + 1, text: `${marks} ${md.slice(start, start + 1)}` };
  return [prefix, { start: last, end, text: "" }];
}

/// 図・表・ボタンとして描かれるフェンスの種類（会話の表示・ノートが特別に扱うもの）。
function isSpecialFence(node: Code): boolean {
  return isFigure(node) || /^data_grid(?:$|\s)/i.test((node.lang ?? "").trim());
}

/// フェンスの開きの行から言語名（と続く情報）を外す。開きの行の記号はそのまま残す。
function plainFence(md: string, node: Code): Edit | null {
  const start = node.position?.start.offset;
  if (start === undefined) return null;
  const lineEnd = md.indexOf("\n", start);
  const openLine = md.slice(start, lineEnd < 0 ? md.length : lineEnd);
  const fence = /^(`{3,}|~{3,})/.exec(openLine);
  if (!fence) return null;
  return { start, end: start + openLine.length, text: fence[1] };
}

/// リンクの後ろに行き先のホストを添える（文字が行き先そのものなら添えない）。http(s) 以外・相対・
/// 断片だけのリンク（`#節` など）は書き出した先で辿れないので、リンクを外して文字だけ残す。
function linkWithHost(md: string, node: Link): Edit | null {
  const start = node.position?.start.offset;
  const end = node.position?.end.offset;
  if (start === undefined || end === undefined) return null;
  const label = toString(node).trim();
  let host: string | null = null;
  try {
    const u = new URL(node.url);
    if (u.protocol === "http:" || u.protocol === "https:") host = u.host;
  } catch {
    host = null;
  }
  if (!host) return { start, end, text: escapeInline(label) };
  if (!label || label === node.url || label === host) return null;
  return { start, end, text: `${md.slice(start, end)}（${escapeInline(host)}）` };
}
/// 会話のやり取り 1 組（質問と、それへの回答）。質問の無い回答（会話の先頭など）もある。
export type ThreadTurn = {
  question: string | null;
  answer: string;
  citations: readonly Citation[];
  /// 回答に付いた Web ページの訳文（回答の本文の後・出典の前に置く）。
  translations?: readonly Translation[];
};

/// 会話全体の Markdown。題名と日付、やり取りを順に（質問 → 回答 → その回答の出典）並べる。
/// `includeQuestions` が偽なら回答だけを並べる。思考・ツールの実行・添付は含めない。
export async function threadMarkdown(
  turns: readonly ThreadTurn[],
  opts: { title: string | null; includeQuestions: boolean; nameOf: SourceName; now?: Date },
): Promise<string> {
  const parts: string[] = [`# ${escapeInline(opts.title?.trim() || "会話")}`, `${dateText(opts.now)} ・ Shiki との会話`];
  for (const t of turns) {
    if (opts.includeQuestions && t.question?.trim()) {
      // 質問は利用者が書いた文。打ったとおりの文字として載せ、見出し・リンク・HTML などの構造にしない
      // （「## 回答」を書いて回答や出典を装う、閉じない HTML で後ろのやり取りを吸い込む、を防ぐ）。
      // 先頭の空行と末尾の空白だけ落とす（1 行目の字下げは打ったとおりに残す）。
      parts.push("## 質問", literalQuote(t.question.replace(/^(?:[ \t]*\n)+/, "").trimEnd()));
    }
    const translations = t.translations ?? [];
    if (!t.answer.trim() && translations.length === 0) continue;
    parts.push("## 回答");
    // 先頭の空行と末尾の空白だけ落とす（字下げのコードブロックで始まる回答の 1 行目を崩さない）。
    if (t.answer.trim()) parts.push(await closeTrailingFence(t.answer.replace(/^(?:[ \t]*\n)+/, "").trimEnd()));
    for (const tr of translations) parts.push(await translationMarkdown(tr, 3));
    const sources = sourcesMarkdown(t.answer, t.citations, opts.nameOf, "###");
    if (sources) parts.push(sources);
  }
  return `${parts.join("\n\n")}\n`;
}

/// 本文の後ろに足す文字がコードブロック・数式・HTML のブロックに入ってしまう（閉じていない）なら、
/// 閉じる。閉じているかは解析に決めさせる（試しの段落を足して、吸われるかを見る）。直した後も吸われる
/// こと（閉じない HTML が続けて複数ある `<!--\n\n<!--` など）があるので、吸われなくなるまで繰り返す。
/// 引用・リストの中なら、空行で入れ物ごと終わるので閉じは要らない。
async function closeTrailingFence(input: string): Promise<string> {
  let body = input;
  for (let pass = 0; pass < MAX_CLOSE_PASSES; pass++) {
    const block = await swallowingBlock(body);
    if (!block) return body;
    const { start, html } = block;
    if (html) {
      // 閉じない HTML のブロック（`<?php`・`<!--`・`<script` など）は空行では終わらない。チャットは生の
      // HTML を文字のまま見せるので、書き出しでも先頭の `<` を文字にして段落へ戻す。
      body = `${body.slice(0, start)}\\${body.slice(start)}`;
      continue;
    }
    const open = /^(`{3,}|~{3,}|\${2,})/.exec(body.slice(start));
    if (!open) return body; // 字下げのコードブロックは空行と字下げの無い段落で終わる（吸われない）
    const lineStart = body.lastIndexOf("\n", start - 1) + 1;
    // 開きの行の入れ物（`>`・リストの記号）の分だけ、閉じの行にも付ける。
    const prefix = body.slice(lineStart, start).replace(/[^>\s]/g, " ");
    body = `${body}\n${prefix}${open[1]}`;
  }
  // 回数を使い切っても吸われる（閉じない HTML が大量に続くなど）ときは、行頭の `<` をすべて文字にする
  // （HTML のブロックは行頭の `<` でしか始まらない）。
  return (await swallowingBlock(body)) ? body.replace(/^([ \t>]*)</gm, "$1\\<") : body;
}

/// 本文の後ろに足した試しの段落を吸い込むブロック（コード・数式・HTML）の開始位置。無ければ null。
async function swallowingBlock(body: string): Promise<{ start: number; html: boolean } | null> {
  let found: { start: number; html: boolean } | null = null;
  walk(await parse(`${body}\n\n${PROBE}`), (node) => {
    const swallows = (node.position?.end.offset ?? 0) > body.length;
    const start = node.position?.start.offset;
    if (swallows && start !== undefined && (node.type === "code" || isMathBlock(node) || node.type === "html")) {
      found = { start, html: node.type === "html" };
    }
  });
  return found;
}

/// 閉じ直しを繰り返す上限（1 回ごとに本文を解析し直すため、敵対的な入力で回り続けないようにする）。
const MAX_CLOSE_PASSES = 8;

const PROBE = "閉じの確認";

/// 「出典」の節（見出しと箇条書き）。本文で使った出所だけを、本文で最初に引いた順に並べる。
export function sourcesMarkdown(
  text: string,
  citations: readonly Citation[],
  nameOf: SourceName,
  heading = "##",
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
  return `${heading} 出典\n\n${lines.join("\n")}`;
}

/// 社内文書（閲覧できないものを含む）を出典に持つか。ドライブに置くときの共有の注意に使う
/// （Web ページは誰でも読めるので注意しない。withheld は一覧に出ないが本文はその文書を元に書かれている）。
export function hasDocumentSources(citations: readonly Citation[]): boolean {
  return citations.some((c) => c.withheld || !isWebCitation(c));
}

/// Word 用の Markdown。```ui の図は Word に描けないため、プレースホルダに置き換える（引用・リストの
/// 中の図も、その入れ物の中に置く）。画像はリンクにする（変換側に外部の画像を取りに行かせない）。
export async function docxMarkdown(md: string): Promise<string> {
  const edits: Edit[] = [];
  walk(await parse(md), (node) => {
    if (node.type === "code" && isFigure(node)) edits.push(editOf(node, FIGURE_PLACEHOLDER));
    if (node.type === "image") edits.push(editOf(node, imageLink(node)));
  });
  return applyEdits(md, edits);
}

/// ノート用の Markdown。ノートの取り込みは画像を代替テキストにしてしまう（URL が消える）ので、
/// 画像はリンクにして元の画像へたどれるようにする。図（```ui）はノートで描けるのでそのまま。
export async function noteMarkdown(md: string): Promise<string> {
  const edits: Edit[] = [];
  walk(await parse(md), (node) => {
    if (node.type === "image") edits.push(editOf(node, imageLink(node)));
  });
  return applyEdits(md, edits);
}

const FIGURE_PLACEHOLDER = "（図はこのファイルに含まれません。チャットの回答で確認してください。）";

/// チャットの表示と同じ解析（remark・GFM・日本語の強調・数式）。図・画像・見出しの判定を表示とずらさない。
/// 数式の解析器は表示（markdown-math）と同じく、初めて書き出すときに読み込む（会話画面の初回の読み込みに
/// 載せない）。数式の中の `![…](…)` を画像として書き換えないためにも、表示と同じ設定で解析する。
let parserPromise: Promise<Processor<Root>> | null = null;

function loadParser(): Promise<Processor<Root>> {
  parserPromise ??= import("remark-math-extended").then(
    (math) =>
      unified()
        .use(remarkParse)
        .use(remarkGfm)
        .use(remarkCjkFriendly)
        .use(math.default, { singleDollarTextMath: false })
        .freeze() as unknown as Processor<Root>,
    (e: unknown) => {
      // 読み込みに失敗したら（配信の入れ替え・通信の切れ目など）覚えずに捨て、次に押したときに読み直す
      // （表示側の markdown-math と同じ。失敗を覚えると、再読み込みまで書き出しが全部できなくなる）。
      parserPromise = null;
      throw e;
    },
  );
  return parserPromise;
}

async function parse(md: string): Promise<Root> {
  return (await loadParser()).parse(md);
}

function walk(node: Nodes, visit: (node: Nodes) => void): void {
  visit(node);
  if ("children" in node) for (const child of node.children) walk(child, visit);
}

/// 数式のブロック（`$$ … $$`）か。型は数式の拡張が足すので、種類の名前で見る。
function isMathBlock(node: Nodes): boolean {
  return (node as { type: string }).type === "math";
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
export async function answerFileName(text: string, threadTitle: string | null, now = new Date()): Promise<string> {
  // 見出しが記法だけ（`## [1]` など）で空になったら、会話の題名へ進む。
  const clean = sanitizeFileName((await firstHeading(text)) ?? "") || sanitizeFileName(threadTitle ?? "");
  return clean || `回答_${isoDate(now)}`;
}

/// 会話全体を書き出すファイルの名前（拡張子なし）。会話の題名 → 「会話_日付」の順。
export function threadFileName(threadTitle: string | null, now = new Date()): string {
  return sanitizeFileName(threadTitle ?? "") || `会話_${isoDate(now)}`;
}

function isoDate(now: Date): string {
  const y = now.getFullYear();
  const m = String(now.getMonth() + 1).padStart(2, "0");
  const d = String(now.getDate()).padStart(2, "0");
  return `${y}-${m}-${d}`;
}

function dateText(now = new Date()): string {
  return isoDate(now).replace(/-/g, "/");
}

/// 最上位の最初の見出し（`#`〜`###`）の文字。強調・コード・リンクの記法は解析で落ち、引用番号
/// （`[1]`）だけを消す（`file_name`・`10~20件` のように名前に含まれる記号は残る）。
async function firstHeading(text: string): Promise<string | null> {
  const h = (await parse(text)).children.find((n): n is Heading => n.type === "heading" && n.depth <= 3);
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

/// 利用者が書いた文を、打ったとおりの文字の引用（`> `）にする。ASCII の記号はすべて `\\` で文字にし
/// （CommonMark はどの ASCII 記号も `\\` で文字にできる）、行の先頭の空白は字下げのコードにならない
/// よう詰めずに残す。行の区切りは改行のまま見せる（行末の `\\` は改行）。
export function literalQuote(text: string): string {
  const lines = text.replace(/\r\n?/g, "\n").split("\n");
  return lines
    .map((line, i) => {
      if (!line.trim()) return ">";
      const lead = /^[ \t]*/.exec(line)?.[0] ?? "";
      const body = line.slice(lead.length).replace(/[!-/:-@[-`{-~]/g, "\\$&");
      const hardBreak = i < lines.length - 1 && lines[i + 1].trim() ? "\\" : "";
      return `> ${"\u00a0".repeat(lead.replace(/\t/g, "    ").length)}${body}${hardBreak}`;
    })
    .join("\n");
}

/// 一覧の 1 行に入れる文字の Markdown 記法を無効化する（ファイル名の `*`・`[` などで崩さない）。
/// 単独の `~` は GFM で打ち消し線に、`&` は文字参照（`&copy;` など）になるので、これも無効化する。
function escapeInline(s: string): string {
  return s.replace(/([\\`*_[\]<>#|~&])/g, "\\$1").replace(/\n+/g, " ");
}
