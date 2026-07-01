"use client";

import {
  Download,
  Eye,
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
import { cn } from "@/lib/utils";

export type NodeAction =
  | "open"
  | "download"
  | "newversion"
  | "rename"
  | "move"
  | "share"
  | "versions"
  | "delete";

/// ノードの操作メニュー（行・カードで共有）。トリガは … ボタン、表示位置は align で調整。
export function NodeActionsMenu({
  node,
  onAction,
  align = "end",
  triggerClassName,
}: {
  node: NodeResponse;
  onAction: (action: NodeAction, node: NodeResponse) => void;
  align?: "start" | "center" | "end";
  triggerClassName?: string;
}) {
  const isFolder = node.kind === "folder";
  return (
    <DropdownMenu>
      <DropdownMenuTrigger asChild>
        <Button
          variant="ghost"
          size="icon"
          className={cn("size-8", triggerClassName)}
          aria-label={`「${node.name}」の操作`}
        >
          <MoreVertical className="size-4" aria-hidden />
        </Button>
      </DropdownMenuTrigger>
      <DropdownMenuContent align={align}>
        {!isFolder ? (
          <DropdownMenuItem onSelect={() => onAction("open", node)}>
            <Eye aria-hidden />
            開く
          </DropdownMenuItem>
        ) : null}
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
  );
}
