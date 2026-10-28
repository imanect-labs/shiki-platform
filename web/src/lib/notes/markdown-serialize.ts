/// ProseMirror（ノートエディタ）ドキュメント → 正規化 Markdown（Task 11P.3 / issue #297）。
///
/// クリップボードの `text/plain` 出力（コピーを md にする・issue #297 A）と、
/// 他ノートへの往復に使う。**正準は Rust の `crates/collab/src/note/ast.rs`**
/// （`render_markdown`）であり、本モジュールはその描画規則にクライアント側で揃える:
/// - インライン: strike(~~)→italic(*)→bold(**) のネスト、code は排他、link は最外周
/// - 特殊文字のバックスラッシュエスケープ（往復安定）
/// - リスト継続行はマーカー幅ぶんインデント、引用は `> ` 前置
///
/// クリップボードは揮発的でありサーバ保存時は Rust が再正規化するため、
/// バイト単位一致ではなく「同じ意味・同じ正規形」を目標にする。

import { Fragment, type Mark, type Node as PMNode } from "@tiptap/pm/model";
import { marked } from "marked";

import { driveRefHref, isNodeId } from "@/lib/skills/drive-ref";

/// フラグメント（スライス内容・ドキュメント本体）を正規化 md へ描画する。
/// 末尾は単一改行（Rust `render_markdown` と同じく空でなければ改行終端）。
///
/// AI の削除の提案（`aiDeletion` の印）は、サーバの保存と同じく「承認したら」の形で描く
/// （旧文と新文を並べない・#886）。
export function serializeFragment(fragment: Fragment, options: SerializeOptions = {}): string {
  const prev = current;
  current = options;
  try {
    const blocks = collectBlocks(withoutDeletions(fragment));
    const body = renderBlocks(blocks);
    return body.length > 0 ? `${body}\n` : "";
  } finally {
    current = prev;
  }
}

export interface SerializeOptions {
  /// エスケープを「読み直しで記法に化けるもの」だけに絞る（スキルエディタ・#919）。
  /// SKILL.md は AI が読む指示文なので、`doc_search` を `doc\_search`、`<rules>` を
  /// `\<rules\>` のように書き換えない。`<…>` は読み直しでも文字のまま残る
  /// （[`parseMarkdownToNodes`] の `htmlAsText`・インライン HTML は文字）。
  literalText?: boolean;
}

/// 描画中のオプション（描画は同期なので、呼び出しの間だけ立てる）。
let current: SerializeOptions = {};

/// 表の形を保つため、中身が無くなっても残すノード（Rust の md 読み出しと同じ）。
const KEEP_EMPTY = new Set(["table", "tableRow", "tableCell", "tableHeader"]);

/// 削除の提案が付いた文字・ブロックを取り除いたフラグメント。
function withoutDeletions(fragment: Fragment): Fragment {
  const out: PMNode[] = [];
  fragment.forEach((node) => {
    const kept = withoutDeletionsIn(node);
    if (kept) out.push(kept);
  });
  return Fragment.fromArray(out);
}

function withoutDeletionsIn(node: PMNode): PMNode | null {
  if (node.attrs.aiDeletion === true) return null;
  if (node.isText) return node.marks.some((m) => m.type.name === "aiDeletion") ? null : node;
  if (node.childCount === 0) return node;
  const content = withoutDeletions(node.content);
  // 中身がすべて削除の提案だったブロック（段落・見出し・項目・リスト）は丸ごと描かない。
  // 文字を消して強制改行だけが残った段落も同じ（サーバの md 読み出しと揃える）。
  const removed = content.childCount !== node.childCount;
  const visible = content.content.some((child) => child.type.name !== "hardBreak");
  if (!KEEP_EMPTY.has(node.type.name) && (content.childCount === 0 || (removed && !visible))) {
    return null;
  }
  return node.copy(content);
}

/// フラグメントを配列に写す（forEach を素直な反復に）。
function collectBlocks(fragment: Fragment): PMNode[] {
  const out: PMNode[] = [];
  fragment.forEach((node) => out.push(node));
  return out;
}

/// ブロック列を空行区切りで描画する（各ブロックは末尾改行を含まない複数行文字列）。
function renderBlocks(blocks: PMNode[]): string {
  return blocks
    .map((block) => renderBlock(block))
    .filter((s) => s.length > 0)
    .join("\n\n");
}

