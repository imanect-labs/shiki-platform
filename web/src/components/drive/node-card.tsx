"use client";

import * as React from "react";
import { Lock, Users } from "lucide-react";

import type { NodeResponse } from "@/lib/storage";
import { formatBytes, formatDateTime } from "@/lib/format";
import { cn } from "@/lib/utils";

import { NodeActionsMenu, useLongPress, type NodeAction } from "./node-menu";
import { ROW_MENU_TRIGGER } from "./node-row";
import { NodeIcon, formatShortDate, isShared } from "./primitives";

/// グリッド表示の 1 枚。アイコンを大きく見せるカード。フォルダは開く、ファイルはビューアへ。
/// 操作メニューは右上（マウスはホバーで、タッチは常時）。行表示と同じ NodeActionsMenu を共有し、
/// スマホでは長押しでも同じアクションシートを開く。
export function NodeCard({
  node,
  onAction,
}: {
  node: NodeResponse;
  onAction: (action: NodeAction, node: NodeResponse) => void;
}) {
  const isFolder = node.kind === "folder";
  const [menuOpen, setMenuOpen] = React.useState(false);
  const longPress = useLongPress(() => setMenuOpen(true));
  return (
    <div
      className={cn(
        "group relative min-w-0 overflow-hidden rounded-xl border border-border/60 bg-card/40 transition-[transform,box-shadow,border-color,background-color] duration-[var(--duration-fast)] ease-[var(--ease-standard)] hover:-translate-y-0.5 hover:border-border hover:bg-accent hover:shadow-md",
        "pointer-coarse:select-none pointer-coarse:[-webkit-touch-callout:none]",
        menuOpen && "border-border bg-accent",
      )}
      {...longPress}
    >
      <div className="absolute right-1 top-1 z-10">
        <NodeActionsMenu
          node={node}
          onAction={onAction}
          align="end"
          open={menuOpen}
          onOpenChange={setMenuOpen}
          triggerClassName={cn("bg-card/70", ROW_MENU_TRIGGER)}
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
          {/* 長い名前は 2 行まで（右上の ⋯ に重ならないよう名前は本文側にだけ置く）。 */}
          <p className="line-clamp-2 text-[14px] font-medium leading-snug [overflow-wrap:anywhere]">
            {node.name}
          </p>
          <div className="mt-1 flex items-center gap-1.5 text-[12px] text-muted-foreground">
            {/* shared フラグはバックエンド実装が入る後続 PR で提供。未提供時は Lock に degrade。 */}
            {isShared(node) ? (
              <Users className="size-3 shrink-0 text-foreground/60" aria-hidden />
            ) : (
              <Lock className="size-3 shrink-0 text-muted-foreground/50" aria-hidden />
            )}
            <span className="truncate">
              {isFolder ? "フォルダ" : formatBytes(node.size_bytes)}
              {"・"}
              {/* 狭いカードでも切れないよう短い日付（時刻はホバーの title で補う）。 */}
              <span title={formatDateTime(node.updated_at)}>{formatShortDate(node.updated_at)} 更新</span>
            </span>
          </div>
        </div>
      </button>
    </div>
  );
}
