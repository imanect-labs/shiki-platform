"use client";

/// 発言の並び。日付の区切り・連続発言のまとめ・未読ラインを組み立てる。

import * as React from "react";

import { type Message } from "@/lib/messages-mock";
import { MessageRow } from "./message-row";

function DayDivider({ label }: { label: string }) {
  return (
    <div className="sticky top-0 z-[5] flex items-center justify-center py-2">
      <span className="shiki-dash-x absolute inset-x-5 top-1/2 h-px" aria-hidden />
      <span className="relative rounded-full border border-border/60 bg-background px-2.5 py-0.5 text-[11px] font-medium text-muted-foreground">
        {label}
      </span>
    </div>
  );
}

/// 直前の発言と同じ人・同じ時刻なら見出しを省いて詰める（Slack 風の連続表示）。
function isGrouped(prev: Message | undefined, cur: Message): boolean {
  if (!prev) return false;
  if (prev.authorId !== cur.authorId) return false;
  if (prev.dayLabel !== cur.dayLabel) return false;
  if (prev.ai || cur.ai) return false;
  return prev.time.slice(0, 2) === cur.time.slice(0, 2);
}

export function MessageList({
  messages,
  viewerId,
  firstUnreadId,
  activeThreadId,
  compact,
  hideDayDividers,
  onToggleReaction,
  onOpenThread,
  onAskAi,
}: {
  messages: Message[];
  viewerId: string;
  firstUnreadId?: string;
  activeThreadId?: string | null;
  compact?: boolean;
  /// スレッド内では日付の区切りを出さない（親と返信で二重に出るため）。
  hideDayDividers?: boolean;
  onToggleReaction: (messageId: string, emoji: string) => void;
  onOpenThread?: (messageId: string) => void;
  onAskAi?: (messageId: string) => void;
}) {
  return (
    <div className="flex flex-col pb-3">
      {messages.map((m, i) => {
        const prev = messages[i - 1];
        const showDay = !hideDayDividers && prev?.dayLabel !== m.dayLabel;
        return (
          <React.Fragment key={m.id}>
            {showDay ? <DayDivider label={m.dayLabel} /> : null}
            {/* スレッド展開中の行は塗りで示す（設計言語: 選択は bg-accent の塗り・枠は使わない）。 */}
            <div className={activeThreadId === m.id ? "bg-accent/60" : undefined}>
              <MessageRow
                message={m}
                viewerId={viewerId}
                grouped={!showDay && isGrouped(prev, m)}
                unreadBoundary={m.id === firstUnreadId}
                compact={compact}
                onToggleReaction={(e) => onToggleReaction(m.id, e)}
                onOpenThread={onOpenThread ? () => onOpenThread(m.id) : undefined}
                onAskAi={onAskAi ? () => onAskAi(m.id) : undefined}
              />
            </div>
          </React.Fragment>
        );
      })}
    </div>
  );
}
