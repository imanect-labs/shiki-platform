"use client";

/// ドライブのフォルダを選ぶダイアログ（skill の知識スコープ用・Task 6.11）。
/// DrivePicker（ファイル添付用）のフォルダ選択版: フォルダは辿れ、「このフォルダを選択」で確定する。

import * as React from "react";
import { ChevronLeft, FolderPlus } from "lucide-react";

import { listChildren, type NodeResponse } from "@/lib/storage";
import { useInfiniteList, useInfiniteSentinel } from "@/hooks/use-infinite-list";
import { NodeIcon, LoadingRow } from "@/components/drive/primitives";
import { Button } from "@/components/ui/button";
import {
  Dialog,
  DialogContent,
  DialogHeader,
  DialogTitle,
  DialogDescription,
} from "@/components/ui/dialog";

export function FolderPicker({
  open,
  onOpenChange,
  onSelect,
}: {
  open: boolean;
  onOpenChange: (open: boolean) => void;
  onSelect: (folder: { id: string; name: string }) => void;
}) {
  // (id, name) のスタック。先頭はルート（マイドライブ）。
  const [stack, setStack] = React.useState<{ id?: string; name: string }[]>([
    { name: "マイドライブ" },
  ]);
  const current = stack[stack.length - 1];

  const fetchPage = React.useCallback(
    (cursor?: string) => {
      if (!open) return Promise.resolve({ items: [] as NodeResponse[], next_cursor: null });
      return listChildren({ parentId: current.id, cursor, limit: 50 });
    },
    [open, current.id],
  );
  const list = useInfiniteList<NodeResponse>(fetchPage, [open, current.id]);
  const sentinelRef = useInfiniteSentinel(list.loadMore, open && list.hasMore && !list.loading);

  React.useEffect(() => {
    if (open) setStack([{ name: "マイドライブ" }]);
  }, [open]);

  const folders = list.items.filter((n) => n.kind === "folder");

  return (
    <Dialog open={open} onOpenChange={onOpenChange}>
      <DialogContent className="max-w-2xl">
        <DialogHeader>
          <DialogTitle>フォルダを選択</DialogTitle>
          <DialogDescription>
            知識スコープに含めるフォルダを選んでください（配下のファイルすべてが対象になります）。
          </DialogDescription>
        </DialogHeader>

        <div className="flex items-center justify-between gap-2">
          {stack.length > 1 ? (
            <button
              type="button"
              onClick={() => setStack((s) => s.slice(0, -1))}
              className="flex items-center gap-1 text-sm text-muted-foreground hover:text-foreground"
            >
              <ChevronLeft className="size-4" aria-hidden /> 戻る
            </button>
          ) : (
            <span className="text-sm text-muted-foreground">{current.name}</span>
          )}
          {current.id ? (
            <Button
              type="button"
              size="sm"
              onClick={() => {
                onSelect({ id: current.id!, name: current.name });
                onOpenChange(false);
              }}
            >
              <FolderPlus className="size-4" aria-hidden />
              「{current.name}」を選択
            </Button>
          ) : null}
        </div>

        <div className="max-h-[55vh] min-h-[280px] overflow-y-auto rounded-lg border border-border">
          {list.loading ? (
            <LoadingRow />
          ) : folders.length === 0 && !list.hasMore ? (
            <p className="px-3 py-10 text-center text-sm text-muted-foreground">
              ここにはフォルダがありません。
            </p>
          ) : (
            <ul className="divide-y divide-border/60">
              {folders.map((n) => (
                <li key={n.id} className="flex items-center gap-2 pr-2 transition-colors hover:bg-secondary">
                  <button
                    type="button"
                    onClick={() => setStack((s) => [...s, { id: n.id, name: n.name }])}
                    className="flex min-w-0 flex-1 items-center gap-3.5 px-4 py-3 text-left text-[15px]"
                  >
                    <NodeIcon kind={n.kind} name={n.name} contentType={n.content_type} className="size-7 shrink-0" />
                    <span className="truncate">{n.name}</span>
                  </button>
                  <Button
                    type="button"
                    size="sm"
                    variant="outline"
                    onClick={() => {
                      onSelect({ id: n.id, name: n.name });
                      onOpenChange(false);
                    }}
                  >
                    選択
                  </Button>
                </li>
              ))}
              {list.hasMore ? (
                <li>
                  <div ref={sentinelRef}>{list.loadingMore ? <LoadingRow /> : null}</div>
                </li>
              ) : null}
            </ul>
          )}
        </div>
      </DialogContent>
    </Dialog>
  );
}
