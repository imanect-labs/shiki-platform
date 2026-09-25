"use client";

/// 右ペイン：スレッド（親発言＋返信）。シキ（AI）の回答も返信としてこの列に積む。

import * as React from "react";
import { X } from "lucide-react";

import { channelTitle, type Channel, type Message, type MessageBlock } from "@/lib/messages-mock";
import { ChannelIcon } from "./primitives";
import { MessageList } from "./message-list";
import { Composer } from "./composer";

export function ThreadPane({
  channel,
  root,
  viewerId,
  onClose,
  onReply,
  onToggleReaction,
}: {
  channel: Channel;
  root: Message;
  viewerId: string;
  onClose: () => void;
  onReply: (blocks: MessageBlock[]) => void;
  onToggleReaction: (messageId: string, emoji: string) => void;
}) {
  const scroller = React.useRef<HTMLDivElement>(null);

  // 返信が増えたら最下部へ寄せる（自分の返信・AI の回答がすぐ見えるように）。
  // AI 回答は 1 文字ずつ伸びるので、末尾の本文長も依存に入れて追従させる。
  const replyCount = root.replies.length;
  const tailLength = React.useMemo(
    () => root.replies[replyCount - 1]?.blocks.reduce(
      (n, b) => n + (b.type === "text" ? b.text.length : 1),
      0,
    ) ?? 0,
    [root.replies, replyCount],
  );
  React.useEffect(() => {
    const el = scroller.current;
    if (!el) return;
    // 利用者が上へ遡って読んでいる間は追わない。生成中の AI 回答は 1 文字ごとに
    // ここを通るので、無条件に最下部へ寄せると読んでいる手を奪う。
    const nearBottom = el.scrollHeight - el.scrollTop - el.clientHeight < 120;
    if (nearBottom) el.scrollTop = el.scrollHeight;
  }, [replyCount, tailLength]);

  return (
    <aside className="flex h-full w-[380px] shrink-0 flex-col border-l border-border bg-background">
      <header className="shiki-dash-bottom flex h-14 shrink-0 items-center gap-2 px-4">
        <div className="min-w-0 flex-1">
          <p className="text-[14px] font-semibold text-foreground">スレッド</p>
          <p className="flex items-center gap-1 truncate text-[11.5px] text-muted-foreground">
            <ChannelIcon
              kind={channel.kind}
              memberCount={channel.memberIds.length}
              className="size-3"
            />
            {channelTitle(channel, viewerId)}
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
        <MessageList
          messages={[root]}
          viewerId={viewerId}
          compact
          hideDayDividers
          onToggleReaction={onToggleReaction}
        />
        {root.replies.length > 0 ? (
          <div className="flex items-center gap-2 px-5 py-1">
            <span className="text-[11.5px] font-medium text-muted-foreground">
              返信 {root.replies.length} 件
            </span>
            <span className="shiki-dash-x h-px flex-1" aria-hidden />
          </div>
        ) : null}
        <MessageList
          messages={root.replies}
          viewerId={viewerId}
          compact
          hideDayDividers
          onToggleReaction={onToggleReaction}
        />
      </div>

      {/* スレッドごとに作り直す（書きかけの返信を別スレッドへ持ち越さない）。 */}
      <Composer
        key={`${root.id}:${viewerId}`}
        placeholder="スレッドに返信…"
        channelMemberIds={channel.memberIds}
        viewerId={viewerId}
        onSend={onReply}
        hideHint
      />
    </aside>
  );
}
