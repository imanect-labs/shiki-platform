"use client";

import * as React from "react";
import Link from "next/link";
import { usePathname } from "next/navigation";
import { Link2, MessageSquareText } from "lucide-react";

import { cn } from "@/lib/utils";
import { groupThreadsByDate, useThreadsState } from "@/lib/chat-api";
import { toast } from "@/components/ui/use-toast";
import { ActiveIndicator } from "@/components/ui/motion-primitives";

/// 履歴 1 行。アクティブは左アクセントバー、ホバーでリンクコピーの小ボタンを出す
/// （スレッドのリネーム/削除は backend 未提供のため、実在する「コピー」のみ）。
function ThreadRow({ id, title, active }: { id: string; title: string; active: boolean }) {
  // コピーボタンは Link の兄弟（インタラクティブ要素のネストを避ける・有効な HTML）。
  const copyLink = async () => {
    try {
      await navigator.clipboard.writeText(`${window.location.origin}/c/${id}`);
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
        href={`/c/${id}`}
        aria-current={active ? "page" : undefined}
        title={title}
        className={cn(
          "flex h-8 items-center rounded-[9px] pl-2.5 pr-9 text-[13px] outline-none",
          "transition-colors focus-visible:ring-2 focus-visible:ring-sidebar-ring",
          active
            ? "font-medium text-sidebar-foreground"
            : "text-sidebar-foreground/75 hover:bg-sidebar-accent/60 hover:text-sidebar-foreground",
        )}
      >
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

/// サイドバー中段のチャット履歴。backend のスレッドを日付グループで表示する。
export function SidebarChatHistory({ collapsed }: { collapsed: boolean }) {
  const { threads, loading } = useThreadsState();
  const pathname = usePathname();

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
                id={thread.id}
                title={thread.title}
                active={pathname === `/c/${thread.id}`}
              />
            ))}
          </ul>
        </div>
      ))}
    </div>
  );
}
