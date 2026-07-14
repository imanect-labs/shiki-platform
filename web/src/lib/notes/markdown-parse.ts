/// 正規化 Markdown → ProseMirror ノード JSON（Task 11P.3 / issue #297 B）。
///
/// 外部からエディタへ「生 Markdown を素テキストで貼り付けた」ときにブロック記法
/// （見出し/リスト/引用/コード/表）を構造へ変換する。**正準は Rust の
/// `crates/collab/src/note/md_parse.rs`**（pulldown-cmark）であり、本モジュールは
/// その意味論にクライアント側で揃える。ブロック構造の字句解析は実績のある `marked`
/// を使い、生成する ProseMirror ノードだけを自前で組む。
///
/// セキュリティ契約（FR-8 / Task 11P.6・issue #297 注意点）:
/// - **生 HTML は実行可能な形にしない**: ブロック HTML は `html` コードブロックへ縮退、
///   インライン HTML はリテラルテキストにする（Rust `md_parse.rs` と同じ縮退）。
/// - **```shiki-embed フェンスを埋め込みノードへ昇格しない**: 任意ペースト由来の埋め込み
///   自動生成は confused-deputy になる。通常のコードブロック（language=shiki-embed）に
///   落として無害化する（埋め込み挿入は既存のスラッシュ経路のみ）。

import { marked } from "marked";

/// ProseMirror ノード JSON（`Node.fromJSON` が受ける最小形）。
export interface PmJson {
  type: string;
  attrs?: Record<string, unknown>;
  content?: PmJson[];
  text?: string;
  marks?: { type: string; attrs?: Record<string, unknown> }[];
}

/// marked トークンの許容形（union を素直に読むための緩い形）。
interface Tok {
  type: string;
  depth?: number;
  text?: string;
  lang?: string;
  ordered?: boolean;
  start?: number | "";
  task?: boolean;
  checked?: boolean;
  tokens?: Tok[];
  items?: Tok[];
  href?: string;
  header?: TableCellTok[];
  rows?: TableCellTok[][];
}
interface TableCellTok {
  text?: string;
  tokens?: Tok[];
}

/// ブロック記法を含む md か（含まなければ既定のインライン貼り付けに委ねる）。
/// 見出し/リスト/引用/コード/水平線/表/HTML のいずれかがあれば true。
const BLOCK_TOKEN_TYPES = new Set([
  "heading",
  "list",
  "blockquote",
  "code",
  "hr",
  "table",
  "html",
]);

export function looksLikeBlockMarkdown(md: string): boolean {
  if (!md.trim()) return false;
  let tokens: Tok[];
  try {
    tokens = marked.lexer(md) as unknown as Tok[];
  } catch {
    return false;
  }
  return tokens.some((t) => BLOCK_TOKEN_TYPES.has(t.type));
}

/// md をブロック ProseMirror ノード列へ変換する（失敗時は単一段落へ fail-safe）。
export function parseMarkdownToNodes(md: string): PmJson[] {
  let tokens: Tok[];
  try {
    tokens = marked.lexer(md) as unknown as Tok[];
  } catch {
    return [paragraph([text(md)])];
  }
  const nodes = blockTokensToNodes(tokens);
  return nodes.length > 0 ? nodes : [paragraph([])];
}

// ---------------------------------------------------------------------------
// ブロック
// ---------------------------------------------------------------------------

function blockTokensToNodes(tokens: Tok[]): PmJson[] {
  const out: PmJson[] = [];
  for (const tok of tokens) {
    const node = blockTokenToNode(tok);
    if (Array.isArray(node)) out.push(...node);
    else if (node) out.push(node);
  }
  return out;
}

function blockTokenToNode(tok: Tok): PmJson | PmJson[] | null {
  switch (tok.type) {
    case "space":
    case "def":
      return null;
    case "heading":
      return {
        type: "heading",
        attrs: { level: clampLevel(tok.depth) },
        content: inlineTokensToNodes(tok.tokens ?? [], []),
      };
    case "paragraph":
    case "text":
      return paragraph(inlineTokensToNodes(tok.tokens ?? [], [], tok.text));
    case "blockquote":
      return { type: "blockquote", content: ensureBlocks(blockTokensToNodes(tok.tokens ?? [])) };
    case "code":
      return codeBlockNode(tok);
    case "hr":
      return { type: "horizontalRule" };
    case "list":
      return splitList(tok);
    case "table":
      return tableNode(tok);
    case "html":
      // 生 HTML ブロックは実行不能な html コードブロックへ縮退（stored XSS 遮断）。
      return codeBlock("html", stripTrailingNewline(tok.text ?? ""));
    default:
      return tok.text ? paragraph([text(tok.text)]) : null;
  }
}

/// コードフェンス。```shiki-embed は昇格せず通常コードブロックに落とす（confused-deputy 回避）。
function codeBlockNode(tok: Tok): PmJson {
  const lang = firstWord(tok.lang ?? "");
  return codeBlock(lang, tok.text ?? "");
}

function codeBlock(language: string, code: string): PmJson {
  return {
    type: "codeBlock",
    attrs: { language },
    // テキストノードは空文字を持てない（空コードブロックは content 無し）。
    content: code.length > 0 ? [text(code)] : [],
  };
}

