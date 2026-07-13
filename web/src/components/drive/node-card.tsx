"use client";

import { Lock, Users } from "lucide-react";

import type { NodeResponse } from "@/lib/storage";
import { formatBytes, formatDateTime } from "@/lib/format";

import { NodeActionsMenu, type NodeAction } from "./node-menu";
import { NodeIcon } from "./primitives";

/// グリッド表示の 1 枚。アイコンを大きく見せるカード。フォルダは開く、ファイルはビューアへ。
/// 操作メニューは右上にホバーで出す（行表示と同じ NodeActionsMenu を共有）。
export function NodeCard({
  node,
  onAction,
}: {
  node: NodeResponse;
  onAction: (action: NodeAction, node: NodeResponse) => void;
}) {
  const isFolder = node.kind === "folder";
  return (
    <div className="group relative min-w-0 overflow-hidden rounded-xl border border-border/60 bg-card/40 transition-[transform,box-shadow,border-color,background-color] duration-[var(--duration-fast)] ease-[var(--ease-standard)] hover:-translate-y-0.5 hover:border-border hover:bg-accent hover:shadow-md">
      <div className="absolute right-1 top-1 z-10">
        <NodeActionsMenu
          node={node}
          onAction={onAction}
          align="end"
          triggerClassName="bg-card/70 opacity-0 transition-opacity group-hover:opacity-100 data-[state=open]:opacity-100"
        />
      </div>
      <button
        type="button"
        onClick={() => onAction("open", node)}
        className="flex w-full flex-col gap-2.5 p-3 text-left outline-none"
      >
        <div className="flex h-24 items-center justify-center rounded-lg bg-muted/40">
          <NodeIcon
            kind={node.kind}
            name={node.name}
            contentType={node.content_type}
            className="size-11"
          />
        </div>
        <div className="min-w-0">
          <p className="truncate text-[14px] font-medium">{node.name}</p>
          <div className="mt-1 flex items-center gap-1.5 text-[12px] text-muted-foreground">
            {/* shared フラグはバックエンド実装が入る後続 PR で提供。未提供時は Lock に degrade。 */}
            {(node as { shared?: boolean }).shared ? (
              <Users className="size-3 shrink-0 text-foreground/60" aria-hidden />
            ) : (
              <Lock className="size-3 shrink-0 text-muted-foreground/50" aria-hidden />
            )}
            <span className="truncate">
              {isFolder ? "フォルダ" : formatBytes(node.size_bytes)}
              {" · "}
              {formatDateTime(node.updated_at)}
            </span>
          </div>
        </div>
      </button>
    </div>
  );
}
