"use client";

/// 本体ペインの上端バー。チャンネル名・トピック・参加者と、デモ用の「表示中の利用者」切替。
///
/// 表示中の利用者を切り替えられるのはこのモック限定の仕掛けで、同じ発言が
/// **見る人の権限によって違って見える**（file_ref・検索範囲）ことを実演するために置いている。

import * as React from "react";
import { Check, ChevronDown, Eye, UserPlus } from "lucide-react";

import { cn } from "@/lib/utils";
import {
  DropdownMenu,
  DropdownMenuContent,
  DropdownMenuItem,
  DropdownMenuLabel,
  DropdownMenuSeparator,
  DropdownMenuTrigger,
} from "@/components/ui/dropdown-menu";
import { Tooltip, TooltipContent, TooltipTrigger } from "@/components/ui/tooltip";
import { VIEWER_CHOICES, channelTitle, findMember, type Channel } from "@/lib/messages-mock";
import { ChannelIcon, MemberAvatar } from "./primitives";

export function ChannelHeader({
  channel,
  viewerId,
  onViewerChange,
  onInvite,
}: {
  channel: Channel;
  viewerId: string;
  onViewerChange: (id: string) => void;
  onInvite: () => void;
}) {
  const viewer = findMember(viewerId);
  const isDm = channel.kind === "dm";
  const shown = channel.memberIds.slice(0, 4);

  return (
    <header className="shiki-dash-bottom flex h-14 shrink-0 items-center gap-3 bg-background/80 px-5 backdrop-blur">
      <div className="flex min-w-0 flex-1 items-center gap-2">
        {isDm && channel.memberIds.length === 2 ? (
          <MemberAvatar
            memberId={channel.memberIds.find((m) => m !== viewerId) ?? viewerId}
            size="sm"
            showPresence
          />
        ) : (
          <ChannelIcon
            kind={channel.kind}
            memberCount={channel.memberIds.length}
            className="shrink-0 text-muted-foreground"
          />
        )}
        <h1 className="shrink-0 truncate text-[15px] font-semibold text-foreground">
          {channelTitle(channel, viewerId)}
        </h1>
        {channel.kind === "private" ? (
          <span className="shrink-0 rounded-full bg-muted px-1.5 py-px text-[10.5px] font-medium leading-4 text-muted-foreground">
            非公開
          </span>
        ) : null}
        {/* トピックは幅に余裕がある時だけ出す。狭いと「2...」のような無意味な省略になる。 */}
        {channel.topic ? (
          <>
            <span className="hidden h-3.5 w-px shrink-0 bg-border xl:block" aria-hidden />
            <p className="hidden min-w-0 truncate text-[12px] text-muted-foreground xl:block">
              {channel.topic}
            </p>
          </>
        ) : null}
      </div>

      {/* 参加者（重ねアバター）＋招待。 */}
      <button
        type="button"
        onClick={onInvite}
        className="flex shrink-0 items-center gap-1.5 rounded-lg border border-border/60 bg-card/40 py-1 pl-1.5 pr-2 outline-none transition-colors hover:bg-accent focus-visible:ring-2 focus-visible:ring-ring"
      >
        <span className="flex -space-x-1.5">
          {shown.map((id) => (
            <MemberAvatar
              key={id}
              memberId={id}
              size="sm"
              className="rounded-[8px] ring-2 ring-background"
            />
          ))}
        </span>
        <span className="text-[12px] font-medium text-foreground/80">
          {channel.memberIds.length}
        </span>
        {!isDm ? <UserPlus className="size-3.5 text-muted-foreground" aria-hidden /> : null}
      </button>

      {/* デモ用: 表示中の利用者を切り替える。 */}
      <DropdownMenu>
        <Tooltip>
          <TooltipTrigger asChild>
            <DropdownMenuTrigger asChild>
              <button
                type="button"
                aria-label="表示中の利用者を切り替える"
                className="flex h-8 shrink-0 items-center gap-1.5 rounded-lg border border-dashed border-border px-2 text-[12px] outline-none transition-colors hover:bg-accent focus-visible:ring-2 focus-visible:ring-ring"
              >
                <Eye className="size-3.5 text-muted-foreground" aria-hidden />
                <span className="font-medium text-foreground/80">{viewer.name}</span>
                <ChevronDown className="size-3.5 text-muted-foreground" aria-hidden />
              </button>
            </DropdownMenuTrigger>
          </TooltipTrigger>
          <TooltipContent side="bottom">
            この画面を誰として見るかを切り替えます（デモ用）
          </TooltipContent>
        </Tooltip>
        <DropdownMenuContent align="end" className="w-[260px]">
          <DropdownMenuLabel>表示中の利用者</DropdownMenuLabel>
          {VIEWER_CHOICES.map((id) => {
            const m = findMember(id);
            return (
              <DropdownMenuItem key={id} onSelect={() => onViewerChange(id)} className="gap-2">
                <MemberAvatar memberId={id} size="sm" />
                <span className="min-w-0 flex-1">
                  <span className="block truncate text-[13px] font-medium">{m.name}</span>
                  <span className="block truncate text-[11px] text-muted-foreground">{m.dept}</span>
                </span>
                <Check
                  className={cn("size-4 text-primary", id !== viewerId && "invisible")}
                  aria-hidden
                />
              </DropdownMenuItem>
            );
          })}
          <DropdownMenuSeparator />
          <p className="px-2 py-1.5 text-[11px] leading-snug text-muted-foreground">
            同じチャンネルでも、共有された文書・検索できる発言は
            見る人の権限によって変わります。
          </p>
        </DropdownMenuContent>
      </DropdownMenu>
    </header>
  );
}
