"use client";

import * as React from "react";
import { Search } from "lucide-react";
import { VisuallyHidden } from "@radix-ui/react-visually-hidden";

import {
  Dialog,
  DialogContent,
  DialogDescription,
  DialogTitle,
} from "@/components/ui/dialog";
import { EmptyState } from "@/components/ui/empty-state";

/// 検索パレット。backend の全文検索は未実装のため、入力 UI ＋ 空状態のみ。
/// ⌘K / Ctrl+K でも開ける。
export function SidebarSearch({
  open,
  onOpenChange,
}: {
  open: boolean;
  onOpenChange: (v: boolean) => void;
}) {
  const [query, setQuery] = React.useState("");

  React.useEffect(() => {
    const onKey = (e: KeyboardEvent) => {
      if ((e.metaKey || e.ctrlKey) && e.key.toLowerCase() === "k") {
        e.preventDefault();
        onOpenChange(true);
      }
    };
    window.addEventListener("keydown", onKey);
    return () => window.removeEventListener("keydown", onKey);
  }, [onOpenChange]);

  return (
    <Dialog open={open} onOpenChange={onOpenChange}>
      <DialogContent showClose={false} className="top-[20%] translate-y-0 gap-0 p-0">
        <VisuallyHidden>
          <DialogTitle>検索</DialogTitle>
          <DialogDescription>チャットやドライブを横断検索します。</DialogDescription>
        </VisuallyHidden>
        <div className="flex items-center gap-3 border-b border-border px-4">
          <Search className="size-4 shrink-0 text-muted-foreground" aria-hidden />
          <input
            autoFocus
            value={query}
            onChange={(e) => setQuery(e.target.value)}
            placeholder="チャット・ドライブを検索…"
            aria-label="検索キーワード"
            className="h-12 w-full bg-transparent text-sm outline-none placeholder:text-muted-foreground"
          />
          <kbd className="hidden rounded border border-border px-1.5 py-0.5 text-[10px] text-muted-foreground sm:inline-block">
            ESC
          </kbd>
        </div>
        <div className="p-2">
          <EmptyState
            icon={Search}
            title={query ? "該当する結果はありません" : "キーワードを入力してください"}
            description="横断検索は今後のアップデートで利用できるようになります。"
            className="border-0 py-10"
          />
        </div>
      </DialogContent>
    </Dialog>
  );
}
