"use client";

/// 3 ペインの左端：チャンネル／ダイレクトメッセージの一覧。
/// 未読は `read_state` からの導出（件数）、メンションは別の強い色のバッジで示す。
/// スター（自分だけの設定）を付けたものは「スター」に上げ、「未読」で未読・メンションのあるものだけに絞れる。

import * as React from "react";
import Link from "next/link";
import { AtSign, BellOff, ChevronDown, Compass, Hash, MessageCirclePlus, Plus, Search, ShieldCheck, Settings2, Star } from "lucide-react";

import { cn } from "@/lib/utils";
import {
  DropdownMenu,
  DropdownMenuContent,
  DropdownMenuItem,
  DropdownMenuTrigger,
} from "@/components/ui/dropdown-menu";
import { Skeleton } from "@/components/ui/skeleton";
import type { ChannelView } from "@/lib/messaging-api";
import { AgentAvatar, ChannelTile, UserAvatar, useMessagingDisplay } from "./primitives";

export type SideView = "channel" | "search" | "mentions" | "approvals";

function Badge({ count, strong, label }: { count: number; strong: boolean; label: string }) {
  return (
    <span
      className={cn(
        "flex h-5 min-w-5 shrink-0 items-center justify-center rounded-full px-1.5 text-[11px] font-bold leading-none tabular-nums",
        strong ? "bg-primary text-primary-foreground" : "bg-sidebar-foreground/12 text-sidebar-foreground",
      )}
    >
      <span className="sr-only">{label}</span>
      {count > 99 ? "99+" : count}
    </span>
  );
}

/// 一覧の 1 行。未読は太字＋件数、通知なしは薄く、選択中は塗りで示す。DM は最新の一言を 2 行目に出す。
/// 右端のスターは行にマウスを乗せた（またはフォーカスした）ときと、付けている行だけに出す。
function Row({
  channel,
  active,
  onSelect,
  onToggleStar,
}: {
  channel: ChannelView;
  active: boolean;
  onSelect: () => void;
  onToggleStar?: (starred: boolean) => void;
}) {
  const { me, agentHasIcon } = useMessagingDisplay();
  const muted = channel.notify === "none";
  const unread = channel.notify === "all" ? channel.unread : 0;
  const hot = unread > 0 || channel.mentions > 0;
  const isDm = channel.kind === "dm";
  const excerpt = isDm ? channel.last_excerpt : undefined;
  return (
    <div className="group/ch relative">
      <button
        type="button"
        onClick={onSelect}
        aria-current={active ? "true" : undefined}
        data-testid="channel-row"
        className={cn(
          "flex w-full items-center gap-2.5 rounded-[9px] px-2.5 text-left outline-none transition-colors",
          excerpt ? "min-h-12 py-1.5" : "h-9",
          "focus-visible:ring-2 focus-visible:ring-sidebar-ring",
          active
            ? "bg-primary text-primary-foreground"
            : hot
              ? "font-semibold text-sidebar-foreground hover:bg-sidebar-accent/70"
              : "text-sidebar-foreground/70 hover:bg-sidebar-accent/70 hover:text-sidebar-foreground",
          muted && !active && !hot && "text-sidebar-foreground/40",
        )}
      >
        {isDm ? (
          <span aria-hidden>
            {channel.agent_dm ? (
              <AgentAvatar ownerId={me} hasIcon={agentHasIcon} size="sm" className="size-6 rounded-[7px]" />
            ) : (
              <UserAvatar userId={channel.dm_peer ?? channel.id} name={channel.name} size="sm" className="size-6 rounded-[7px]" />
            )}
          </span>
        ) : (
          <ChannelTile
            channelId={channel.id}
            kind={channel.kind}
            announcement={channel.announcement}
            size="xs"
            className={cn(active ? "text-primary-foreground/80" : hot ? "text-sidebar-foreground" : "")}
          />
        )}
        <span className="min-w-0 flex-1">
          <span className="block truncate text-[14px]">{channel.name}</span>
          {excerpt ? (
            <span
              className={cn(
                "block truncate text-[12px] font-normal",
                active ? "text-primary-foreground/70" : "text-sidebar-foreground/50",
              )}
            >
              {excerpt}
            </span>
          ) : null}
        </span>
        {muted && !active ? <BellOff className="size-3.5 shrink-0 opacity-60" aria-label="通知なし" /> : null}
        {channel.mentions > 0 ? (
          <Badge count={channel.mentions} strong label="メンション" />
        ) : unread > 0 ? (
          <Badge count={unread} strong={false} label="未読" />
        ) : null}
        {/* スターの的のぶん、右側を空けておく（重ならないように）。 */}
        {onToggleStar ? <span className="w-5 shrink-0" aria-hidden /> : null}
      </button>
      {onToggleStar ? (
        <button
          type="button"
          onClick={() => onToggleStar(!channel.starred)}
          aria-label={channel.starred ? `${channel.name} のスターを外す` : `${channel.name} にスターを付ける`}
          aria-pressed={channel.starred}
          className={cn(
            "absolute right-1.5 top-1/2 flex size-6 -translate-y-1/2 items-center justify-center rounded-md outline-none transition-opacity focus-visible:opacity-100 focus-visible:ring-2 focus-visible:ring-sidebar-ring pointer-coarse:opacity-100",
            channel.starred ? "opacity-100" : "opacity-0 group-hover/ch:opacity-100",
            active ? "text-primary-foreground/80 hover:text-primary-foreground" : "text-sidebar-foreground/45 hover:text-sidebar-foreground",
          )}
        >
          <Star className={cn("size-3.5", channel.starred && "fill-current")} aria-hidden />
        </button>
      ) : null}
    </div>
  );
}

