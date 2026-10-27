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

import { parseDriveRefHref } from "@/lib/skills/drive-ref";

import { normalizeYuiInfo } from "./yui-language";

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

export interface ParseOptions {
  /// `[…](shiki://drive/<kind>/<id>)` のリンクを `driveRef` ノードにする（スキルエディタ・#919）。
  /// スキーマに driveRef を持つエディタだけが有効にする（ノートでは素のテキストに落とす）。
  driveRefs?: boolean;
  /// 生 HTML ブロックを html コードブロックではなく本文の文字として残す（スキルエディタ・#919）。
  /// SKILL.md では `<rules>…</rules>` のようなタグを区切りとして書くのが普通で、コード例に
  /// 変えると AI が読む指示文の意味が変わる。タグだけの行はその文字のまま段落にし、間の行は
  /// Markdown として読む。どちらも文字として扱うだけで、HTML として描画はしない。
  htmlAsText?: boolean;
}

/// 変換中のオプション（変換は同期なので、呼び出しの間だけ立てる）。
let current: ParseOptions = {};

/// md をブロック ProseMirror ノード列へ変換する（失敗時は単一段落へ fail-safe）。
export function parseMarkdownToNodes(md: string, options: ParseOptions = {}): PmJson[] {
  let tokens: Tok[];
  try {
    tokens = marked.lexer(md) as unknown as Tok[];
  } catch {
    return [paragraph([text(md)])];
  }
  const prev = current;
  current = options;
  try {
    const nodes = blockTokensToNodes(tokens);
    return nodes.length > 0 ? nodes : [paragraph([])];
  } finally {
    current = prev;
  }
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
      if (current.htmlAsText) return htmlBlockAsText(tok.text ?? "");
      // 生 HTML ブロックは実行不能な html コードブロックへ縮退（stored XSS 遮断）。
      return codeBlock("html", stripTrailingNewline(tok.text ?? ""));
    default:
      return tok.text ? paragraph([text(tok.text)]) : null;
  }
}

/// タグだけの行（`<rules>`・`</rules>`・`<br/>` など）。
const TAG_LINE = /^\s*<\/?[A-Za-z][\w:-]*(\s[^<>]*)?\/?>\s*$/;

/// 生 HTML ブロックを文字として読む（`htmlAsText`）。タグだけの行はその文字の段落にし、
/// 間の行は Markdown として読む。中にまた現れた HTML は行ごとの文字の段落にする（再帰しない）。
function htmlBlockAsText(raw: string): PmJson[] {
  const out: PmJson[] = [];
  const literal = (s: string) => {
    for (const line of stripTrailingNewline(s).split("\n")) {
      if (line.trim()) out.push(paragraph([text(line)]));
    }
  };
  let buf: string[] = [];
  const flush = () => {
    const md = buf.join("\n");
    buf = [];
    if (!md.trim()) return;
    for (const t of marked.lexer(md) as unknown as Tok[]) {
      if (t.type === "html") {
        literal(t.text ?? "");
        continue;
      }
      const node = blockTokenToNode(t);
      if (Array.isArray(node)) out.push(...node);
      else if (node) out.push(node);
    }
  };
  for (const line of stripTrailingNewline(raw).split("\n")) {
    if (TAG_LINE.test(line)) {
      flush();
      out.push(paragraph([text(line.trim())]));
    } else {
      buf.push(line);
    }
  }
  flush();
  return out;
}

/// md 埋め込みフェンスの予約語（Rust `ast.rs` の `EMBED_FENCE_LANG` と対）。
const RESERVED_EMBED_LANG = "shiki-embed";

/// コードフェンス。```shiki-embed は昇格せず通常コードブロックに落とす（confused-deputy 回避）。
function codeBlockNode(tok: Tok): PmJson {
  let lang = firstWord(tok.lang ?? "");
  // 予約語 shiki-embed をそのまま残すと、保存 md を Rust `md_parse.rs` が再読込した
  // 際に `Block::Embed` へ昇格し、任意ペーストが実埋め込みになる（confused-deputy）。
  // language を無害化して昇格経路を断つ（ペイロードは inert なコードとして残す）。
  if (lang === RESERVED_EMBED_LANG) lang = "";
  // ```ui bar（Yui・#530）は情報文字列の 2 語目が図の種類なので、`ui:bar` に寄せて残す
  // （1 語に切り詰めると種類が落ちて図にできない。`ui:bar` は class 1 つで HTML 往復も保つ）。
  // 種類として分かるものだけ寄せ、小文字にする（`ui a```b` は寄せない＝md 往復を壊さない）。
  const yui = normalizeYuiInfo(tok.lang ?? "");
  if (yui) lang = yui;
  return codeBlock(lang, tok.text ?? "");
}

