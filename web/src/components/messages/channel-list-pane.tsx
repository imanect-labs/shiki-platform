"use client";

/// 3 ペインの左端：チャンネル／ダイレクトメッセージの一覧。
/// 未読は `read_state` からの導出（バッジ）で示し、行ごとの既読フラグは持たない。

import * as React from "react";
import { ChevronDown, Plus, Search } from "lucide-react";

import { cn } from "@/lib/utils";
import { Tooltip, TooltipContent, TooltipTrigger } from "@/components/ui/tooltip";
import { channelTitle, type Channel, unreadCount } from "@/lib/messages-mock";
import { ChannelIcon, MemberAvatar } from "./primitives";

function Row({
  channel,
  active,
  viewerId,
  onSelect,
}: {
  channel: Channel;
  active: boolean;
  viewerId: string;
  onSelect: () => void;
}) {
  const unread = unreadCount(channel);
  const other = channel.memberIds.find((m) => m !== viewerId) ?? viewerId;
  const isSoloDm = channel.kind === "dm" && channel.memberIds.length === 2;

  return (
    <button
      type="button"
      onClick={onSelect}
      aria-current={active ? "true" : undefined}
      className={cn(
        "group/ch relative flex h-8 w-full items-center gap-2 rounded-[8px] px-2 text-left outline-none transition-colors",
        "focus-visible:ring-2 focus-visible:ring-sidebar-ring",
        active
          ? "bg-sidebar-accent font-medium text-sidebar-foreground"
          : unread > 0
            ? "font-medium text-sidebar-foreground hover:bg-sidebar-accent/60"
            : "text-sidebar-foreground/70 hover:bg-sidebar-accent/60 hover:text-sidebar-foreground",
      )}
    >
      {isSoloDm ? (
        <span aria-hidden>
          <MemberAvatar memberId={other} size="xs" showPresence className="rounded-[6px]" />
        </span>
      ) : (
        <ChannelIcon
          kind={channel.kind}
          memberCount={channel.memberIds.length}
          className={cn("shrink-0", active ? "text-sidebar-foreground" : "text-sidebar-foreground/45")}
        />
      )}
      <span className="min-w-0 flex-1 truncate text-[13px]">{channelTitle(channel, viewerId)}</span>
      {unread > 0 ? (
        <span className="flex h-[18px] min-w-[18px] items-center justify-center rounded-full bg-primary px-1.5 text-[10.5px] font-semibold leading-none text-primary-foreground">
          <span className="sr-only">未読</span>
          {unread}
        </span>
      ) : null}
    </button>
  );
}

function Group({
  label,
  children,
  action,
}: {
  label: string;
  children: React.ReactNode;
  action?: React.ReactNode;
}) {
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

export function ChannelListPane({
  channels,
  activeId,
  viewerId,
  searchOpen,
  drawer,
  onSelect,
  onOpenSearch,
  onCreateChannel,
}: {
  channels: Channel[];
  activeId: string | null;
  viewerId: string;
  searchOpen: boolean;
  /// ドロワ（狭幅）として描くか。Sheet 自身の閉じるボタンが右上に重なるため、
  /// 見出し行の右側を空けて「＋」がぶつからないようにする。
  drawer?: boolean;
  onSelect: (id: string) => void;
  onOpenSearch: () => void;
  onCreateChannel: () => void;
}) {
  // 参加しているものだけを一覧に出す（非参加チャンネルは存在ごと見えない）。
  const mine = channels.filter((c) => c.memberIds.includes(viewerId));
  const rooms = mine.filter((c) => c.kind !== "dm");
  const dms = mine.filter((c) => c.kind === "dm");

  return (
    <aside
      className={cn(
        "flex h-full shrink-0 flex-col border-r border-sidebar-border bg-sidebar",
        drawer ? "w-full" : "w-[236px]",
      )}
    >
      <div className={cn("flex h-14 shrink-0 items-center gap-1 px-3", drawer && "pr-11")}>
        <h2 className="truncate text-[15px] font-semibold text-sidebar-foreground">メッセージ</h2>
        {/* 捏造データを実データと取り違えさせないための表示。バックエンド（crates/messaging・
            OpenFGA channel 型・SSE 配信）が入ったら消す。 */}
        <Tooltip>
          <TooltipTrigger asChild>
            <span className="shrink-0 cursor-default rounded-full border border-dashed border-sidebar-border px-1.5 py-px text-[10px] font-medium leading-4 text-sidebar-foreground/60">
              モック
            </span>
          </TooltipTrigger>
          <TooltipContent side="bottom" className="max-w-[240px]">
            画面のみの試作です。発言は保存されず、表示されている人と発言はすべて架空のものです。
          </TooltipContent>
        </Tooltip>
        <span className="flex-1" />
        <Tooltip>
          <TooltipTrigger asChild>
            <button
              type="button"
              onClick={onCreateChannel}
              aria-label="チャンネルを作成"
              className="flex size-7 items-center justify-center rounded-[7px] text-sidebar-foreground/60 outline-none transition-colors hover:bg-sidebar-accent hover:text-sidebar-foreground focus-visible:ring-2 focus-visible:ring-sidebar-ring"
            >
              <Plus className="size-[17px]" aria-hidden />
            </button>
          </TooltipTrigger>
          <TooltipContent side="bottom">チャンネルを作成</TooltipContent>
        </Tooltip>
      </div>

      <div className="px-2.5 pb-2">
        <button
          type="button"
          onClick={onOpenSearch}
          className={cn(
            "flex h-8 w-full items-center gap-2 rounded-[9px] border px-2.5 text-left text-[12.5px] outline-none transition-colors focus-visible:ring-2 focus-visible:ring-sidebar-ring",
            searchOpen
              ? "border-sidebar-border bg-sidebar-accent text-sidebar-foreground"
              : "border-sidebar-border bg-sidebar-accent/40 text-sidebar-foreground/45 hover:bg-sidebar-accent",
          )}
        >
          <Search className="size-3.5 shrink-0" aria-hidden />
          <span className="flex-1 truncate">発言を検索</span>
        </button>
      </div>

      <div className="scrollbar-subtle min-h-0 flex-1 overflow-y-auto pb-4">
        <Group label="チャンネル">
          {rooms.map((c) => (
            <Row
              key={c.id}
              channel={c}
              viewerId={viewerId}
              active={!searchOpen && c.id === activeId}
              onSelect={() => onSelect(c.id)}
            />
          ))}
        </Group>
        <Group label="ダイレクト">
          {dms.map((c) => (
            <Row
              key={c.id}
              channel={c}
              viewerId={viewerId}
              active={!searchOpen && c.id === activeId}
              onSelect={() => onSelect(c.id)}
            />
          ))}
        </Group>
      </div>
    </aside>
  );
}
