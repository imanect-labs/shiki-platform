"use client";

import Link from "next/link";
import { usePathname } from "next/navigation";
import { MessageSquareText } from "lucide-react";

import { cn } from "@/lib/utils";
import { groupSessionsByDate, useChatSessions } from "@/lib/chat-store";

/// サイドバー中段のチャット履歴（スクロール領域）。
/// chat-store（localStorage）のセッションを日付グループで表示する。まだ無ければ
/// フェイク履歴を置かず空状態を出す。レール時は領域ごと隠す。
export function SidebarChatHistory({ collapsed }: { collapsed: boolean }) {
  const sessions = useChatSessions();
  const pathname = usePathname();

  if (collapsed) return <div className="flex-1" aria-hidden />;

  if (sessions.length === 0) {
    return (
      <div className="min-h-0 flex-1 overflow-y-auto px-2.5 pb-2">
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

  const groups = groupSessionsByDate(sessions);

  return (
    <div className="min-h-0 flex-1 overflow-y-auto px-2.5 pb-2">
      {groups.map((group) => (
        <div key={group.label} className="mb-1">
          <div className="px-2.5 pb-1 pt-3 text-[11px] font-semibold uppercase tracking-[0.06em] text-sidebar-foreground/40">
            {group.label}
          </div>
          <ul className="flex flex-col gap-0.5">
            {group.sessions.map((session) => {
              const active = pathname === `/c/${session.id}`;
              return (
                <li key={session.id}>
                  <Link
                    href={`/c/${session.id}`}
                    aria-current={active ? "page" : undefined}
                    className={cn(
                      "block truncate rounded-md px-2.5 py-1.5 text-[13px] outline-none transition-colors focus-visible:ring-2 focus-visible:ring-sidebar-ring",
                      active
                        ? "bg-sidebar-accent font-medium text-sidebar-foreground"
                        : "text-sidebar-foreground/75 hover:bg-sidebar-accent/60 hover:text-sidebar-foreground",
                    )}
                    title={session.title}
                  >
                    {session.title}
                  </Link>
                </li>
              );
            })}
          </ul>
        </div>
      ))}
    </div>
  );
}
