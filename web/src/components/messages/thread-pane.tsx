"use client";

/// 右ペイン：スレッド（親発言＋返信）。返信は配信（SSE）で増える。

import * as React from "react";
import { Loader2, X } from "lucide-react";

import type { AgentReplyView, ChannelView, MemberView, MessageBlock, MessageView } from "@/lib/messaging-api";
import { ChannelIcon } from "./primitives";
import { MessageList, type MessageActions } from "./message-list";
import { Composer } from "./composer";

export function ThreadPane({
  channel,
  threadId,
  items,
  loading,
  error,
  members,
  agentReplies,
  composerDisabledReason,
  actions,
  onClose,
  onReply,
}: {
  channel: ChannelView;
  /// 親の発言の id（入力欄をスレッドごとに作り直す）。
  threadId: string;
  /// 親＋返信（古い順）。読み込み中は空。
  items: MessageView[];
  loading: boolean;
  error: string | null;
  members: MemberView[];
  agentReplies: AgentReplyView[];
  composerDisabledReason: string | null;
  actions: MessageActions;
  onClose: () => void;
  onReply: (blocks: MessageBlock[], opts: { agentPrivate: boolean }) => Promise<boolean>;
}) {
  const scroller = React.useRef<HTMLDivElement>(null);
  const [root, ...replies] = items;
  const replyCount = replies.length;

  // 返信が増えたら最下部へ寄せる。上へ遡って読んでいる間は追わない。
  React.useEffect(() => {
    const el = scroller.current;
    if (!el) return;
    const nearBottom = el.scrollHeight - el.scrollTop - el.clientHeight < 160;
    if (nearBottom) el.scrollTop = el.scrollHeight;
  }, [replyCount, agentReplies.length]);

  React.useEffect(() => {
    const onKey = (e: KeyboardEvent) => {
      if (e.key === "Escape" && !e.defaultPrevented && document.activeElement?.tagName !== "TEXTAREA") onClose();
    };
    window.addEventListener("keydown", onKey);
    return () => window.removeEventListener("keydown", onKey);
  }, [onClose]);

  return (
    <aside
      className="flex h-full w-full shrink-0 flex-col border-l border-border bg-background sm:w-[380px] lg:w-[440px] 2xl:w-[520px]"
      aria-label="スレッド"
      data-testid="thread-pane"
    >
      <header className="shiki-dash-bottom flex h-14 shrink-0 items-center gap-2 px-4">
        <div className="min-w-0 flex-1">
          <p className="text-[14px] font-semibold text-foreground">スレッド</p>
          <p className="flex items-center gap-1 truncate text-[11.5px] text-muted-foreground">
            <ChannelIcon kind={channel.kind} announcement={channel.announcement} agentDm={channel.agent_dm} className="size-3" />
            {channel.name}
          </p>
        </div>
        <button
          type="button"
          onClick={onClose}
          aria-label="スレッドを閉じる"
          className="flex size-8 items-center justify-center rounded-lg text-muted-foreground outline-none transition-colors hover:bg-accent hover:text-foreground focus-visible:ring-2 focus-visible:ring-ring"
        >
          <X className="size-4" aria-hidden />
        </button>
      </header>

      <div ref={scroller} className="scrollbar-subtle min-h-0 flex-1 overflow-y-auto">
        {loading && !root ? (
          <div className="flex items-center justify-center gap-2 py-10 text-[12.5px] text-muted-foreground" aria-busy="true">
            <Loader2 className="size-4 animate-spin" aria-hidden />
            スレッドを読み込んでいます
          </div>
        ) : error ? (
          <p className="px-5 py-10 text-center text-[12.5px] text-destructive" role="alert">
            {error}
          </p>
        ) : root ? (
          <>
            <MessageList
              messages={[root]}
              members={members}
              canModerate={channel.is_admin}
              canReact={channel.joined && !channel.archived}
              compact
              hideDayDividers
              agentReplies={agentReplies}
              actions={actions}
            />
            <div className="flex items-center gap-2 px-5 py-1">
              <span className="text-[11.5px] font-medium text-muted-foreground">
                {replyCount > 0 ? `返信 ${replyCount} 件` : "まだ返信はありません"}
              </span>
              <span className="shiki-dash-x h-px flex-1" aria-hidden />
            </div>
            <MessageList
              messages={replies}
              members={members}
              canModerate={channel.is_admin}
              canReact={channel.joined && !channel.archived}
              compact
              hideDayDividers
              agentReplies={agentReplies}
              actions={actions}
            />
          </>
        ) : null}
      </div>

      {/* スレッドごとに作り直す（書きかけの返信を別スレッドへ持ち越さない）。 */}
      <Composer
        key={threadId}
        placeholder="スレッドに返信…"
        members={members}
        allowChannelMention={false}
        disabledReason={composerDisabledReason}
        onSubmit={onReply}
      />
    </aside>
  );
}