/// marked の list トークンを task/非 task の連続で分割し、TipTap のノードへ写す。
/// TipTap の taskList は taskItem のみ許すため Rust `finish_list` と同じ方針で分割する。
function splitList(tok: Tok): PmJson[] {
  const items = tok.items ?? [];
  const ordered = tok.ordered === true;
  const start = typeof tok.start === "number" ? tok.start : 1;
  const out: PmJson[] = [];
  let plainRun: PmJson[] = [];
  let taskRun: PmJson[] = [];
  let seq = start;

  const flushPlain = () => {
    if (plainRun.length === 0) return;
    if (ordered) {
      out.push({ type: "orderedList", attrs: { start: seq }, content: plainRun });
      seq += plainRun.length;
    } else {
      out.push({ type: "bulletList", content: plainRun });
    }
    plainRun = [];
  };
  const flushTask = () => {
    if (taskRun.length === 0) return;
    out.push({ type: "taskList", content: taskRun });
    taskRun = [];
  };

  for (const item of items) {
    const body = ensureBlocks(blockTokensToNodes(item.tokens ?? []));
    if (item.task === true) {
      flushPlain();
      taskRun.push({ type: "taskItem", attrs: { checked: item.checked === true }, content: body });
    } else {
      flushTask();
      plainRun.push({ type: "listItem", content: body });
    }
  }
  flushPlain();
  flushTask();
  return out;
}

/// GFM テーブルを table>tableRow>(tableHeader|tableCell)>paragraph へ写す。
function tableNode(tok: Tok): PmJson {
  const rows: PmJson[] = [];
  const header = (tok.header ?? []).map((cell) => tableCell("tableHeader", cell));
  rows.push({ type: "tableRow", content: header });
  for (const row of tok.rows ?? []) {
    rows.push({ type: "tableRow", content: row.map((cell) => tableCell("tableCell", cell)) });
  }
  return { type: "table", content: rows };
}

function tableCell(type: "tableHeader" | "tableCell", cell: TableCellTok): PmJson {
  return { type, content: [paragraph(inlineTokensToNodes(cell.tokens ?? [], [], cell.text))] };
}

/// listItem/blockquote 等が空にならないよう最低 1 段落を保証する。
function ensureBlocks(nodes: PmJson[]): PmJson[] {
  return nodes.length > 0 ? nodes : [paragraph([])];
}

// ---------------------------------------------------------------------------
// インライン
// ---------------------------------------------------------------------------

type MarkJson = { type: string; attrs?: Record<string, unknown> };

/// インライントークン列をテキスト/hardBreak ノードへ写す（marks は文脈から継承）。
/// tokens が空で raw text だけある場合は fallbackText を 1 テキストノードにする。
function inlineTokensToNodes(tokens: Tok[], marks: MarkJson[], fallbackText?: string): PmJson[] {
  if (tokens.length === 0 && fallbackText) {
    return [text(decodeEntities(fallbackText), marks)];
  }
  const out: PmJson[] = [];
  for (const tok of tokens) {
    pushInline(out, tok, marks);
  }
  return out;
}

function pushInline(out: PmJson[], tok: Tok, marks: MarkJson[]): void {
  switch (tok.type) {
    case "text":
      // ネストしたインライン（gfm）があれば辿り、無ければ生テキスト。
      if (tok.tokens && tok.tokens.length > 0) {
        for (const c of tok.tokens) pushInline(out, c, marks);
      } else {
        pushText(out, decodeEntities(tok.text ?? ""), marks);
      }
      break;
    case "escape":
      pushText(out, tok.text ?? "", marks);
      break;
    case "strong":
      recurseInline(out, tok, marks, { type: "bold" });
      break;
    case "em":
      recurseInline(out, tok, marks, { type: "italic" });
      break;
    case "del":
      recurseInline(out, tok, marks, { type: "strike" });
      break;
    case "codespan":
      // コードは排他マーク（他マークは付けない・md の意味論）。
      pushText(out, decodeEntities(tok.text ?? ""), [{ type: "code" }]);
      break;
    case "link":
      recurseInline(out, tok, marks, { type: "link", attrs: { href: tok.href ?? "" } });
      break;
    case "br":
      out.push({ type: "hardBreak" });
      break;
    case "image":
      // 画像はドライブ埋め込みへ寄せる設計。ここでは alt をテキストとして残す。
      pushText(out, decodeEntities(tok.text ?? ""), marks);
      break;
    case "html":
      // インライン HTML はリテラル文字列（実行させない）。
      pushText(out, tok.text ?? "", marks);
      break;
    default:
      if (tok.text) pushText(out, decodeEntities(tok.text), marks);
  }
}

function recurseInline(out: PmJson[], tok: Tok, marks: MarkJson[], add: MarkJson): void {
  const next = [...marks, add];
  if (tok.tokens && tok.tokens.length > 0) {
    for (const c of tok.tokens) pushInline(out, c, next);
  } else {
    pushText(out, decodeEntities(tok.text ?? ""), next);
  }
}

function pushText(out: PmJson[], value: string, marks: MarkJson[]): void {
  if (value.length === 0) return;
  out.push(text(value, marks));
}

// ---------------------------------------------------------------------------
// 小物
// ---------------------------------------------------------------------------

function paragraph(content: PmJson[]): PmJson {
  return { type: "paragraph", content };
}

function text(value: string, marks: MarkJson[] = []): PmJson {
  return marks.length > 0 ? { type: "text", text: value, marks } : { type: "text", text: value };
}

/// marked が出力する HTML エスケープ（& < > " '）を復元する（テキストノードは生文字を持つ）。
function decodeEntities(s: string): string {
  return s
    .replace(/&amp;/g, "&")
    .replace(/&lt;/g, "<")
    .replace(/&gt;/g, ">")
    .replace(/&quot;/g, '"')
    .replace(/&#39;/g, "'");
}

function firstWord(s: string): string {
  return s.trim().split(/\s+/)[0] ?? "";
}

function stripTrailingNewline(s: string): string {
  return s.replace(/\n+$/, "");
}

function clampLevel(depth: unknown): number {
  const n = typeof depth === "number" ? depth : 1;
  return Math.min(6, Math.max(1, n));
}
