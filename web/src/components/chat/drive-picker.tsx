"use client";

import * as React from "react";
import { ChevronLeft } from "lucide-react";

import { listChildren, type NodeResponse } from "@/lib/storage";
import { NodeIcon } from "@/components/drive/primitives";
import {
  Dialog,
  DialogContent,
  DialogHeader,
  DialogTitle,
  DialogDescription,
} from "@/components/ui/dialog";

/// ドライブから添付するファイルを選ぶダイアログ。フォルダは辿れ、ファイルを選ぶと閉じる。
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
  const [items, setItems] = React.useState<NodeResponse[]>([]);
  const [loading, setLoading] = React.useState(false);
  const parentId = stack[stack.length - 1];

  React.useEffect(() => {
    if (!open) return;
    let active = true;
    setLoading(true);
    listChildren({ parentId, limit: 100 })
      .then((r) => active && setItems(r.items ?? []))
      .catch(() => active && setItems([]))
      .finally(() => active && setLoading(false));
    return () => {
      active = false;
    };
  }, [open, parentId]);

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
          {loading ? (
            <p className="px-3 py-10 text-center text-sm text-muted-foreground">読み込み中…</p>
          ) : items.length === 0 ? (
            <p className="px-3 py-10 text-center text-sm text-muted-foreground">
              ここにはファイルがありません。
            </p>
          ) : (
            <ul className="divide-y divide-border/60">
              {items.map((n) => {
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
            </ul>
          )}
        </div>
      </DialogContent>
    </Dialog>
  );
}