/// 1 ブロックを描画する（複数行・末尾改行なし）。
function renderBlock(node: PMNode): string {
  switch (node.type.name) {
    case "paragraph": {
      // 段落の先頭行がブロック記法（見出し #・箇条書き -/+・番号 1.）に一致すると、
      // 素テキストで貼り戻したとき別ブロックに再解釈される。先頭行だけエスケープして
      // 往復で意味が変わらないようにする（継続行は同一段落に属するので対象外）。
      let rendered = renderInlineChildren(node);
      // literalText は素の文字に読み戻せる記号を残すので、強制改行の後の行頭も
      // [`escapeLiteralLineStart`] で引用・箇条書き・水平線・フェンスにならないようにする
      // （先頭行は下の escapeBlockStart が見る）。
      if (current.literalText) {
        rendered = rendered.replace(
          /(\\\n)([^\n]*)/g,
          (_, br: string, line: string) => br + escapeLiteralLineStart(line),
        );
      }
      const nl = rendered.indexOf("\n");
      return nl === -1
        ? escapeBlockStart(rendered)
        : escapeBlockStart(rendered.slice(0, nl)) + rendered.slice(nl);
    }
    case "heading": {
      const level = clampLevel(node.attrs.level);
      return `${"#".repeat(level)} ${renderInlineChildren(node)}`;
    }
    case "codeBlock": {
      const lang = typeof node.attrs.language === "string" ? node.attrs.language : "";
      return renderFence(lang, node.textContent);
    }
    case "shikiEmbed": {
      const payload = typeof node.attrs.payload === "string" ? node.attrs.payload : "";
      return renderFence("shiki-embed", payload);
    }
    case "blockquote":
      return prefixLines(renderBlocks(collectBlocks(node.content)), "> ", ">");
    case "bulletList":
      return renderList(node, () => "- ");
    case "orderedList": {
      const start = numberAttr(node.attrs.start, 1);
      return renderList(node, (i) => `${start + i}. `);
    }
    case "taskList":
      return renderTaskList(node);
    case "horizontalRule":
      return "---";
    case "table":
      return renderTable(node);
    default:
      // 未知のブロックは子のインライン/ブロックを素直に落とす（fail-open だが安全）。
      return node.isTextblock ? renderInlineChildren(node) : renderBlocks(collectBlocks(node.content));
  }
}

/// bullet/ordered リストを描画する（各項目は複数ブロック・ネスト可）。
function renderList(node: PMNode, marker: (index: number) => string): string {
  const items = collectBlocks(node.content);
  return items.map((item, i) => renderListItem(item, marker(i))).join("\n");
}

/// taskList を描画する（`- [x] ` / `- [ ] `）。
function renderTaskList(node: PMNode): string {
  const items = collectBlocks(node.content);
  return items
    .map((item) => {
      const checked = item.attrs.checked === true;
      return renderListItem(item, checked ? "- [x] " : "- [ ] ");
    })
    .join("\n");
}

/// リスト 1 項目を描画する。継続行はマーカー幅ぶんインデント（CommonMark 準拠・Rust と同じ）。
function renderListItem(item: PMNode, marker: string): string {
  const inner = renderListItemBody(item);
  const cont = " ".repeat(marker.length);
  const lines = inner.split("\n");
  return lines
    .map((line, i) => {
      if (i === 0) return `${marker}${line}`;
      return line.length === 0 ? "" : `${cont}${line}`;
    })
    .join("\n");
}

/// listItem/taskItem の中身を描画する。単段落項目は 1 行、ネストは空行なしで続ける。
function renderListItemBody(item: PMNode): string {
  const children = collectBlocks(item.content);
  const parts: string[] = [];
  children.forEach((child, i) => {
    const isNestedList =
      child.type.name === "bulletList" ||
      child.type.name === "orderedList" ||
      child.type.name === "taskList";
    const sep = i > 0 && !isNestedList ? "\n\n" : i > 0 ? "\n" : "";
    parts.push(sep + renderBlock(child));
  });
  return parts.join("");
}

/// GFM テーブルを描画する（ヘッダ＋区切り＋データ行）。
function renderTable(node: PMNode): string {
  const rows = collectBlocks(node.content);
  if (rows.length === 0) return "";
  const cellText = (row: PMNode): string[] =>
    collectBlocks(row.content).map((cell) =>
      renderInlineChildren(firstTextblock(cell) ?? cell)
        .replace(/\n/g, " ")
        .replace(/\|/g, "\\|"),
    );
  const header = cellText(rows[0]);
  const cols = Math.max(header.length, 1);
  const lines: string[] = [];
  lines.push(`| ${header.join(" | ")} |`);
  lines.push(`|${" --- |".repeat(cols)}`);
  for (const row of rows.slice(1)) {
    lines.push(`| ${cellText(row).join(" | ")} |`);
  }
  return lines.join("\n");
}