function Group({
  label,
  count,
  children,
  action,
}: {
  label: string;
  count?: number;
  children: React.ReactNode;
  action?: React.ReactNode;
}) {
  const [open, setOpen] = React.useState(true);
  return (
    <div className="mt-3 first:mt-1">
      <div className="flex items-center gap-1 px-2">
        <button
          type="button"
          onClick={() => setOpen((v) => !v)}
          aria-expanded={open}
          className="flex h-7 flex-1 items-center gap-1 rounded-[7px] px-1 text-left text-[11.5px] font-bold tracking-[0.06em] text-sidebar-foreground/50 outline-none transition-colors hover:text-sidebar-foreground/80 focus-visible:ring-2 focus-visible:ring-sidebar-ring"
        >
          <ChevronDown
            className={cn(
              "size-3.5 transition-transform duration-[var(--duration-fast)] ease-[var(--ease-standard)]",
              !open && "-rotate-90",
            )}
            aria-hidden
          />
          <span className="flex-1">{label}</span>
          {count != null ? <span className="font-medium tracking-normal tabular-nums">{count}</span> : null}
        </button>
        {action}
      </div>
      {open ? <div className="flex flex-col gap-0.5 px-2 pb-1">{children}</div> : null}
    </div>
  );
}

const SIDE_BTN =
  "flex h-8 w-full items-center gap-2 rounded-[8px] px-2 text-left text-[13px] outline-none transition-colors focus-visible:ring-2 focus-visible:ring-sidebar-ring";

