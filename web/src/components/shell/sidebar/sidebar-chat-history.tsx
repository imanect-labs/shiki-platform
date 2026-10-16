"use client";

import * as React from "react";
import Link from "next/link";
import { usePathname, useSearchParams } from "next/navigation";
import { FileText, Link2, Loader2, MessageSquareText, NotebookPen } from "lucide-react";

import { cn } from "@/lib/utils";
import { groupThreadsByDate, type Thread } from "@/lib/chat-api";
import { useInfiniteSentinel } from "@/hooks/use-infinite-list";
import { useThreadList } from "@/hooks/use-thread-list";
import { threadHref, threadOriginKind } from "@/lib/thread-link";
import { toast } from "@/components/ui/use-toast";
import { ActiveIndicator } from "@/components/ui/motion-primitives";

/// 履歴 1 行。アクティブは左アクセントバー、ホバーでリンクコピーの小ボタンを出す
/// （スレッドのリネーム/削除は backend 未提供のため、実在する「コピー」のみ）。
///
/// **文書由来スレッド**は先頭に由来のアイコンを出し、行のリンク先を**その文書の画面（この会話を
/// アクティブに）**にする（文書からも履歴からも辿れる）。ノート由来（issue #282）はノートの分割
/// ビュー、Office 文書由来（#598）は Office 画面。通常チャットは従来どおり /c/{id}。
function ThreadRow({
  thread,
  active,
  onNavigate,
}: {
  thread: Thread;
  active: boolean;
  onNavigate?: () => void;
}) {
  const { title, originNoteName } = thread;
  const origin = threadOriginKind(thread);
  const href = threadHref(thread);
  // コピーボタンは Link の兄弟（インタラクティブ要素のネストを避ける・有効な HTML）。
  const copyLink = async () => {
    try {
      await navigator.clipboard.writeText(`${window.location.origin}${href}`);
      toast({ description: "リンクをコピーしました。" });
    } catch {
      toast({ description: "リンクをコピーできませんでした。" });
    }
  };

  return (
    <li className="group/thread relative isolate">
      {active ? (
        <ActiveIndicator
          layoutId="sidebar-active-thread"
          className="absolute inset-0 -z-10 rounded-[9px] bg-sidebar-accent"
        />
      ) : null}
      <Link
        href={href}
        // モバイルのドロワーは行を選んだら閉じる（ナビ項目と同じ・開いたままだと遷移先が隠れる）。
        onClick={onNavigate}
        aria-current={active ? "page" : undefined}
        title={
          origin === "note"
            ? `ノート「${originNoteName ?? ""}」の会話`
            : origin === "office"
              ? `文書「${originNoteName ?? ""}」の会話`
              : title
        }
        className={cn(
          "flex h-8 items-center gap-1.5 rounded-[9px] pl-2.5 pr-9 text-[13px] outline-none",
          "transition-colors focus-visible:ring-2 focus-visible:ring-sidebar-ring",
          active
            ? "font-medium text-sidebar-foreground"
            : "text-sidebar-foreground/75 hover:bg-sidebar-accent/60 hover:text-sidebar-foreground",
        )}
      >
        {origin === "note" ? (
          <NotebookPen
            className="size-3.5 shrink-0 text-sidebar-foreground/45"
            aria-label="ノート由来"
          />
        ) : origin === "office" ? (
          <FileText
            className="size-3.5 shrink-0 text-sidebar-foreground/45"
            aria-label="Office 文書由来"
          />
        ) : null}
        <span className="min-w-0 flex-1 truncate">{title}</span>
      </Link>
      <button
        type="button"
        onClick={copyLink}
        aria-label="リンクをコピー"
        className={cn(
          "absolute right-1 top-1/2 flex size-6 -translate-y-1/2 items-center justify-center rounded-md text-sidebar-foreground/55 outline-none",
          "opacity-0 transition-opacity duration-[var(--duration-fast)] focus-visible:opacity-100",
          "hover:bg-sidebar-accent hover:text-sidebar-foreground group-hover/thread:opacity-100",
          "focus-visible:ring-2 focus-visible:ring-sidebar-ring active:scale-90",
        )}
      >
        <Link2 className="size-3.5" aria-hidden />
      </button>
    </li>
  );
}

