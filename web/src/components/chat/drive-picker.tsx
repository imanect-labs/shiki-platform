"use client";

import * as React from "react";
import { ChevronLeft } from "lucide-react";

import { listChildren, type NodeResponse } from "@/lib/storage";
import { useInfiniteList, useInfiniteSentinel } from "@/hooks/use-infinite-list";
import { NodeIcon, LoadingRow } from "@/components/drive/primitives";
import {
  Dialog,
  DialogContent,
  DialogHeader,
  DialogTitle,
  DialogDescription,
} from "@/components/ui/dialog";

/// ドライブから添付するファイルを選ぶダイアログ。フォルダは辿れ、ファイルを選ぶと閉じる。
/// 一覧は next_cursor を消費する無限スクロール（100 件超のフォルダでも辿り着ける）。
export function DrivePicker({
  open,
  onOpenChange,
  onSelect,
}: {
  open: boolean;
  onOpenChange: (open: boolean) => void;
  onSelect: (node: NodeResponse) => void;
}) {
  const [stack, setStack] = React.useState<(string | undefined)[]>([undefined]);
  const parentId = stack[stack.length - 1];

  const fetchPage = React.useCallback(
    (cursor?: string) => {
      if (!open) return Promise.resolve({ items: [] as NodeResponse[], next_cursor: null });
      return listChildren({ parentId, cursor, limit: 50 });
    },
    [open, parentId],
  );
  const list = useInfiniteList<NodeResponse>(fetchPage, [open, parentId]);
  const sentinelRef = useInfiniteSentinel(list.loadMore, open && list.hasMore && !list.loading);

  // 開くたびにルートへ戻す。
  React.useEffect(() => {
    if (open) setStack([undefined]);
  }, [open]);

  return (
    <Dialog open={open} onOpenChange={onOpenChange}>
      <DialogContent className="max-w-2xl">
        <DialogHeader>
          <DialogTitle>ドライブから選択</DialogTitle>
          <DialogDescription>添付するファイルを選んでください。</DialogDescription>
        </DialogHeader>

        {stack.length > 1 ? (
          <button
            type="button"
            onClick={() => setStack((s) => s.slice(0, -1))}
            className="mb-1 flex items-center gap-1 text-sm text-muted-foreground hover:text-foreground"
          >
            <ChevronLeft className="size-4" /> 戻る
          </button>
        ) : null}

        <div className="max-h-[60vh] min-h-[320px] overflow-y-auto rounded-lg border border-border">
          {list.loading ? (
            <LoadingRow />
          ) : list.error ? (
            <div className="px-3 py-10 text-center text-sm">
              <p className="text-destructive">読み込みに失敗しました。</p>
              <button
                type="button"
                onClick={list.reload}
                className="mt-2 text-muted-foreground underline underline-offset-2 hover:text-foreground"
              >
                再試行
              </button>
            </div>
          ) : list.items.length === 0 ? (
            <p className="px-3 py-10 text-center text-sm text-muted-foreground">
              ここにはファイルがありません。
            </p>
          ) : (
            <ul className="divide-y divide-border/60">
              {list.items.map((n) => {
                const isFolder = n.kind === "folder";
                return (
                  <li key={n.id}>
                    <button
                      type="button"
                      onClick={() => {
                        if (isFolder) setStack((s) => [...s, n.id]);
                        else {
                          onSelect(n);
                          onOpenChange(false);
                        }
                      }}
                      className="flex w-full items-center gap-3.5 px-4 py-3 text-left text-[15px] transition-colors hover:bg-secondary"
                    >
                      <NodeIcon
                        kind={n.kind}
                        name={n.name}
                        contentType={n.content_type}
                        className="size-7 shrink-0"
                      />
                      <span className="truncate">{n.name}</span>
                    </button>
                  </li>
                );
              })}
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
