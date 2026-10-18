"use client";

import * as React from "react";
import { useRouter } from "next/navigation";
import {
  ArrowUpRight,
  FileText,
  Loader2,
  MessageSquare,
  NotebookPen,
  PenSquare,
  Search,
  X,
} from "lucide-react";
import { VisuallyHidden } from "@radix-ui/react-visually-hidden";

import { cn } from "@/lib/utils";
import {
  Dialog,
  DialogClose,
  DialogContent,
  DialogDescription,
  DialogTitle,
} from "@/components/ui/dialog";
import { groupThreadsByDate, type Thread } from "@/lib/chat-api";
import { threadDisplay, threadHref } from "@/lib/thread-link";
import { useInfiniteSentinel } from "@/hooks/use-infinite-list";
import { useThreadList } from "@/hooks/use-thread-list";
import type { DriveSearchItem } from "@/lib/drive-search";

import { DriveResults, useDriveSearch } from "./sidebar-doc-results";
import { useSidebar } from "./sidebar-context";

/// チャット検索語の上限（`GET /threads?q=` のサーバ側上限と一致）。
const MAX_CHAT_QUERY_CHARS = 200;

/// 検索パレット（画像2 風）。⌘K / Ctrl+K / "/" で開く。
/// キーワード入力中は最上部に「"q"で検索 ↗︎」（ドライブの検索結果画面へ遷移）、
/// 続けてドライブのフォルダ/ファイル（名前一致＋内容一致・スコア降順）、
/// チャット履歴の順。未入力時は「新しいチャット」＋履歴一覧。
/// チャットはサーバ側でタイトル検索し、スクロールで続きを読み足す（先頭 30 件に限らない・#591）。
/// スマホ（< sm）は全画面。検索が目的の画面なので、タッチでも開いた瞬間に入力へフォーカスする。
export function SidebarSearch({
  open,
  onOpenChange,
}: {
  open: boolean;
  onOpenChange: (v: boolean) => void;
}) {
  const router = useRouter();
  // スマホのドロワから開いた場合も、結果を選んだらドロワごと閉じる（遷移先が隠れないように）。
  const { setMobileOpen } = useSidebar();
  const [query, setQuery] = React.useState("");
  const drive = useDriveSearch(query, open);
  // 打鍵ごとに叩かない（ドライブ検索と同じ 300ms）。
  const [debounced, setDebounced] = React.useState("");
  React.useEffect(() => {
    const t = setTimeout(() => setDebounced(query.trim()), 300);
    return () => clearTimeout(t);
  }, [query]);
  // サーバの上限（200 文字）を超える語はチャット検索を送らない（ドライブ検索とは欄を共用）。
  const tooLong = [...debounced].length > MAX_CHAT_QUERY_CHARS; // サーバと同じく文字（コードポイント）数で数える
  const chats = useThreadList(debounced, open && !tooLong);
  const sentinelRef = useInfiniteSentinel(chats.loadMore, open && chats.hasMore && !chats.loading);
  // 入力が確定（debounce）するまでは、手元の行をその場で絞って見せる（打鍵への即応）。
  // 確定後はサーバの結果に差し替わる。確定前と先頭ページの取得中は「該当なし」を出さない。
  const pending = debounced !== query.trim();
  const searching = !tooLong && (chats.loading || pending);
  const shown = React.useMemo(() => {
    const q = query.trim().toLowerCase();
    if (tooLong) return [];
    if (!pending || !q) return chats.threads;
    return chats.threads.filter((t) => t.title.toLowerCase().includes(q));
  }, [chats.threads, pending, query, tooLong]);

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

  // 閉じたらクエリをリセット（前回の入力を持ち越さない）。debounce 済みの語も同時に空へ戻し、
  // 再表示の直後に前回の検索を投げ直したり、その結果を空欄に出したりしない。
  React.useEffect(() => {
    if (!open) {
      setQuery("");
      setDebounced("");
    }
  }, [open]);

  const groups = groupThreadsByDate(shown);

  const go = (href: string) => {
    onOpenChange(false);
    setMobileOpen(false);
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
      <DialogContent
        showClose={false}
        className={cn(
          "gap-0 p-0 sm:top-[12%] sm:max-w-xl sm:translate-y-0",
          // スマホは全画面（下からのシートの取っ手は出さない）。結果リストだけが伸びてスクロールする。
          "max-sm:inset-0 max-sm:flex max-sm:h-dvh max-sm:max-h-dvh max-sm:flex-col max-sm:overflow-hidden max-sm:rounded-none max-sm:border-0",
          "max-sm:px-0 max-sm:pt-[env(safe-area-inset-top)] max-sm:pb-[env(safe-area-inset-bottom)] max-sm:[&>div:first-child]:hidden",
        )}
      >
        <VisuallyHidden>
          <DialogTitle>検索</DialogTitle>
          <DialogDescription>
            文書（ドライブ）とチャット履歴の検索、新規チャットの開始ができます。
          </DialogDescription>
        </VisuallyHidden>

        {/* 検索入力＋閉じる */}
        <div className="flex shrink-0 items-center gap-3 border-b border-border px-4 max-sm:pr-1.5">
          <Search className="size-[18px] shrink-0 text-muted-foreground" aria-hidden />
          <input
            autoFocus
            value={query}
            onChange={(e) => setQuery(e.target.value)}
            onKeyDown={(e) => {
              // キーボードの「検索」キー（Enter）でドライブの検索結果画面へ（IME の確定は除く）。
              if (e.key === "Enter" && !e.nativeEvent.isComposing && query.trim()) {
                e.preventDefault();
                openDriveSearch();
              }
            }}
            enterKeyHint="search"
            autoComplete="off"
            placeholder="文書・チャットを検索..."
            aria-label="文書・チャットを検索"
            className="h-14 w-full min-w-0 bg-transparent text-base outline-none placeholder:text-muted-foreground focus-visible:ring-0 focus-visible:ring-offset-0 sm:text-[15px]"
          />
          <DialogClose
            aria-label="閉じる"
            className="flex shrink-0 items-center justify-center rounded-md p-1 text-muted-foreground transition-colors hover:text-foreground focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-ring max-sm:size-11 pointer-coarse:size-11"
          >
            <X className="size-[18px]" aria-hidden />
          </DialogClose>
        </div>

        {/* 結果リスト */}
        <div className="scrollbar-subtle overflow-y-auto overscroll-contain p-2 max-sm:min-h-0 max-sm:flex-1 sm:max-h-[60dvh]">
          {query.trim() ? (
            /* キーワード入力中の最上段: ドライブの検索結果画面へ */
            <button
              type="button"
              onClick={openDriveSearch}
              className="flex w-full items-center gap-3 rounded-lg px-3 py-2.5 text-left text-sm text-foreground outline-none transition-colors hover:bg-accent focus-visible:bg-accent active:bg-accent max-sm:min-h-12 max-sm:text-[15px]"
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
              className="flex w-full items-center gap-3 rounded-lg px-3 py-2.5 text-left text-sm text-foreground outline-none transition-colors hover:bg-accent focus-visible:bg-accent active:bg-accent max-sm:min-h-12 max-sm:text-[15px]"
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
                      onSelect={() => go(threadHref(thread))}
                    />
                  ))}
                </ul>
              </div>
            ))
          ) : searching ? (
            <p className="flex items-center justify-center gap-2 px-3 py-8 text-sm text-muted-foreground">
              <Loader2 className="size-4 animate-spin" aria-hidden />
              チャットを検索中...
            </p>
          ) : (
            <p className="px-3 py-8 text-center text-sm text-muted-foreground">
              {tooLong
                ? `チャットは ${MAX_CHAT_QUERY_CHARS} 文字までの語で検索できます`
                : chats.error
                  ? "チャットを検索できませんでした。時間をおいてもう一度お試しください"
                  : query.trim()
                    ? "該当するチャットはありません"
                    : "まだチャットはありません"}
            </p>
          )}
          {chats.hasMore && groups.length > 0 ? (
            <div ref={sentinelRef} className="flex h-10 items-center justify-center" aria-hidden>
              {chats.loadingMore ? (
                <Loader2 className="size-4 animate-spin text-muted-foreground" />
              ) : null}
            </div>
          ) : null}
        </div>
      </DialogContent>
    </Dialog>
  );
}

function SearchResultRow({ thread, onSelect }: { thread: Thread; onSelect: () => void }) {
  // 文書の横で始めた会話は接頭「ノート: 」等を外し、種類アイコンと補足で由来を示す（サイドバーと揃える）。
  const { kind, title, detail } = threadDisplay(thread);
  const Icon = kind === "note" ? NotebookPen : kind === "office" ? FileText : MessageSquare;
  return (
    <li>
      <button
        type="button"
        onClick={onSelect}
        className={cn(
          "flex w-full items-center gap-3 rounded-lg px-3 py-2.5 text-left text-sm outline-none transition-colors",
          "text-foreground hover:bg-accent focus-visible:bg-accent active:bg-accent max-sm:min-h-12 max-sm:text-[15px]",
        )}
      >
        <Icon className="size-[18px] shrink-0 text-muted-foreground" aria-hidden />
        <span className="min-w-0 flex-1">
          <span className="block truncate">{title}</span>
          {detail ? (
            <span className="block truncate text-xs text-muted-foreground">{detail}</span>
          ) : null}
        </span>
      </button>
    </li>
  );
}
