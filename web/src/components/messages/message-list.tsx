"use client";

/// 発言の並び。日付の区切り・連続発言のまとめ・未読ライン・自分だけに表示の回答の差し込みを組み立てる。

import * as React from "react";

import type { AgentReplyView, MemberView, MessageBlock, MessageView } from "@/lib/messaging-api";
import { AgentReplyCard } from "./agent-reply-card";
import { MessageRow } from "./message-row";
import { dayLabel } from "./time";

function DayDivider({ label }: { label: string }) {
  return (
    <div className="sticky top-0 z-[5] flex items-center justify-center bg-background/95 py-2 backdrop-blur-sm" role="separator" aria-label={label}>
      <span className="shiki-dash-x absolute inset-x-5 top-1/2 h-px" aria-hidden />
      <span className="relative rounded-full border border-border/60 bg-background px-2.5 py-0.5 text-[11px] font-medium text-muted-foreground">
        {label}
      </span>
    </div>
  );
}

/// 直前の発言と同じ書き手・5 分以内なら見出しを省いて詰める。
function isGrouped(prev: MessageView | undefined, cur: MessageView): boolean {
  if (!prev) return false;
  if (prev.author.kind !== cur.author.kind || prev.author.user_id !== cur.author.user_id) return false;
  if (prev.state !== "normal" || cur.state !== "normal") return false;
  if (prev.reply_count > 0) return false;
  return new Date(cur.created_at).getTime() - new Date(prev.created_at).getTime() < 5 * 60_000;
}

export type MessageActions = {
  onReact: (m: MessageView, emoji: string, on: boolean) => void;
  onOpenThread?: (m: MessageView) => void;
  onEdit: (m: MessageView, blocks: MessageBlock[]) => Promise<boolean>;
  onDelete: (m: MessageView) => Promise<void>;
  onAgentReplyChanged: () => void;
};

export function MessageList({
  messages,
  members,
  canModerate,
  canReact,
  firstUnreadId,
  activeThreadId,
  compact,
  hideDayDividers,
  agentReplies,
  actions,
}: {
  /// 古い順。
  messages: MessageView[];
  members: MemberView[];
  canModerate: boolean;
  canReact: boolean;
  firstUnreadId?: string | null;
  activeThreadId?: string | null;
  compact?: boolean;
  /// スレッド内では日付の区切りを出さない（親と返信で二重に出るため）。
  hideDayDividers?: boolean;
  /// 自分だけに表示の回答（呼んだ発言の下に出す）。
  agentReplies?: AgentReplyView[];
  actions: MessageActions;
}) {
  const now = React.useMemo(() => new Date(), []);
  const byTrigger = React.useMemo(() => {
    const map = new Map<string, AgentReplyView[]>();
    for (const r of agentReplies ?? []) map.set(r.trigger_message_id, [...(map.get(r.trigger_message_id) ?? []), r]);
    return map;
  }, [agentReplies]);

  return (
    <div className="flex flex-col pb-3">
      {messages.map((m, i) => {
        const prev = messages[i - 1];
        const label = dayLabel(m.created_at, now);
        const showDay = !hideDayDividers && (!prev || dayLabel(prev.created_at, now) !== label);
        const replies = byTrigger.get(m.id);
        return (
          <React.Fragment key={m.id}>
            {showDay ? <DayDivider label={label} /> : null}
            {/* スレッド展開中の行は塗りで示す（設計言語: 選択は bg-accent の塗り・枠は使わない）。 */}
            <div className={activeThreadId === m.id ? "bg-accent/60" : undefined}>
              <MessageRow
                message={m}
                members={members}
                canModerate={canModerate}
                canReact={canReact}
                grouped={!showDay && m.id !== firstUnreadId && isGrouped(prev, m)}
                unreadBoundary={m.id === firstUnreadId}
                compact={compact}
                onReact={(e, on) => actions.onReact(m, e, on)}
                onOpenThread={actions.onOpenThread ? () => actions.onOpenThread?.(m) : undefined}
                onEdit={(blocks) => actions.onEdit(m, blocks)}
                onDelete={() => actions.onDelete(m)}
              >
                {replies?.map((r) => <AgentReplyCard key={r.id} reply={r} onChanged={actions.onAgentReplyChanged} />)}
              </MessageRow>
            </div>
          </React.Fragment>
        );
      })}
    </div>
  );
}