/// 今の画面がこの会話か（文書由来は文書の画面かつ `?thread=` が一致したときだけ）。
function isActiveThread(thread: Thread, pathname: string, activeThreadId: string | null): boolean {
  const [path] = threadHref(thread).split("?");
  if (path === `/c/${thread.id}`) return pathname === path;
  return pathname === path && activeThreadId === thread.id;
}

/// サイドバー中段のチャット履歴。backend のスレッドを日付グループで表示する。
/// 末尾までスクロールすると続きを読み足す（先頭 30 件以外にも辿り着ける・#591）。
///
/// `useSearchParams`（ノート由来行の active 判定に使用）を含むため Suspense 境界でラップする
/// （静的プリレンダされる (auth) ページ /apps 等で CSR bailout を起こさない・#282）。
export function SidebarChatHistory({
  collapsed,
  onNavigate,
}: {
  collapsed: boolean;
  onNavigate?: () => void;
}) {
  return (
    <React.Suspense fallback={<div className="min-h-0 flex-1" aria-hidden />}>
      <SidebarChatHistoryInner collapsed={collapsed} onNavigate={onNavigate} />
    </React.Suspense>
  );
}

function SidebarChatHistoryInner({
  collapsed,
  onNavigate,
}: {
  collapsed: boolean;
  onNavigate?: () => void;
}) {
  const { threads, loading, loadingMore, hasMore, loadMore } = useThreadList();
  const sentinelRef = useInfiniteSentinel(loadMore, hasMore && !loading);
  const pathname = usePathname();
  // 文書由来行は `/notes/{id}?thread=` ・ `/office/{id}?thread=` へ張るため、active 判定は thread
  // クエリまで見る（同一文書に複数会話がある 1:N で、全行が同時 active になるのを防ぐ）。
  const searchParams = useSearchParams();

  if (collapsed) return <div className="flex-1" aria-hidden />;

  // 取得前はスケルトン（空状態のちらつきを防ぐ）。
  if (loading) {
    return (
      <div className="min-h-0 flex-1 overflow-hidden px-2.5 pb-2 pt-3">
        <div className="flex flex-col gap-1.5">
          {[68, 52, 60, 44, 56].map((w, i) => (
            <div key={i} className="flex h-8 items-center px-2.5">
              <div className="h-3.5 rounded bg-sidebar-foreground/10" style={{ width: `${w}%` }} />
            </div>
          ))}
        </div>
      </div>
    );
  }

  if (threads.length === 0) {
    return (
      <div className="scrollbar-subtle min-h-0 flex-1 overflow-y-auto px-2.5 pb-2">
        <div className="flex flex-col items-center gap-2 px-3 py-7 text-center">
          <MessageSquareText className="size-5 text-sidebar-foreground/35" aria-hidden />
          <p className="text-xs leading-relaxed text-sidebar-foreground/45">
            まだチャットはありません。
            <br />
            「新しいチャット」から始めましょう。
          </p>
        </div>
      </div>
    );
  }

  const groups = groupThreadsByDate(threads);

  return (
    <div className="scrollbar-subtle min-h-0 flex-1 overflow-y-auto px-2.5 pb-2">
      {groups.map((group) => (
        <div key={group.label} className="mb-1">
          <div className="px-2.5 pb-1 pt-3 text-[11px] font-semibold uppercase tracking-[0.06em] text-sidebar-foreground/40">
            {group.label}
          </div>
          <ul className="flex flex-col gap-0.5">
            {group.threads.map((thread) => (
              <ThreadRow
                key={thread.id}
                thread={thread}
                active={isActiveThread(thread, pathname, searchParams.get("thread"))}
                onNavigate={onNavigate}
              />
            ))}
          </ul>
        </div>
      ))}
      {hasMore ? (
        <div ref={sentinelRef} className="flex h-8 items-center justify-center" aria-hidden>
          {loadingMore ? (
            <Loader2 className="size-3.5 animate-spin text-sidebar-foreground/40" />
          ) : null}
        </div>
      ) : null}
    </div>
  );
}
