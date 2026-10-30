/// Markdown の本文から、見えている文字だけを取り出す（ブロックの間は空白で区切る）。
/// スレッドの仮タイトルに使う（`**`・`#`・`- `・`` ` `` などの記法を題名に残さない）。
///
/// marked を引くので、使う所で動的に読み込む（全ページの初回読み込みに載せない）。

import { parseMarkdownToNodes, type PmJson } from "@/lib/notes/markdown-parse";

export function markdownPlainText(md: string): string {
  const out: string[] = [];
  const walk = (node: PmJson) => {
    if (typeof node.text === "string") out.push(node.text);
    else if (node.type === "hardBreak") out.push(" ");
    node.content?.forEach(walk);
    if (node.type !== "text") out.push(" ");
  };
  parseMarkdownToNodes(md, { htmlAsText: true }).forEach(walk);
  return out.join("");
}
