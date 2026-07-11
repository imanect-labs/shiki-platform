"use client";

/// ノート埋め込みブロックの TipTap ノード（Task 11P.6）。
///
/// Yjs 側の `shikiEmbed` 要素（`payload` 属性・backend の Block::Embed と対）に対応する
/// **アトムのブロックノード**。中身は編集不可で、[`EmbedView`] が 3 種のみを安全に描画する
/// （生 HTML/JSX は一切描画しない）。md では ```shiki-embed フェンス（JSON）へ往復する。

import { Node, mergeAttributes } from "@tiptap/core";
import { NodeViewWrapper, ReactNodeViewRenderer, type NodeViewProps } from "@tiptap/react";
import * as React from "react";

import { EmbedView } from "./embed-view";
import { serializeEmbedPayload, type EmbedPayload } from "./types";

declare module "@tiptap/core" {
  interface Commands<ReturnType> {
    shikiEmbed: {
      /// 検証済みペイロードで埋め込みブロックを挿入する（3 種のみ）。
      insertShikiEmbed: (payload: EmbedPayload) => ReturnType;
    };
  }
}

function EmbedNodeView({ node }: NodeViewProps) {
  const payload = (node.attrs.payload as string) ?? "";
  return (
    <NodeViewWrapper
      className="shiki-embed"
      data-testid="note-embed"
      // 選択・削除はできるが中身は編集不可（アトム）。
      contentEditable={false}
    >
      <EmbedView payloadJson={payload} />
    </NodeViewWrapper>
  );
}

export const ShikiEmbed = Node.create({
  name: "shikiEmbed",
  group: "block",
  atom: true,
  selectable: true,
  draggable: true,

  addAttributes() {
    return {
      // ペイロードは JSON 文字列（Yjs `shikiEmbed` の `payload` 属性と 1:1）。
      payload: {
        default: "",
        parseHTML: (el) => el.getAttribute("data-payload") ?? "",
        renderHTML: (attrs) => ({ "data-payload": attrs.payload as string }),
      },
    };
  },

  parseHTML() {
    return [{ tag: "div[data-shiki-embed]" }];
  },

  renderHTML({ HTMLAttributes }) {
    // 静的シリアライズ時も生 HTML は出さない（data 属性に JSON を載せるだけ）。
    return ["div", mergeAttributes(HTMLAttributes, { "data-shiki-embed": "true" })];
  },

  addNodeView() {
    return ReactNodeViewRenderer(EmbedNodeView);
  },

  addCommands() {
    return {
      insertShikiEmbed:
        (payload: EmbedPayload) =>
        ({ commands, state }) =>
          // 現在位置へ挿入し、カーソルを直後（末尾）へ置く。atom を NodeSelection の
          // まま残すと次の入力/挿入が置換になるため、insertContentAt で位置指定する。
          commands.insertContentAt(state.selection.to, {
            type: this.name,
            attrs: { payload: serializeEmbedPayload(payload) },
          }),
    };
  },
});