/// テーブルセル（tableHeader/tableCell）内の最初のテキストブロックを返す。
function firstTextblock(cell: PMNode): PMNode | null {
  let found: PMNode | null = null;
  cell.descendants((n) => {
    if (found) return false;
    if (n.isTextblock) {
      found = n;
      return false;
    }
    return true;
  });
  return found;
}

/// コード/埋め込みフェンスを描画する（末尾改行を正規化）。
/// フェンス長は内容中の最長バックティック連長 +1（最小 3）に追従させ、内容が ``` 行を
/// 含んでも早期終端しないようにする（往復安定）。
function renderFence(lang: string, code: string): string {
  const body = code.endsWith("\n") || code.length === 0 ? code : `${code}\n`;
  const fence = "`".repeat(Math.max(3, longestBacktickRun(code) + 1));
  return `${fence}${lang}\n${body}${fence}`;
}

/// 文字列中の連続バックティックの最長長を返す。
function longestBacktickRun(s: string): number {
  const runs = s.match(/`+/g);
  return runs ? Math.max(...runs.map((r) => r.length)) : 0;
}

// ---------------------------------------------------------------------------
// インライン
// ---------------------------------------------------------------------------

/// テキストブロックの子インラインを描画する。
function renderInlineChildren(node: PMNode): string {
  let out = "";
  node.forEach((child) => {
    if (child.type.name === "hardBreak") {
      out += "\\\n";
    } else if (child.type.name === "driveRef") {
      out += renderDriveRef(child);
    } else if (child.isText) {
      out += renderText(child.text ?? "", child.marks);
    }
  });
  return out;
}

/// マーク集合を持つ 1 テキストランを md へ描画する（Rust `render_marked_text` と同順）。
function renderText(text: string, marks: readonly Mark[]): string {
  if (text.length === 0) return "";
  const has = (name: string) => marks.some((m) => m.type.name === name);
  const link = marks.find((m) => m.type.name === "link");

  let body: string;
  if (has("code")) {
    // コードスパンは他マークを描画しない（md の意味論・排他）。
    body = renderCodeSpan(text);
  } else {
    body = escapeInline(text);
    if (has("strike")) body = `~~${body}~~`;
    if (has("italic")) body = `*${body}*`;
    if (has("bold")) body = `**${body}**`;
  }
  if (link) {
    const href = typeof link.attrs.href === "string" ? link.attrs.href : "";
    body = `[${body}](${escapeLinkDest(href)})`;
  }
  return body;
}

/// ドライブ参照（スキルエディタ・#919）: `[パス](shiki://drive/<kind>/<id>)`。
function renderDriveRef(node: PMNode): string {
  const kind = node.attrs.kind === "folder" ? "folder" : "file";
  const id = typeof node.attrs.id === "string" ? node.attrs.id : "";
  const label = typeof node.attrs.label === "string" && node.attrs.label ? node.attrs.label : id;
  // id が UUID の形でなければリンクにしない（リンク先へ文字を差し込ませない・名前だけ残す）。
  if (!isNodeId(id)) return escapeInline(label);
  // ラベルの `[`・`]` は必ず潰す（素の文字に読み戻せても、リンクのラベル内では括弧の対応を崩す）。
  const text = current.literalText ? escapeLiteralAll(label) : escapeInline(label);
  return `[${text}](${driveRefHref(kind, id)})`;
}

/// コードスパン: 内容のバッククォート連長より 1 長いフェンスで包む（CommonMark）。
function renderCodeSpan(text: string): string {
  const runs = text.match(/`+/g);
  const maxRun = runs ? Math.max(...runs.map((r) => r.length)) : 0;
  const fence = "`".repeat(maxRun + 1);
  if (text.startsWith("`") || text.endsWith("`")) {
    return `${fence} ${text} ${fence}`;
  }
  return `${fence}${text}${fence}`;
}

/// インライン特殊文字のバックスラッシュエスケープ（Rust `escape_inline` と一致）。
function escapeInline(text: string): string {
  if (current.literalText) return escapeLiteral(text);
  return text.replace(/[\\`*_[\]<>~|]/g, (c) => `\\${c}`);
}

/// `literalText` のエスケープ: 読み直して素の文字に戻るなら何もしない（`a*b`・`[必須]`・
/// `C:\share`・`doc_search` を書き換えない）。記法に化ける（強調・リンク・コード・エスケープ
/// になる）ときだけ [`escapeLiteralAll`] で潰す。インラインの `<…>` は読み込みで文字のまま
/// 残る（[`parseMarkdownToNodes`]）ので素の文字とみなす。行頭の `>` は [`escapeBlockStart`]。
function escapeLiteral(text: string): string {
  let tokens: { type: string; raw: string }[];
  try {
    tokens = marked.Lexer.lexInline(text) as unknown as { type: string; raw: string }[];
  } catch {
    return escapeLiteralAll(text);
  }
  const plain =
    tokens.every((t) => t.type === "text" || t.type === "html") &&
    tokens.map((t) => t.raw).join("") === text;
  return plain ? text : escapeLiteralAll(text);
}

/// 記法に化ける文字列のエスケープ（`<` `>` はそのまま・`_` は語の境目だけ）。
function escapeLiteralAll(text: string): string {
  const word = /[\p{L}\p{N}]/u;
  let out = "";
  for (let i = 0; i < text.length; i++) {
    const c = text[i];
    if (c === "_") {
      const inWord = word.test(text[i - 1] ?? "") && word.test(text[i + 1] ?? "");
      out += inWord ? c : "\\_";
    } else if ("\\`*[]~|".includes(c)) {
      out += `\\${c}`;
    } else {
      out += c;
    }
  }
  return out;
}

/// 段落先頭行のブロック記法をエスケープする（`escapeInline` 済みの文字列に適用）。
/// `*`/`>`/`` ` `` は `escapeInline` で既にエスケープ済みなので、残る `#`・`-`・`+`・
/// 番号付きリスト（`1.`/`1)`）だけを対象にする。
function escapeBlockStart(line: string): string {
  if (current.literalText) {
    const escaped = escapeLiteralLineStart(line);
    if (escaped !== line) return escaped;
  }
  const marker = /^(\s*)([#\-+])(\s|$)/.exec(line);
  if (marker) {
    return `${marker[1]}\\${line.slice(marker[1].length)}`;
  }
  const ordered = /^(\s*)(\d{1,9})([.)])(\s|$)/.exec(line);
  if (ordered) {
    // 区切り記号（`.`/`)`）の直前に `\` を入れて番号付きリスト化を断つ（`1\. …`）。
    const head = `${ordered[1]}${ordered[2]}`;
    return `${head}\\${line.slice(head.length)}`;
  }
  // 水平線（`---` / `- - -` 等・ハイフンのみ。`***`/`___` は escapeInline で処理済み）。
  const rule = /^(\s*)-[ \t-]*-[ \t-]*-[ \t-]*$/.exec(line);
  if (rule) {
    return `${rule[1]}\\${line.slice(rule[1].length)}`;
  }
  return line;
}

/// `literalText` の行頭: [`escapeLiteral`] は素の文字に読み戻せる記号（`>`・`*`・`` ` ``・`~`・
/// `_`）を残すが、行頭ではブロック記法（引用・`*` の箇条書き・`***`/`___` の水平線・
/// `` ``` ``/`~~~` のフェンス）になるので、先頭の記号だけ断つ。
function escapeLiteralLineStart(line: string): string {
  const m =
    /^(\s*)(?:>|\*(?=\s|$)|([*_])(?:[ \t]*\2){2,}[ \t]*$|`{3,}|~{3,})/.exec(line);
  if (!m) return line;
  return `${m[1]}\\${line.slice(m[1].length)}`;
}

/// リンク先: 空白・括弧を含む URL は <> で包む（Rust `escape_link_dest`）。
function escapeLinkDest(href: string): string {
  if (/[\s()]/.test(href)) return `<${href}>`;
  return href;
}

// ---------------------------------------------------------------------------
// 小物
// ---------------------------------------------------------------------------

/// 複数行本文へ行頭接頭辞を付ける（空行は emptyPrefix・引用の `>` 用）。
function prefixLines(body: string, prefix: string, emptyPrefix: string): string {
  return body
    .split("\n")
    .map((line) => (line.length === 0 ? emptyPrefix : `${prefix}${line}`))
    .join("\n");
}

function clampLevel(level: unknown): number {
  const n = numberAttr(level, 1);
  return Math.min(6, Math.max(1, n));
}

function numberAttr(value: unknown, fallback: number): number {
  return typeof value === "number" && Number.isFinite(value) ? value : fallback;
}
