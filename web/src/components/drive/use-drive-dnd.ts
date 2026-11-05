"use client";

/// ドライブ内の D&D（項目をフォルダへドラッグして移動）。
///
/// - ドラッグ元は行・カード。運ぶのは独自 MIME（`DRIVE_NODE_MIME`）の JSON で、OS のファイル
///   ドロップ（`Files`）と区別する。外部アプリへ落としても意味を持たない値にする。
/// - ドロップ先はフォルダの行・カードとパンくず。自分自身・今いるフォルダ（=現在の親）へは
///   受けない（何も起きない移動を成功に見せない）。自身の配下への移動はクライアントでは
///   祖先を知らないため判定せず、サーバの拒否（400）をそのまま伝える。
/// - フォルダ行には OS のファイルも落とせる（そのフォルダへアップロード）。
/// - 認可はサーバ（StorageService）が判定する。ここは操作の入口だけで権限を推測しない。

import * as React from "react";

import type { NodeResponse } from "@/lib/storage";

export const DRIVE_NODE_MIME = "application/x-shiki-drive-node";

export type DraggedNode = Pick<NodeResponse, "id" | "kind" | "name">;

/// ドラッグ中の項目。dragover 中は dataTransfer の中身を読めない（types だけ読める）ため、
/// 自分自身へのドロップ可否を判定できるよう、ページ内で持っておく。
let current: DraggedNode | null = null;

export function isFileDrag(e: React.DragEvent): boolean {
  return Array.from(e.dataTransfer.types).includes("Files");
}

export function isNodeDrag(e: React.DragEvent): boolean {
  return Array.from(e.dataTransfer.types).includes(DRIVE_NODE_MIME);
}

/// ドラッグ元（行・カード）に付ける props。
export function nodeDragSource(node: NodeResponse, onEnd?: () => void) {
  return {
    draggable: true,
    onDragStart: (e: React.DragEvent) => {
      const payload: DraggedNode = { id: node.id, kind: node.kind, name: node.name };
      current = payload;
      e.dataTransfer.effectAllowed = "move";
      e.dataTransfer.setData(DRIVE_NODE_MIME, JSON.stringify(payload));
      // 外部（テキスト欄など）へ落とした時は名前だけが入る。
      e.dataTransfer.setData("text/plain", node.name);
    },
    onDragEnd: () => {
      current = null;
      onEnd?.();
    },
  };
}

export type DropHandlers = {
  /// 項目を `dest`（null=ドライブ直下）へ移す。
  onMoveNode: (node: DraggedNode, dest: string | null) => void;
  /// OS のファイルを `dest` へアップロードする。未指定ならファイルは受けない。
  onDropFiles?: (files: File[], dest: string | null) => void;
};

/// ドロップ先（フォルダ行・カード・パンくず）の props と「乗っている」状態。
/// `dest` は移動先のフォルダ id（null=ドライブ直下）、`disabled` なら一切受けない。
export function useDropTarget(
  dest: string | null,
  handlers: DropHandlers | undefined,
  opts: { disabled?: boolean } = {},
) {
  const [over, setOver] = React.useState(false);
  // 子要素の出入りで dragleave が連発するため、入った回数で数える。
  const depth = React.useRef(0);

  const accepts = (e: React.DragEvent) => {
    if (!handlers || opts.disabled) return false;
    if (isNodeDrag(e)) return current?.id !== dest;
    return Boolean(handlers.onDropFiles) && isFileDrag(e);
  };

  const reset = () => {
    depth.current = 0;
    setOver(false);
  };

  const props = {
    onDragEnter: (e: React.DragEvent) => {
      if (!accepts(e)) return;
      e.preventDefault();
      e.stopPropagation();
      depth.current += 1;
      setOver(true);
    },
    onDragOver: (e: React.DragEvent) => {
      if (!accepts(e)) return;
      // 受けるときだけ preventDefault（しないとブラウザはドロップ不可の表示にする）。
      e.preventDefault();
      e.stopPropagation();
      e.dataTransfer.dropEffect = isNodeDrag(e) ? "move" : "copy";
    },
    onDragLeave: (e: React.DragEvent) => {
      if (!accepts(e)) return;
      depth.current = Math.max(0, depth.current - 1);
      if (depth.current === 0) setOver(false);
    },
    onDrop: (e: React.DragEvent) => {
      if (!accepts(e) || !handlers) return;
      e.preventDefault();
      e.stopPropagation();
      reset();
      if (isNodeDrag(e)) {
        const raw = e.dataTransfer.getData(DRIVE_NODE_MIME);
        const node = parseDragged(raw);
        current = null;
        if (node && node.id !== dest) handlers.onMoveNode(node, dest);
        return;
      }
      const files = Array.from(e.dataTransfer.files);
      if (files.length > 0) handlers.onDropFiles?.(files, dest);
    },
  };

  return { over, props };
}

function parseDragged(raw: string): DraggedNode | null {
  try {
    const v = JSON.parse(raw) as Partial<DraggedNode>;
    if (typeof v.id === "string" && typeof v.name === "string" && (v.kind === "file" || v.kind === "folder")) {
      return { id: v.id, name: v.name, kind: v.kind };
    }
  } catch {
    /* 別ページ・別アプリ由来の壊れた値は無視する */
  }
  return null;
}

/// ドロップ先として乗っている間の見た目（行・カード・パンくず共通）。
export const DROP_OVER = "bg-primary/10 ring-2 ring-inset ring-primary/50";
