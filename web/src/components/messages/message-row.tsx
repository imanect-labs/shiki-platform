"use client";

/// 1 発言の行。ホバーで操作（リアクション・スレッド返信・シキに聞く）が浮く。

import * as React from "react";
import { MessageSquareReply, Smile, Sparkles } from "lucide-react";

import { cn } from "@/lib/utils";
import { currentSeasonIndex, seasonVar } from "@/lib/season";
import { Tooltip, TooltipContent, TooltipTrigger } from "@/components/ui/tooltip";
import { Popover, PopoverContent, PopoverTrigger } from "@/components/ui/popover";
import { findMember, type Message } from "@/lib/messages-mock";
import { Blocks, MemberAvatar } from "./primitives";

const QUICK_EMOJI = ["👍", "🙏", "✅", "🎉", "👀", "🔥"];

/// AI の発言のアイコン。プライマリ（Deep Navy）を地に、今季の差し色をわずかに混ぜる。
/// 四季 4 色を全部つなぐと中間がにごるので、2 段の近い色に留める。
function shikiGradient(): string {
  const season = seasonVar(currentSeasonIndex());
  return `linear-gradient(140deg, var(--primary), color-mix(in oklab, var(--primary) 68%, ${season}))`;
}

export function ReactionPills({
  message,
  viewerId,
  onToggle,
}: {
  message: Message;
  viewerId: string;
  onToggle: (emoji: string) => void;
}) {
  if (message.reactions.length === 0) return null;
  return (
    <div className="mt-1.5 flex flex-wrap items-center gap-1">
      {message.reactions.map((r) => {
        const mine = r.by.includes(viewerId);
        return (
          <Tooltip key={r.emoji}>
            <TooltipTrigger asChild>
              <button
                type="button"
                onClick={() => onToggle(r.emoji)}
                className={cn(
                  "flex h-6 items-center gap-1 rounded-full border px-2 text-[11.5px] leading-none outline-none transition-colors focus-visible:ring-2 focus-visible:ring-ring",
                  mine
                    ? "border-transparent bg-accent font-semibold text-foreground"
                    : "border-border/60 bg-card/40 text-muted-foreground hover:bg-accent",
                )}
              >
                <span className="text-[12.5px]">{r.emoji}</span>
                {r.by.length}
              </button>
            </TooltipTrigger>
            <TooltipContent side="top">
              {r.by.map((id) => findMember(id).name).join("、")}
            </TooltipContent>
          </Tooltip>
        );
      })}
    </div>
  );
}

function HoverToolbar({
  onReact,
  onReply,
  onAskAi,
}: {
  onReact: (emoji: string) => void;
  onReply?: () => void;
  onAskAi?: () => void;
}) {
  const [pickerOpen, setPickerOpen] = React.useState(false);
  return (
    <div
      className={cn(
        "absolute -top-3 right-3 z-10 flex items-center gap-0.5 rounded-[9px] border border-border/70 bg-popover p-0.5 shadow-sm",
        // 見えていない間はクリックを奪わない（上の行の右上に不可視の的が重なるのを防ぐ）。
        "pointer-events-none opacity-0 transition-opacity duration-[var(--duration-fast)] ease-[var(--ease-standard)]",
        "group-hover/msg:pointer-events-auto group-hover/msg:opacity-100 focus-within:pointer-events-auto focus-within:opacity-100",
        pickerOpen && "pointer-events-auto opacity-100",
      )}
    >
      <Popover open={pickerOpen} onOpenChange={setPickerOpen}>
        <PopoverTrigger asChild>
          <button
            type="button"
            aria-label="リアクションを付ける"
            className="flex size-7 items-center justify-center rounded-[7px] text-muted-foreground outline-none transition-colors hover:bg-accent hover:text-foreground focus-visible:ring-2 focus-visible:ring-ring"
          >
            <Smile className="size-4" aria-hidden />
          </button>
        </PopoverTrigger>
        <PopoverContent align="end" className="w-auto p-1">
          <div className="flex gap-0.5">
            {QUICK_EMOJI.map((e) => (
              <button
                key={e}
                type="button"
                onClick={() => {
                  onReact(e);
                  setPickerOpen(false);
                }}
                className="flex size-8 items-center justify-center rounded-md text-[16px] outline-none transition-colors hover:bg-accent focus-visible:ring-2 focus-visible:ring-ring"
              >
                {e}
              </button>
            ))}
          </div>
        </PopoverContent>
      </Popover>

      {onReply ? (
        <Tooltip>
          <TooltipTrigger asChild>
            <button
              type="button"
              onClick={onReply}
              aria-label="スレッドで返信"
              className="flex size-7 items-center justify-center rounded-[7px] text-muted-foreground outline-none transition-colors hover:bg-accent hover:text-foreground focus-visible:ring-2 focus-visible:ring-ring"
            >
              <MessageSquareReply className="size-4" aria-hidden />
            </button>
          </TooltipTrigger>
          <TooltipContent side="top">スレッドで返信</TooltipContent>
        </Tooltip>
      ) : null}

      {onAskAi ? (
        <Tooltip>
          <TooltipTrigger asChild>
            <button
              type="button"
              onClick={onAskAi}
              aria-label="シキに聞く"
              className="flex h-7 items-center gap-1 rounded-[7px] px-1.5 text-[11.5px] font-medium text-muted-foreground outline-none transition-colors hover:bg-accent hover:text-foreground focus-visible:ring-2 focus-visible:ring-ring"
            >
              <Sparkles
                className="size-4"
                style={{ color: seasonVar(0) }}
                aria-hidden
              />
              シキに聞く
            </button>
          </TooltipTrigger>
          <TooltipContent side="top">この発言の文脈でシキに聞く</TooltipContent>
        </Tooltip>
      ) : null}
    </div>
  );
}

