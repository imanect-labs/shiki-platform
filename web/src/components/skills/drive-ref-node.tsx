"use client";

/// スキル本文のドライブ参照ノード（#919）。
///
/// インラインのアトムで、属性は `{kind, id, label}`。Markdown では
/// `[label](shiki://drive/<kind>/<id>)` に往復する（[`@/lib/notes/markdown-serialize`]）。
/// 描画は [`DriveRefChip`]（閲覧者本人の権限で引いたパスの青いラベル）。

import { Node, mergeAttributes } from "@tiptap/core";
import { NodeViewWrapper, ReactNodeViewRenderer, type NodeViewProps } from "@tiptap/react";
import * as React from "react";

import { driveRefHref, type DriveRef, type DriveRefKind } from "@/lib/skills/drive-ref";
import { DriveRefChip } from "./drive-ref-chip";

declare module "@tiptap/core" {
  interface Commands<ReturnType> {
    driveRef: {
      /// 現在位置（選択があれば置き換え）へドライブ参照を挿入し、直後に空白を置く。
      insertDriveRef: (ref: DriveRef) => ReturnType;
    };
  }
}

function DriveRefView({ node, selected }: NodeViewProps) {
  const kind: DriveRefKind = node.attrs.kind === "folder" ? "folder" : "file";
  return (
    <NodeViewWrapper as="span" className="drive-ref" contentEditable={false}>
      <DriveRefChip
        kind={kind}
        id={String(node.attrs.id ?? "")}
        label={String(node.attrs.label ?? "")}
        selected={selected}
      />
    </NodeViewWrapper>
  );
}

export const DriveRefNode = Node.create({
  name: "driveRef",
  group: "inline",
  inline: true,
  atom: true,
  selectable: true,

  addAttributes() {
    return {
      kind: { default: "file" },
      id: { default: "" },
      label: { default: "" },
    };
  },

  parseHTML() {
    return [
      {
        tag: "a[data-drive-ref]",
        getAttrs: (el) => ({
          kind: el.getAttribute("data-kind") === "folder" ? "folder" : "file",
          id: el.getAttribute("data-node-id") ?? "",
          label: el.textContent ?? "",
        }),
      },
    ];
  },

  renderHTML({ node, HTMLAttributes }) {
    // エディタ内のコピー（HTML）でも参照のまま往復できるよう、data 属性に載せる。
    return [
      "a",
      mergeAttributes(HTMLAttributes, {
        "data-drive-ref": "true",
        "data-kind": node.attrs.kind,
        "data-node-id": node.attrs.id,
        href: driveRefHref(node.attrs.kind === "folder" ? "folder" : "file", node.attrs.id),
      }),
      node.attrs.label || node.attrs.id,
    ];
  },

  renderText({ node }) {
    return node.attrs.label || node.attrs.id;
  },

  addNodeView() {
    return ReactNodeViewRenderer(DriveRefView, { as: "span" });
  },

  addCommands() {
    return {
      insertDriveRef:
        (ref: DriveRef) =>
        ({ commands }) =>
          commands.insertContent([
            { type: this.name, attrs: { kind: ref.kind, id: ref.id, label: ref.label } },
            { type: "text", text: " " },
          ]),
    };
  },
});
