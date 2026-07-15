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

import type { Fragment, Mark, Node as PMNode } from "@tiptap/pm/model";

/// フラグメント（スライス内容・ドキュメント本体）を正規化 md へ描画する。
/// 末尾は単一改行（Rust `render_markdown` と同じく空でなければ改行終端）。
export function serializeFragment(fragment: Fragment): string {
  const blocks = collectBlocks(fragment);
  const body = renderBlocks(blocks);
  return body.length > 0 ? `${body}\n` : "";
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
      const rendered = renderInlineChildren(node);
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
  return text.replace(/[\\`*_[\]<>~|]/g, (c) => `\\${c}`);
}

/// 段落先頭行のブロック記法をエスケープする（`escapeInline` 済みの文字列に適用）。
/// `*`/`>`/`` ` `` は `escapeInline` で既にエスケープ済みなので、残る `#`・`-`・`+`・
/// 番号付きリスト（`1.`/`1)`）だけを対象にする。
function escapeBlockStart(line: string): string {
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
