"use client";

/// 3 ペインの左端：チャンネル／ダイレクトメッセージの一覧。
/// 未読は `read_state` からの導出（件数）、メンションは別の強い色のバッジで示す。

import * as React from "react";
import Link from "next/link";
import { AtSign, BellOff, ChevronDown, Compass, Hash, MessageCirclePlus, Plus, Search, ShieldCheck, Settings2 } from "lucide-react";

import { cn } from "@/lib/utils";
import {
  DropdownMenu,
  DropdownMenuContent,
  DropdownMenuItem,
  DropdownMenuTrigger,
} from "@/components/ui/dropdown-menu";
import { Skeleton } from "@/components/ui/skeleton";
import type { ChannelView } from "@/lib/messaging-api";
import { AgentAvatar, ChannelIcon, UserAvatar, useMessagingDisplay } from "./primitives";

export type SideView = "channel" | "search" | "mentions" | "approvals";

function Badge({ count, strong, label }: { count: number; strong: boolean; label: string }) {
  return (
    <span
      className={cn(
        "flex h-[18px] min-w-[18px] items-center justify-center rounded-full px-1.5 text-[10.5px] font-semibold leading-none",
        strong ? "bg-primary text-primary-foreground" : "bg-sidebar-accent text-sidebar-foreground/80",
      )}
    >
      <span className="sr-only">{label}</span>
      {count > 99 ? "99+" : count}
    </span>
  );
}

function Row({ channel, active, onSelect }: { channel: ChannelView; active: boolean; onSelect: () => void }) {
  const { me, agentHasIcon } = useMessagingDisplay();
  const muted = channel.notify === "none";
  const unread = channel.notify === "all" ? channel.unread : 0;
  const hot = unread > 0 || channel.mentions > 0;
  return (
    <button
      type="button"
      onClick={onSelect}
      aria-current={active ? "true" : undefined}
      data-testid="channel-row"
      className={cn(
        "group/ch relative flex h-8 w-full items-center gap-2 rounded-[8px] px-2 text-left outline-none transition-colors",
        "focus-visible:ring-2 focus-visible:ring-sidebar-ring",
        active
          ? "bg-sidebar-accent font-medium text-sidebar-foreground"
          : hot
            ? "font-semibold text-sidebar-foreground hover:bg-sidebar-accent/60"
            : "text-sidebar-foreground/70 hover:bg-sidebar-accent/60 hover:text-sidebar-foreground",
        muted && !active && "text-sidebar-foreground/45",
      )}
    >
      {channel.kind === "dm" ? (
        <span aria-hidden>
          {channel.agent_dm ? (
            <AgentAvatar ownerId={me} hasIcon={agentHasIcon} size="xs" className="rounded-[6px]" />
          ) : (
            <UserAvatar userId={channel.dm_peer ?? channel.id} name={channel.name} size="xs" className="rounded-[6px]" />
          )}
        </span>
      ) : (
        <ChannelIcon
          kind={channel.kind}
          announcement={channel.announcement}
          className={cn("shrink-0", active ? "text-sidebar-foreground" : "text-sidebar-foreground/45")}
        />
      )}
      <span className="min-w-0 flex-1 truncate text-[13px]">{channel.name}</span>
      {muted ? <BellOff className="size-3.5 shrink-0 text-sidebar-foreground/40" aria-label="通知なし" /> : null}
      {channel.mentions > 0 ? (
        <Badge count={channel.mentions} strong label="メンション" />
      ) : unread > 0 ? (
        <Badge count={unread} strong={false} label="未読" />
      ) : null}
    </button>
  );
}

function Group({ label, children, action }: { label: string; children: React.ReactNode; action?: React.ReactNode }) {
  const [open, setOpen] = React.useState(true);
  return (
    <div className="mt-1">
      <div className="flex items-center gap-1 px-2">
        <button
          type="button"
          onClick={() => setOpen((v) => !v)}
          aria-expanded={open}
          className="flex h-7 flex-1 items-center gap-1 rounded-[7px] px-1 text-left text-[11.5px] font-semibold uppercase tracking-wide text-sidebar-foreground/50 outline-none transition-colors hover:text-sidebar-foreground/80 focus-visible:ring-2 focus-visible:ring-sidebar-ring"
        >
          <ChevronDown
            className={cn(
              "size-3.5 transition-transform duration-[var(--duration-fast)] ease-[var(--ease-standard)]",
              !open && "-rotate-90",
            )}
            aria-hidden
          />
          {label}
        </button>
        {action}
      </div>
      {open ? <div className="flex flex-col gap-px px-1.5 pb-1">{children}</div> : null}
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
}) {
  const rooms = (channels ?? []).filter((c) => c.kind !== "dm" && !c.archived);
  const dms = (channels ?? []).filter((c) => c.kind === "dm");
  const archived = (channels ?? []).filter((c) => c.kind !== "dm" && c.archived);

  return (
    <aside
      className={cn(
        "flex h-full shrink-0 flex-col border-r border-sidebar-border bg-sidebar",
        drawer ? "w-full" : "w-[248px]",
      )}
      aria-label="チャンネル一覧"
    >
      <div className={cn("flex h-14 shrink-0 items-center gap-1 px-3", drawer && "pr-11")}>
        <h2 className="truncate text-[15px] font-semibold text-sidebar-foreground">メッセージ</h2>
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
            "mb-1 flex h-8 w-full items-center gap-2 rounded-[9px] border px-2.5 text-left text-[12.5px] outline-none transition-colors focus-visible:ring-2 focus-visible:ring-sidebar-ring",
            view === "search"
              ? "border-sidebar-border bg-sidebar-accent text-sidebar-foreground"
              : "border-sidebar-border bg-sidebar-accent/40 text-sidebar-foreground/55 hover:bg-sidebar-accent",
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

      <div className="scrollbar-subtle shiki-dash-top min-h-0 flex-1 overflow-y-auto pb-4 pt-1">
        {channels === null ? (
          <div className="flex flex-col gap-2 px-4 py-3" aria-busy="true" aria-label="読み込み中">
            <Skeleton className="h-5 w-3/4" />
            <Skeleton className="h-5 w-2/3" />
            <Skeleton className="h-5 w-4/5" />
          </div>
        ) : (
          <>
            <Group label="チャンネル">
              {rooms.map((c) => (
                <Row key={c.id} channel={c} active={view === "channel" && c.id === activeId} onSelect={() => onSelect(c.id)} />
              ))}
              {rooms.length === 0 ? (
                <button
                  type="button"
                  onClick={onBrowse}
                  className="px-2 py-1.5 text-left text-[12px] text-sidebar-foreground/55 underline-offset-2 hover:underline"
                >
                  公開チャンネルを探す
                </button>
              ) : null}
            </Group>
            <Group label="ダイレクト">
              {dms.map((c) => (
                <Row key={c.id} channel={c} active={view === "channel" && c.id === activeId} onSelect={() => onSelect(c.id)} />
              ))}
              {dms.length === 0 ? (
                <button
                  type="button"
                  onClick={onNewDm}
                  className="px-2 py-1.5 text-left text-[12px] text-sidebar-foreground/55 underline-offset-2 hover:underline"
                >
                  ダイレクトメッセージを始める
                </button>
              ) : null}
            </Group>
            {archived.length > 0 ? (
              <Group label="アーカイブ">
                {archived.map((c) => (
                  <Row key={c.id} channel={c} active={view === "channel" && c.id === activeId} onSelect={() => onSelect(c.id)} />
                ))}
              </Group>
            ) : null}
          </>
        )}
      </div>

      <div className="shiki-dash-top px-2.5 py-2">
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
