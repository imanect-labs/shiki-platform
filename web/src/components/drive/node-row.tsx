"use client";

import * as React from "react";
import {
  Download,
  FilePlus2,
  FolderInput,
  History,
  MoreVertical,
  Pencil,
  Share2,
  Trash2,
} from "lucide-react";

import { Button } from "@/components/ui/button";
import {
  DropdownMenu,
  DropdownMenuContent,
  DropdownMenuItem,
  DropdownMenuSeparator,
  DropdownMenuTrigger,
} from "@/components/ui/dropdown-menu";
import type { NodeResponse } from "@/lib/storage";
import { formatBytes, formatDateTime } from "@/lib/format";
import { cn } from "@/lib/utils";

import { NodeIcon } from "./primitives";

export type NodeAction =
  | "open"
  | "download"
  | "newversion"
  | "rename"
  | "move"
  | "share"
  | "versions"
  | "delete";

/// 一覧の 1 行。フォルダは開く、ファイルはダウンロード。… メニューから各操作。
export function NodeRow({
  node,
  onAction,
}: {
  node: NodeResponse;
  onAction: (action: NodeAction, node: NodeResponse) => void;
}) {
  const isFolder = node.kind === "folder";
  const primary = () => onAction(isFolder ? "open" : "download", node);

  return (
    <div
      className={cn(
        "group flex items-center gap-3 rounded-lg px-3 py-2.5 transition-colors hover:bg-accent",
      )}
    >
      <button
        type="button"
        onClick={primary}
        className="flex min-w-0 flex-1 items-center gap-3 text-left"
      >
        <NodeIcon kind={node.kind} />
        <div className="min-w-0 flex-1">
          <p className="truncate text-sm font-medium">{node.name}</p>
          <p className="truncate text-xs text-muted-foreground">
            {formatDateTime(node.updated_at)}
            {isFolder ? "" : ` · ${formatBytes(node.size_bytes)}`}
          </p>
        </div>
      </button>

      <DropdownMenu>
        <DropdownMenuTrigger asChild>
          <Button
            variant="ghost"
            size="icon"
            className="size-8 opacity-0 transition-opacity group-hover:opacity-100 data-[state=open]:opacity-100"
            aria-label={`「${node.name}」の操作`}
          >
            <MoreVertical className="size-4" aria-hidden />
          </Button>
        </DropdownMenuTrigger>
        <DropdownMenuContent align="end">
          {!isFolder ? (
            <DropdownMenuItem onSelect={() => onAction("download", node)}>
              <Download aria-hidden />
              ダウンロード
            </DropdownMenuItem>
          ) : null}
          <DropdownMenuItem onSelect={() => onAction("rename", node)}>
            <Pencil aria-hidden />
            名前を変更
          </DropdownMenuItem>
          <DropdownMenuItem onSelect={() => onAction("move", node)}>
            <FolderInput aria-hidden />
            移動
          </DropdownMenuItem>
          <DropdownMenuItem onSelect={() => onAction("share", node)}>
            <Share2 aria-hidden />
            共有
          </DropdownMenuItem>
          {!isFolder ? (
            <DropdownMenuItem onSelect={() => onAction("newversion", node)}>
              <FilePlus2 aria-hidden />
              新しいバージョン
            </DropdownMenuItem>
          ) : null}
          {!isFolder ? (
            <DropdownMenuItem onSelect={() => onAction("versions", node)}>
              <History aria-hidden />
              版履歴
            </DropdownMenuItem>
          ) : null}
          <DropdownMenuSeparator />
          <DropdownMenuItem variant="destructive" onSelect={() => onAction("delete", node)}>
            <Trash2 aria-hidden />
            ゴミ箱へ移動
          </DropdownMenuItem>
        </DropdownMenuContent>
      </DropdownMenu>
    </div>
  );
}
