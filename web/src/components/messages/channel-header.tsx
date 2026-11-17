"use client";

/// 本体ペインの上端バー。チャンネル名・トピック・参加者・通知と設定の入口。

import * as React from "react";
import { Archive, Bell, BellOff, EyeOff, MoreHorizontal, Settings2, Users } from "lucide-react";

import {
  DropdownMenu,
  DropdownMenuContent,
  DropdownMenuItem,
  DropdownMenuSeparator,
  DropdownMenuTrigger,
} from "@/components/ui/dropdown-menu";
import { Tooltip, TooltipContent, TooltipTrigger } from "@/components/ui/tooltip";
import type { ChannelView, MemberView } from "@/lib/messaging-api";
import { AgentAvatar, ChannelTile, UserAvatar, useMessagingDisplay } from "./primitives";
import type { SettingsTab } from "./channel-settings-dialog";

const ICON_BTN =
  "flex size-8 shrink-0 items-center justify-center rounded-lg text-muted-foreground outline-none transition-colors hover:bg-accent hover:text-foreground focus-visible:ring-2 focus-visible:ring-ring";

export function ChannelHeader({
  channel,
  members = [],
  leading,
  onOpenSettings,
  onHideDm,
}: {
  channel: ChannelView;
  /// 参加者（先頭の数人の顔を参加者ボタンに並べる）。
  members?: MemberView[];
  /// 狭い画面の「一覧を開く」ボタン。
  leading?: React.ReactNode;
  onOpenSettings: (tab: SettingsTab) => void;
  onHideDm: () => void;
}) {
  const { me, agentHasIcon } = useMessagingDisplay();
  const isDm = channel.kind === "dm";
  const NotifyIcon = channel.notify === "none" ? BellOff : Bell;
  const notifyLabel = channel.notify === "all" ? "すべて通知" : channel.notify === "mentions" ? "メンションだけ通知" : "通知なし";

  return (
    <header className="shiki-dash-bottom flex h-14 shrink-0 items-center gap-2 bg-background/80 pl-2 pr-3 backdrop-blur lg:pl-5">
      {leading}
      <div className="flex min-w-0 flex-1 items-center gap-2">
        {isDm ? (
          channel.agent_dm ? (
            <AgentAvatar ownerId={me} hasIcon={agentHasIcon} size="sm" />
          ) : (
            <UserAvatar userId={channel.dm_peer ?? channel.id} name={channel.name} size="sm" />
          )
        ) : (
          <ChannelTile channelId={channel.id} kind={channel.kind} announcement={channel.announcement} size="sm" />
        )}
        <h1 className="min-w-0 truncate text-[15.5px] font-semibold tracking-tight text-foreground">{channel.name}</h1>
        {channel.kind === "private" ? (
          <span className="shrink-0 rounded-full bg-muted px-1.5 py-px text-[10.5px] font-medium leading-4 text-muted-foreground">
            非公開
          </span>
        ) : null}
        {channel.announcement ? (
          <span className="shrink-0 rounded-full bg-muted px-1.5 py-px text-[10.5px] font-medium leading-4 text-muted-foreground">
            アナウンス
          </span>
        ) : null}
        {channel.archived ? (
          <span className="flex shrink-0 items-center gap-0.5 rounded-full bg-muted px-1.5 py-px text-[10.5px] font-medium leading-4 text-muted-foreground">
            <Archive className="size-3" aria-hidden />
            アーカイブ
          </span>
        ) : null}
        {/* トピックは幅に余裕がある時だけ出す。狭いと無意味な省略になる。 */}
        {channel.topic ? (
          <>
            <span className="hidden h-3.5 w-px shrink-0 bg-border xl:block" aria-hidden />
            <p className="hidden min-w-0 flex-1 basis-0 truncate text-[12px] text-muted-foreground xl:block" title={channel.topic}>
              {channel.topic}
            </p>
          </>
        ) : null}
      </div>

      {!isDm ? (
        <button
          type="button"
          onClick={() => onOpenSettings("members")}
          aria-label={`参加者 ${channel.member_count} 名`}
          className="flex h-8 shrink-0 items-center gap-1.5 rounded-full border border-border/60 bg-card/60 pl-1 pr-2.5 text-[12px] font-medium text-foreground/80 outline-none transition-colors hover:bg-accent focus-visible:ring-2 focus-visible:ring-ring"
        >
          {/* 顔ぶれが分かるよう先頭の 3 人を重ねて並べる（読み込み前は人のアイコン）。 */}
          {members.length > 0 ? (
            <span className="flex -space-x-1.5" aria-hidden>
              {members
                .filter((m) => m.active)
                .slice(0, 3)
                .map((m) => (
                  <UserAvatar
                    key={m.user_id}
                    userId={m.user_id}
                    name={m.display_name}
                    size="xs"
                    className="rounded-full ring-2 ring-background [&>*]:rounded-full"
                  />
                ))}
            </span>
          ) : (
            <Users className="ml-1 size-3.5 text-muted-foreground" aria-hidden />
          )}
          <span className="tabular-nums">{channel.member_count}</span>
        </button>
      ) : null}

      {channel.joined ? (
        <Tooltip>
          <TooltipTrigger asChild>
            <button type="button" onClick={() => onOpenSettings("notify")} aria-label={`通知: ${notifyLabel}`} className={ICON_BTN}>
              <NotifyIcon className="size-4" aria-hidden />
            </button>
          </TooltipTrigger>
          <TooltipContent side="bottom">{notifyLabel}</TooltipContent>
        </Tooltip>
      ) : null}

      <DropdownMenu>
        <DropdownMenuTrigger asChild>
          <button type="button" aria-label="チャンネルの操作" className={ICON_BTN}>
            <MoreHorizontal className="size-4" aria-hidden />
          </button>
        </DropdownMenuTrigger>
        <DropdownMenuContent align="end" className="w-[220px]">
          {!isDm ? (
            <DropdownMenuItem onSelect={() => onOpenSettings("about")} className="gap-2">
              <Settings2 className="size-4" aria-hidden />
              チャンネルの設定
            </DropdownMenuItem>
          ) : null}
          {!isDm ? (
            <DropdownMenuItem onSelect={() => onOpenSettings("members")} className="gap-2">
              <Users className="size-4" aria-hidden />
              参加者
            </DropdownMenuItem>
          ) : null}
          <DropdownMenuItem onSelect={() => onOpenSettings("notify")} className="gap-2">
            <Bell className="size-4" aria-hidden />
            通知の受け方
          </DropdownMenuItem>
          {isDm ? (
            <>
              <DropdownMenuSeparator />
              <DropdownMenuItem onSelect={onHideDm} className="gap-2">
                <EyeOff className="size-4" aria-hidden />
                一覧から隠す
              </DropdownMenuItem>
            </>
          ) : null}
        </DropdownMenuContent>
      </DropdownMenu>
    </header>
  );
}
