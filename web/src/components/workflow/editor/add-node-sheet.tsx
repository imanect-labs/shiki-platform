"use client";

/// ブロック追加の下からのシート（スマホ）。中身はキャンバスの＋と同じ AddNodeMenu。

import * as React from "react";

import { Sheet, SheetContent, SheetDescription, SheetHeader, SheetTitle } from "@/components/ui/sheet";
import { AddNodeMenu } from "./add-node-menu";

export function AddNodeSheet({
  open,
  onOpenChange,
  contextLabel,
  onPick,
}: {
  open: boolean;
  onOpenChange: (open: boolean) => void;
  contextLabel: string;
  onPick: (nodeType: string) => void;
}) {
  return (
    <Sheet open={open} onOpenChange={onOpenChange}>
      <SheetContent
        side="bottom"
        className="h-[80dvh]"
        // 開いた瞬間に検索欄へフォーカスしてキーボードを出さない（一覧から選ぶのが主）。
        onOpenAutoFocus={(e) => e.preventDefault()}
      >
        <SheetHeader className="pr-14">
          <SheetTitle>ブロックを追加</SheetTitle>
          <SheetDescription>{contextLabel}</SheetDescription>
        </SheetHeader>
        <div className="flex min-h-0 flex-1 flex-col px-4 pb-3 pt-3">
          <AddNodeMenu
            className="max-h-none w-full flex-1"
            onPick={(nodeType) => {
              onOpenChange(false);
              onPick(nodeType);
            }}
          />
        </div>
      </SheetContent>
    </Sheet>
  );
}