export function MessageRow({
  message,
  viewerId,
  grouped,
  unreadBoundary,
  compact,
  onToggleReaction,
  onOpenThread,
  onAskAi,
}: {
  message: Message;
  viewerId: string;
  /// 直前の発言と同じ人・同じ時間帯なら名前とアバターを省く（Slack 風の連続表示）。
  grouped: boolean;
  unreadBoundary?: boolean;
  /// スレッドペイン内の表示（返信数サマリと AI ボタンを出さない）。
  compact?: boolean;
  onToggleReaction: (emoji: string) => void;
  onOpenThread?: () => void;
  onAskAi?: () => void;
}) {
  const author = findMember(message.authorId);
  const replyCount = message.replies.length;

  return (
    <>
      {unreadBoundary ? (
        <div className="relative my-2 flex items-center gap-2 px-5">
          <span className="h-px flex-1 bg-[color-mix(in_oklab,var(--season-spring)_55%,transparent)]" />
          <span
            className="rounded-full px-2 py-0.5 text-[10.5px] font-semibold leading-none"
            style={{
              backgroundColor: `color-mix(in oklab, ${seasonVar(0)} 18%, transparent)`,
              color: seasonVar(0),
            }}
          >
            ここから未読
          </span>
        </div>
      ) : null}

      <div
        className={cn(
          "group/msg relative px-5 transition-colors hover:bg-accent/35",
          grouped ? "py-0.5" : "pb-0.5 pt-2.5",
        )}
      >
        <HoverToolbar
          onReact={onToggleReaction}
          onReply={compact ? undefined : onOpenThread}
          onAskAi={compact ? undefined : onAskAi}
        />
        <div className="flex gap-2.5">
          <div className="w-8 shrink-0 pt-0.5">
            {grouped ? (
              <span className="hidden select-none pt-1 text-[10.5px] leading-none text-muted-foreground/70 group-hover/msg:block">
                {message.time}
              </span>
            ) : message.ai ? (
              // プライマリ地＋今季の差し色。アイコンはプライマリの前景で抜く。
              <span
                className="flex size-8 items-center justify-center rounded-[10px] text-primary-foreground shadow-xs"
                style={{ backgroundImage: shikiGradient() }}
              >
                <Sparkles className="size-[17px]" strokeWidth={2.25} aria-hidden />
              </span>
            ) : (
              <MemberAvatar memberId={message.authorId} />
            )}
          </div>

          <div className="min-w-0 flex-1">
            {!grouped ? (
              <div className="flex items-baseline gap-2">
                <span className="text-[13.5px] font-semibold text-foreground">{author.name}</span>
                {message.ai ? (
                  <span className="rounded-full border border-border/60 px-1.5 py-px text-[9.5px] font-semibold uppercase leading-4 tracking-wide text-muted-foreground">
                    AI
                  </span>
                ) : null}
                <span className="text-[11px] text-muted-foreground">{message.time}</span>
              </div>
            ) : null}

            <Blocks blocks={message.blocks} viewerId={viewerId} />
            {message.pending ? (
              <span className="ml-0.5 inline-block h-3.5 w-[2px] translate-y-0.5 animate-pulse bg-foreground/70" />
            ) : null}
            {message.edited ? (
              <span className="ml-1 text-[11px] text-muted-foreground">（編集済み）</span>
            ) : null}

            {/* AI 回答が参照した文脈。照会者の権限で読み直した発言・文書だけが並ぶ（PIT-63）。 */}
            {message.sources && message.sources.length > 0 ? (
              <div className="mt-2 rounded-lg border border-border/60 bg-card/40 px-3 py-2">
                <p className="text-[10.5px] font-semibold uppercase tracking-wide text-muted-foreground">
                  参照した文脈
                </p>
                <ul className="mt-1 flex flex-col gap-0.5">
                  {message.sources.map((s) => (
                    <li key={s} className="text-[11.5px] leading-snug text-muted-foreground">
                      ・{s}
                    </li>
                  ))}
                </ul>
                <p className="mt-1.5 text-[11px] leading-snug text-muted-foreground/80">
                  あなたが読める発言と文書だけを参照しています。
                </p>
              </div>
            ) : null}

            <ReactionPills message={message} viewerId={viewerId} onToggle={onToggleReaction} />

            {!compact && replyCount > 0 ? (
              <button
                type="button"
                onClick={onOpenThread}
                className="mt-1.5 flex items-center gap-2 rounded-md py-1 pr-2 text-left outline-none transition-colors hover:bg-accent focus-visible:ring-2 focus-visible:ring-ring"
              >
                <span className="flex -space-x-1">
                  {Array.from(new Set(message.replies.map((r) => r.authorId)))
                    .slice(0, 3)
                    .map((id) => (
                      <MemberAvatar
                        key={id}
                        memberId={id}
                        size="xs"
                        className="rounded-[6px] ring-2 ring-background"
                      />
                    ))}
                </span>
                <span className="text-[12px] font-medium text-primary">返信 {replyCount} 件</span>
                <span className="text-[11.5px] text-muted-foreground">
                  最終 {message.replies[replyCount - 1]?.time}
                </span>
              </button>
            ) : null}
          </div>
        </div>
      </div>
    </>
  );
}