export function ChannelListPane({
  channels,
  activeId,
  view,
  drawer,
  unreadMentions,
  canSeeApprovals,
  onSelect,
  onView,
  onCreateChannel,
  onBrowse,
  onNewDm,
  onToggleStar,
}: {
  /// 読み込み中は null。
  channels: ChannelView[] | null;
  activeId: string | null;
  view: SideView;
  /// ドロワ（狭幅）として描くか。Sheet 自身の閉じるボタンが右上に重なるので見出し行の右側を空ける。
  drawer?: boolean;
  unreadMentions: number;
  /// 閲覧の申請（承認者・監査担当者）を見られるか。
  canSeeApprovals: boolean;
  onSelect: (id: string) => void;
  onView: (v: SideView) => void;
  onCreateChannel: () => void;
  onBrowse: () => void;
  onNewDm: () => void;
  /// スター（一覧の上に固定）を付ける・外す。
  onToggleStar: (id: string, starred: boolean) => void;
}) {
  // 「未読」は未読かメンションのあるものだけに絞る（どこから読むかを先に見せる）。
  const [filter, setFilter] = React.useState<"all" | "unread">("all");
  const isHot = (c: ChannelView) => (c.notify === "all" && c.unread > 0) || c.mentions > 0;
  const all = (channels ?? []).filter((c) => filter === "all" || isHot(c) || c.id === activeId);
  const hotCount = (channels ?? []).filter(isHot).length;
  const starred = all.filter((c) => c.starred && !c.archived);
  const rooms = all.filter((c) => c.kind !== "dm" && !c.archived && !c.starred);
  const dms = all.filter((c) => c.kind === "dm" && !c.starred);
  const archived = all.filter((c) => c.kind !== "dm" && c.archived);
  const row = (c: ChannelView) => (
    <Row
      key={c.id}
      channel={c}
      active={view === "channel" && c.id === activeId}
      onSelect={() => onSelect(c.id)}
      onToggleStar={c.archived ? undefined : (on) => onToggleStar(c.id, on)}
    />
  );

  return (
    <aside
      className={cn(
        "flex h-full shrink-0 flex-col border-r border-sidebar-border bg-sidebar",
        drawer ? "w-full" : "w-[272px]",
      )}
      aria-label="チャンネル一覧"
    >
      <div className={cn("flex h-[60px] shrink-0 items-center gap-1 px-3", drawer && "pr-11")}>
        <h2 className="truncate text-[16px] font-bold tracking-tight text-sidebar-foreground">メッセージ</h2>
        <span className="flex-1" />
        <DropdownMenu>
          <DropdownMenuTrigger asChild>
            <button
              type="button"
              aria-label="新規作成"
              className="flex size-7 items-center justify-center rounded-[7px] text-sidebar-foreground/60 outline-none transition-colors hover:bg-sidebar-accent hover:text-sidebar-foreground focus-visible:ring-2 focus-visible:ring-sidebar-ring"
            >
              <Plus className="size-[17px]" aria-hidden />
            </button>
          </DropdownMenuTrigger>
          <DropdownMenuContent align="end" className="w-[220px]">
            <DropdownMenuItem onSelect={onCreateChannel} className="gap-2">
              <Hash className="size-4" aria-hidden />
              チャンネルを作成
            </DropdownMenuItem>
            <DropdownMenuItem onSelect={onBrowse} className="gap-2">
              <Compass className="size-4" aria-hidden />
              公開チャンネルを探す
            </DropdownMenuItem>
            <DropdownMenuItem onSelect={onNewDm} className="gap-2">
              <MessageCirclePlus className="size-4" aria-hidden />
              ダイレクトメッセージ
            </DropdownMenuItem>
          </DropdownMenuContent>
        </DropdownMenu>
      </div>

      <div className="flex flex-col gap-px px-2.5 pb-2">
        <button
          type="button"
          onClick={() => onView("search")}
          className={cn(
            "mb-1.5 flex h-9 w-full items-center gap-2 rounded-[10px] border px-3 text-left text-[13.5px] outline-none transition-colors focus-visible:ring-2 focus-visible:ring-sidebar-ring",
            view === "search"
              ? "border-sidebar-border bg-background text-sidebar-foreground"
              : "border-sidebar-border bg-background/70 text-sidebar-foreground/55 hover:bg-background",
          )}
        >
          <Search className="size-3.5 shrink-0" aria-hidden />
          <span className="flex-1 truncate">発言を検索</span>
        </button>
        <button
          type="button"
          onClick={() => onView("mentions")}
          aria-current={view === "mentions" ? "true" : undefined}
          className={cn(
            SIDE_BTN,
            view === "mentions"
              ? "bg-sidebar-accent font-medium text-sidebar-foreground"
              : "text-sidebar-foreground/75 hover:bg-sidebar-accent/60 hover:text-sidebar-foreground",
          )}
        >
          <AtSign className="size-4 shrink-0 text-sidebar-foreground/50" aria-hidden />
          <span className="flex-1 truncate">メンション</span>
          {unreadMentions > 0 ? <Badge count={unreadMentions} strong label="未読のメンション" /> : null}
        </button>
        {canSeeApprovals ? (
          <button
            type="button"
            onClick={() => onView("approvals")}
            aria-current={view === "approvals" ? "true" : undefined}
            className={cn(
              SIDE_BTN,
              view === "approvals"
                ? "bg-sidebar-accent font-medium text-sidebar-foreground"
                : "text-sidebar-foreground/75 hover:bg-sidebar-accent/60 hover:text-sidebar-foreground",
            )}
          >
            <ShieldCheck className="size-4 shrink-0 text-sidebar-foreground/50" aria-hidden />
            <span className="flex-1 truncate">閲覧の申請と承認</span>
          </button>
        ) : null}
      </div>

      <div className="px-3 pb-1">
        <div className="flex gap-1 rounded-[10px] bg-sidebar-accent/80 p-[3px]" role="group" aria-label="一覧の絞り込み">
          {(
            [
              ["all", "すべて"],
              ["unread", hotCount > 0 ? `未読 ${hotCount}` : "未読"],
            ] as const
          ).map(([key, label]) => (
            <button
              key={key}
              type="button"
              aria-pressed={filter === key}
              onClick={() => setFilter(key)}
              className={cn(
                "h-7 flex-1 rounded-[8px] text-[12.5px] outline-none transition-colors focus-visible:ring-2 focus-visible:ring-sidebar-ring",
                filter === key
                  ? "bg-background font-semibold text-sidebar-foreground shadow-xs"
                  : "text-sidebar-foreground/60 hover:text-sidebar-foreground",
              )}
            >
              {label}
            </button>
          ))}
        </div>
      </div>

      <div className="scrollbar-subtle min-h-0 flex-1 overflow-y-auto pb-4 pt-1">
        {channels === null ? (
          <div className="flex flex-col gap-2 px-4 py-3" aria-busy="true" aria-label="読み込み中">
            <Skeleton className="h-5 w-3/4" />
            <Skeleton className="h-5 w-2/3" />
            <Skeleton className="h-5 w-4/5" />
          </div>
        ) : (
          <>
            {starred.length > 0 ? <Group label="スター">{starred.map(row)}</Group> : null}
            <Group label="チャンネル" count={rooms.length}>
              {rooms.map(row)}
              {rooms.length === 0 && filter === "all" ? (
                <button
                  type="button"
                  onClick={onBrowse}
                  className="px-2 py-1.5 text-left text-[12.5px] text-sidebar-foreground/55 underline-offset-2 hover:underline"
                >
                  公開チャンネルを探す
                </button>
              ) : null}
            </Group>
            <Group label="ダイレクト" count={dms.length}>
              {dms.map(row)}
              {dms.length === 0 && filter === "all" ? (
                <button
                  type="button"
                  onClick={onNewDm}
                  className="px-2 py-1.5 text-left text-[12.5px] text-sidebar-foreground/55 underline-offset-2 hover:underline"
                >
                  ダイレクトメッセージを始める
                </button>
              ) : null}
            </Group>
            {archived.length > 0 ? <Group label="アーカイブ">{archived.map(row)}</Group> : null}
            {filter === "unread" && hotCount === 0 ? (
              <p className="px-4 py-6 text-center text-[12.5px] text-sidebar-foreground/55">未読はありません</p>
            ) : null}
          </>
        )}
      </div>

      <div className="border-t border-sidebar-border px-2.5 py-2">
        <Link
          href="/settings/agent"
          className={cn(SIDE_BTN, "text-sidebar-foreground/65 hover:bg-sidebar-accent/60 hover:text-sidebar-foreground")}
        >
          <Settings2 className="size-4 shrink-0" aria-hidden />
          <span className="flex-1 truncate">エージェントの名前とアイコン</span>
        </Link>
      </div>
    </aside>
  );
}
