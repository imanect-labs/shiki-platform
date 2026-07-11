"use client";

import { Lock, Users } from "lucide-react";

import type { NodeResponse } from "@/lib/storage";
import { formatBytes, formatDateTime } from "@/lib/format";
import { cn } from "@/lib/utils";

import { NodeActionsMenu, type NodeAction } from "./node-menu";
import { LIST_GRID, NodeIcon } from "./primitives";

export type { NodeAction };

/// 一覧の 1 行。フォルダは開く、ファイルはダウンロード。… メニューから各操作。
export function NodeRow({
  node,
  onAction,
}: {
  node: NodeResponse;
  onAction: (action: NodeAction, node: NodeResponse) => void;
}) {
  const isFolder = node.kind === "folder";
  // フォルダは開く、ファイルはビューアで開く（ダウンロードはメニューから）。
  const primary = () => onAction("open", node);

  return (
    <div
      className={cn(
        "group shiki-dash-bottom px-3 py-2.5 transition-colors last:bg-none hover:bg-accent",
        LIST_GRID,
      )}
    >
      <button
        type="button"
        onClick={primary}
        className="flex min-w-0 items-center gap-3.5 text-left"
      >
        <NodeIcon kind={node.kind} name={node.name} contentType={node.content_type} className="size-7 shrink-0" />
        <span className="truncate text-[15px] font-medium">{node.name}</span>
      </button>

      <span className="hidden truncate text-[13px] text-muted-foreground sm:block">
        {formatDateTime(node.updated_at)}
      </span>
      <span className="hidden truncate text-[13px] text-muted-foreground lg:block">
        {/* 最終更新者（updated_by・11P.10）。AI 編集は AI 主体名義で表示される。 */}
        {node.updated_by}
      </span>
      <span className="hidden truncate text-[13px] text-muted-foreground sm:block">
        {isFolder ? "—" : formatBytes(node.size_bytes)}
      </span>
      <span className="hidden items-center gap-1.5 truncate text-[13px] lg:flex">
        {/* shared フラグも後続 PR で提供。未提供時は「プライベート」表示に degrade する。 */}
        {(node as { shared?: boolean }).shared ? (
          <>
            <Users className="size-3.5 shrink-0 text-foreground/70" aria-hidden />
            <span className="truncate text-foreground/80">共有</span>
          </>
        ) : (
          <>
            <Lock className="size-3.5 shrink-0 text-muted-foreground/55" aria-hidden />
            <span className="truncate text-muted-foreground">プライベート</span>
          </>
        )}
      </span>

      <NodeActionsMenu
        node={node}
        onAction={onAction}
        triggerClassName="opacity-0 transition-opacity group-hover:opacity-100 data-[state=open]:opacity-100"
      />
    </div>
  );
}
