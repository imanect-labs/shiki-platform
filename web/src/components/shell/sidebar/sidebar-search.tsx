"use client";

import * as React from "react";
import { useRouter } from "next/navigation";
import { ArrowUpRight, MessageSquare, PenSquare, Search, X } from "lucide-react";
import { VisuallyHidden } from "@radix-ui/react-visually-hidden";

import { cn } from "@/lib/utils";
import {
  Dialog,
  DialogClose,
  DialogContent,
  DialogDescription,
  DialogTitle,
} from "@/components/ui/dialog";
import {
  groupThreadsByDate,
  useThreads,
  type Thread,
} from "@/lib/chat-api";
import type { DriveSearchItem } from "@/lib/drive-search";

import { DriveResults, useDriveSearch } from "./sidebar-doc-results";

/// 検索パレット（画像2 風）。⌘K / Ctrl+K / "/" で開く。
/// キーワード入力中は最上部に「"q"で検索 ↗︎」（ドライブの検索結果画面へ遷移）、
/// 続けてドライブのフォルダ/ファイル（名前一致＋内容一致・スコア降順）、
/// チャット履歴の順。未入力時は「新しいチャット」＋履歴一覧。
export function SidebarSearch({
  open,
  onOpenChange,
}: {
  open: boolean;
  onOpenChange: (v: boolean) => void;
}) {
  const router = useRouter();
  const [query, setQuery] = React.useState("");
  const threads = useThreads();
  const drive = useDriveSearch(query, open);

  React.useEffect(() => {
    const isEditable = (el: EventTarget | null) => {
      if (!(el instanceof HTMLElement)) return false;
      const tag = el.tagName;
      return tag === "INPUT" || tag === "TEXTAREA" || tag === "SELECT" || el.isContentEditable;
    };
    const onKey = (e: KeyboardEvent) => {
      const cmdK = (e.metaKey || e.ctrlKey) && e.key.toLowerCase() === "k";
      // "/" 単独でも開く（サイドバーのキーヒントと一致させる）。入力中は無効。
      const slash =
        e.key === "/" && !e.metaKey && !e.ctrlKey && !e.altKey && !isEditable(e.target);
      if (cmdK || slash) {
        e.preventDefault();
        onOpenChange(true);
      }
    };
    window.addEventListener("keydown", onKey);
    return () => window.removeEventListener("keydown", onKey);
  }, [onOpenChange]);

  // 開くたびにクエリをリセット（前回の入力を持ち越さない）。
  React.useEffect(() => {
    if (open) setQuery("");
  }, [open]);

  const filtered = React.useMemo(() => {
    const q = query.trim().toLowerCase();
    if (!q) return threads;
    return threads.filter((t) => t.title.toLowerCase().includes(q));
  }, [threads, query]);

  const groups = groupThreadsByDate(filtered);

  const go = (href: string) => {
    onOpenChange(false);
    router.push(href);
  };
  const openDriveItem = (item: DriveSearchItem) => {
    go(item.targetFolderId ? `/drive?folder=${item.targetFolderId}` : "/drive");
  };
  const openDriveSearch = () => {
    go(`/drive?q=${encodeURIComponent(query.trim())}`);
  };

  return (
    <Dialog open={open} onOpenChange={onOpenChange}>
      <DialogContent showClose={false} className="top-[12%] max-w-xl translate-y-0 gap-0 p-0">
        <VisuallyHidden>
          <DialogTitle>検索</DialogTitle>
          <DialogDescription>
            文書（ドライブ）とチャット履歴の検索、新規チャットの開始ができます。
          </DialogDescription>
        </VisuallyHidden>

        {/* 検索入力＋閉じる */}
        <div className="flex items-center gap-3 border-b border-border px-4">
          <Search className="size-[18px] shrink-0 text-muted-foreground" aria-hidden />
          <input
            autoFocus
            value={query}
            onChange={(e) => setQuery(e.target.value)}
            placeholder="文書・チャットを検索..."
            aria-label="文書・チャットを検索"
            className="h-14 w-full bg-transparent text-[15px] outline-none placeholder:text-muted-foreground focus-visible:ring-0 focus-visible:ring-offset-0"
          />
          <DialogClose
            aria-label="閉じる"
            className="rounded-md p-1 text-muted-foreground transition-colors hover:text-foreground focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-ring"
          >
            <X className="size-[18px]" aria-hidden />
          </DialogClose>
        </div>

        {/* 結果リスト */}
        <div className="scrollbar-subtle max-h-[60vh] overflow-y-auto p-2">
          {query.trim() ? (
            /* キーワード入力中の最上段: ドライブの検索結果画面へ */
            <button
              type="button"
              onClick={openDriveSearch}
              className="flex w-full items-center gap-3 rounded-lg px-3 py-2.5 text-left text-sm text-foreground outline-none transition-colors hover:bg-accent focus-visible:bg-accent"
            >
              <Search className="size-[18px] shrink-0 text-muted-foreground" aria-hidden />
              <span className="min-w-0 truncate">「{query.trim()}」で検索</span>
              <ArrowUpRight className="ml-auto size-4 shrink-0 text-muted-foreground" aria-hidden />
            </button>
          ) : (
            /* 未入力時の先頭アクション */
            <button
              type="button"
              onClick={() => go("/")}
              className="flex w-full items-center gap-3 rounded-lg px-3 py-2.5 text-left text-sm text-foreground outline-none transition-colors hover:bg-accent focus-visible:bg-accent"
            >
              <PenSquare className="size-[18px] shrink-0 text-muted-foreground" aria-hidden />
              新しいチャット
            </button>
          )}

          {/* ドライブ（名前一致＋内容一致の統合・スコア降順。読める文書だけがヒット） */}
          <DriveResults
            query={query}
            items={drive.items}
            loading={drive.loading}
            onOpen={openDriveItem}
          />

          {groups.length > 0 ? (
            groups.map((group) => (
              <div key={group.label} className="mt-1">
                <div className="px-3 pb-1 pt-2 text-[11px] font-medium text-muted-foreground/70">
                  {group.label}
                </div>
                <ul>
                  {group.threads.map((thread) => (
                    <SearchResultRow
                      key={thread.id}
                      thread={thread}
                      onSelect={() => go(`/c/${thread.id}`)}
                    />
                  ))}
                </ul>
              </div>
            ))
          ) : (
            <p className="px-3 py-8 text-center text-sm text-muted-foreground">
              {query.trim() ? "該当するチャットはありません" : "まだチャットはありません"}
            </p>
          )}
        </div>
      </DialogContent>
    </Dialog>
  );
}

function SearchResultRow({ thread, onSelect }: { thread: Thread; onSelect: () => void }) {
  return (
    <li>
      <button
        type="button"
        onClick={onSelect}
        className={cn(
          "flex w-full items-center gap-3 rounded-lg px-3 py-2.5 text-left text-sm outline-none transition-colors",
          "text-foreground hover:bg-accent focus-visible:bg-accent",
        )}
      >
        <MessageSquare className="size-[18px] shrink-0 text-muted-foreground" aria-hidden />
        <span className="truncate">{thread.title}</span>
      </button>
    </li>
  );
}
