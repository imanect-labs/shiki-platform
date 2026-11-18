"use client";

/// 本体ペインの上端バー。チャンネル名・トピック・資料／ピン／参加者の入口・通知と設定の入口。

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
import type { ChannelView } from "@/lib/messaging-api";
import { AgentAvatar, ChannelTile, UserAvatar, useMessagingDisplay } from "./primitives";
import type { SettingsTab } from "./channel-settings-dialog";

const ICON_BTN =
  "flex size-8 shrink-0 items-center justify-center rounded-lg text-muted-foreground outline-none transition-colors hover:bg-accent hover:text-foreground focus-visible:ring-2 focus-visible:ring-ring";

export function ChannelHeader({
  channel,
  leading,
  pills,
  onOpenSettings,
  onHideDm,
}: {
  channel: ChannelView;
  /// 狭い画面の「一覧を開く」ボタン。
  leading?: React.ReactNode;
  /// 資料・ピン・参加者の入口（右の一覧を開く）。
  pills?: React.ReactNode;
  onOpenSettings: (tab: SettingsTab) => void;
  onHideDm: () => void;
}) {
  const { me, agentHasIcon } = useMessagingDisplay();
  const isDm = channel.kind === "dm";
  const NotifyIcon = channel.notify === "none" ? BellOff : Bell;
  const notifyLabel = channel.notify === "all" ? "すべて通知" : channel.notify === "mentions" ? "メンションだけ通知" : "通知なし";

  return (
    <header className="border-b border-border/70 flex h-[60px] shrink-0 items-center gap-2 bg-background/80 pl-2 pr-3 backdrop-blur lg:pl-6">
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
        <h1 className="min-w-0 truncate text-[18px] font-bold tracking-tight text-foreground">{channel.name}</h1>
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
            <p className="hidden min-w-0 flex-1 basis-0 truncate text-[13px] text-muted-foreground xl:block" title={channel.topic}>
              {channel.topic}
            </p>
          </>
        ) : null}
      </div>

      {pills}

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
