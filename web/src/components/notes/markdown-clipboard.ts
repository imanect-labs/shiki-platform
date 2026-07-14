/// クリップボードの Markdown 変換（Task 11P.3 / issue #297 A・B）。
///
/// - **A（コピー）**: `clipboardTextSerializer` で `text/plain` を正規化 Markdown にする。
///   既定の `text/html`（リッチ）はそのまま残すため、Docs/Slack 等リッチ先の体験は不変。
/// - **B（ペースト）**: `handlePaste` で「`text/html` を持たない＝素テキスト」かつ「ブロック
///   Markdown を含む」場合だけ構造へ変換する。単段落やインラインだけの素テキストは既定
///   （StarterKit の paste rule／インライン結合）に委ね、UX を壊さない。
///
/// 共同編集セーフ: 変換結果は通常の編集トランザクションとして dispatch する（Yjs 同期は
/// 既存経路そのまま）。ドキュメント全体の置換は行わない。
///
/// セキュリティ: `text/html` を持つリッチ経路は既定パーサ（DOMParser・スキーマ経由）に
/// 委ね、素テキスト経路は [`parseMarkdownToNodes`] がスキーマノードだけを生成する。生 HTML
/// ノードや埋め込みノードは作らない（stored XSS / confused-deputy 遮断・issue #297 注意点）。

import { Extension } from "@tiptap/core";
import { Slice } from "@tiptap/pm/model";
import { Plugin, PluginKey } from "@tiptap/pm/state";

import {
  looksLikeBlockMarkdown,
  parseMarkdownToNodes,
} from "@/lib/notes/markdown-parse";
import { serializeFragment } from "@/lib/notes/markdown-serialize";

export const MarkdownClipboard = Extension.create({
  name: "markdownClipboard",

  addProseMirrorPlugins() {
    return [
      new Plugin({
        key: new PluginKey("markdownClipboard"),
        props: {
          // A: コピー時の text/plain を正規化 Markdown にする（text/html は既定のまま）。
          clipboardTextSerializer: (slice) => serializeFragment(slice.content),

          // B: 素テキストのブロック Markdown をブロックノードへ変換する。
          handlePaste: (view, event) => {
            const data = event.clipboardData;
            if (!data) return false;
            // リッチ経路（text/html あり）は既定に委ねて構造を保持する。
            const html = data.getData("text/html");
            if (html && html.trim().length > 0) return false;

            const text = data.getData("text/plain");
            if (!text || !looksLikeBlockMarkdown(text)) return false;

            const nodes = parseMarkdownToNodes(text);
            let slice: Slice;
            try {
              const doc = view.state.schema.nodeFromJSON({ type: "doc", content: nodes });
              slice = new Slice(doc.content, 0, 0);
            } catch {
              // スキーマ非適合はフォールバック（既定の素テキスト貼り付け）。
              return false;
            }
            view.dispatch(view.state.tr.replaceSelection(slice).scrollIntoView());
            return true;
          },
        },
      }),
    ];
  },
});