/// コードブロックのノード（language はそのまま。```ui は正規化済みの名前を受け取る）。
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
      // marked は `\<` `\>` `\&` の escape トークンを `&lt;` などの実体参照で返すので戻す
      // （戻さないと、エスケープして保存した `<` `>` が読み直しで `&lt;` の文字になる）。
      pushText(out, decodeEntities(tok.text ?? ""), marks);
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
    case "codespan": {
      // コードは bold/italic/strike と排他（md の意味論）だが、外側の link は保持する
      // （Rust `render_marked_text` と一致・`[`code`](url)` でリンクが消えないように）。
      const link = marks.find((m) => m.type === "link");
      pushText(out, decodeEntities(tok.text ?? ""), link ? [link, { type: "code" }] : [{ type: "code" }]);
      break;
    }
    case "link": {
      const ref = current.driveRefs ? parseDriveRefHref(tok.href ?? "") : null;
      if (ref) {
        // ラベルはエスケープを解いた文字（`tok.text` は `\_` などが残る生の文字列で、
        // そのまま持つと保存のたびにバックスラッシュが増える）。
        const label = tok.tokens?.length ? plainText(tok.tokens) : decodeEntities(tok.text ?? "");
        out.push({ type: "driveRef", attrs: { kind: ref.kind, id: ref.id, label } });
        break;
      }
      // 危険スキーム（javascript:/data: 等）は Link 拡張の scheme ガードを迂回して
      // 実行可能リンクになりうるため、ここで検証してリンク化しない（テキストは残す）。
      const safe = safeLinkHref(tok.href ?? "");
      if (safe === null) {
        if (tok.tokens && tok.tokens.length > 0) {
          for (const c of tok.tokens) pushInline(out, c, marks);
        } else {
          pushText(out, decodeEntities(tok.text ?? ""), marks);
        }
      } else {
        recurseInline(out, tok, marks, { type: "link", attrs: { href: safe } });
      }
      break;
    }
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

/// インライントークン列の素の文字（エスケープを解き、書式は落とす）。
function plainText(tokens: Tok[]): string {
  return tokens
    .map((t) => {
      if (t.type === "escape") return decodeEntities(t.text ?? "");
      if (t.tokens && t.tokens.length > 0) return plainText(t.tokens);
      return decodeEntities(t.text ?? "");
    })
    .join("");
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

/// リンク先を安全なスキームだけ許可する（note-editor の Link 拡張の protocols と一致）。
/// http/https/mailto と、スキームを持たない相対/アンカーのみ許可。危険スキームは null。
///
/// ブラウザは URL 評価前に TAB/改行などの制御文字を除去するため、`java\tscript:` の
/// ような難読化は素の正規表現をすり抜ける。判定前に制御文字を除去し、正規化後の
/// href を返す（受理経路にも制御文字を残さない）。
function safeLinkHref(href: string): string | null {
  // C0 制御文字（TAB/改行等）・空白・DEL を除去してから判定する（`java<TAB>script:` や
  // 先頭空白での難読化はブラウザの URL パーサに無視され実行されうるため封じる）。
  let cleaned = "";
  for (const ch of href) {
    const code = ch.codePointAt(0) ?? 0;
    if (code > 0x20 && code !== 0x7f) cleaned += ch;
  }
  const m = /^([a-zA-Z][a-zA-Z0-9+.-]*):/.exec(cleaned);
  if (!m) return cleaned; // スキーム無し（相対・#anchor）は許可。
  const scheme = m[1].toLowerCase();
  return scheme === "http" || scheme === "https" || scheme === "mailto" ? cleaned : null;
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
