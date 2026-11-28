"use client";

/// 本体の右に開く一覧：このチャンネルに共有した資料・ピン留めした発言・参加者（design §4.14）。
/// スレッドと同じ場所に出し、どちらか一方だけを開く。資料とピンは閲覧者の権限でサーバが絞った結果だけを出す。

import * as React from "react";
import Link from "next/link";
import { FolderOpen, Loader2, MessageSquareReply, Paperclip, Pin, PinOff, Settings2, Users, X } from "lucide-react";

import { cn } from "@/lib/utils";
import { formatBytes } from "@/lib/format";
import { NodeIcon } from "@/components/drive/primitives";
import {
  listChannelFiles,
  listPins,
  type ChannelFileEntry,
  type ChannelView,
  type MemberView,
  type MessageView,
} from "@/lib/messaging-api";
import { AuthorAvatar, AuthorLabel, Blocks, UserAvatar } from "./primitives";
import type { SettingsTab } from "./channel-settings-dialog";
import { dateTimeLabel } from "./time";

export type SidePanelTab = "files" | "pins" | "members";

type Loaded<T> = { items: T[]; loading: boolean; error: string | null };

function errorText(e: unknown): string {
  return e instanceof Error ? e.message : "読み込めませんでした";
}

/// 資料とピンの一覧（ヘッダの件数と右の一覧で共有する）。チャンネルを替えたら読み直す。
/// ピンは `pinSignature`（読み込み済みの発言のうちピン留めされたものの並び）が変わったときにも読み直す。
/// 付け外しは配信の差分で発言の `pinned` が変わるので、他の人の操作もここで拾える。
export function useChannelExtras(channelId: string | null, pinSignature: string) {
  const [files, setFiles] = React.useState<Loaded<ChannelFileEntry>>({ items: [], loading: false, error: null });
  const [pins, setPins] = React.useState<Loaded<MessageView>>({ items: [], loading: false, error: null });

  const reloadFiles = React.useCallback(async () => {
    if (!channelId) return;
    setFiles((s) => ({ ...s, loading: true, error: null }));
    try {
      const items = await listChannelFiles(channelId);
      setFiles({ items, loading: false, error: null });
    } catch (e) {
      setFiles((s) => ({ ...s, loading: false, error: errorText(e) }));
    }
  }, [channelId]);

  const reloadPins = React.useCallback(async () => {
    if (!channelId) return;
    setPins((s) => ({ ...s, loading: true, error: null }));
    try {
      const items = await listPins(channelId);
      setPins({ items, loading: false, error: null });
    } catch (e) {
      setPins((s) => ({ ...s, loading: false, error: errorText(e) }));
    }
  }, [channelId]);

  React.useEffect(() => {
    setFiles({ items: [], loading: false, error: null });
    setPins({ items: [], loading: false, error: null });
    void reloadFiles();
  }, [channelId, reloadFiles]);

  React.useEffect(() => {
    void reloadPins();
  }, [reloadPins, pinSignature]);

  return { files, pins, reloadFiles, reloadPins };
}

const TABS: { key: SidePanelTab; label: string }[] = [
  { key: "files", label: "資料" },
  { key: "pins", label: "ピン" },
  { key: "members", label: "参加者" },
];

function Status({ loading, error, empty, emptyText }: { loading: boolean; error: string | null; empty: boolean; emptyText: React.ReactNode }) {
  if (error) {
    return (
      <p className="px-5 py-10 text-center text-[13px] text-destructive" role="alert">
        {error}
      </p>
    );
  }
  if (loading && empty) {
    return (
      <div className="flex items-center justify-center gap-2 py-10 text-[13px] text-muted-foreground" aria-busy="true">
        <Loader2 className="size-4 animate-spin" aria-hidden />
        読み込んでいます
      </div>
    );
  }
  if (empty) return <div className="px-6 py-10 text-center text-[13px] leading-relaxed text-muted-foreground">{emptyText}</div>;
  return null;
}

function FilesTab({ state }: { state: Loaded<ChannelFileEntry> }) {
  return (
    <>
      <p className="px-5 pb-2 pt-4 text-[12.5px] leading-relaxed text-muted-foreground">
        このチャンネルに共有した資料です。参加者全員が開け、シキが答えるときの根拠にも使われます。
      </p>
      <Status
        loading={state.loading}
        error={state.error}
        empty={state.items.length === 0}
        emptyText={
          <>
            まだ共有された資料はありません。
            <br />
            ドライブの「共有」からこのチャンネルを選ぶと、ここに並びます。
          </>
        }
      />
      <ul className="flex flex-col px-2 pb-2">
        {state.items.map((f) => (
          <li key={f.node_id}>
            <Link
              href={f.kind === "folder" ? `/drive?folder=${encodeURIComponent(f.node_id)}` : `/file/${encodeURIComponent(f.node_id)}`}
              className="flex items-center gap-3 rounded-[10px] px-3 py-2.5 outline-none transition-colors hover:bg-accent focus-visible:ring-2 focus-visible:ring-ring"
              data-testid="channel-file"
            >
              <span className="flex size-10 shrink-0 items-center justify-center rounded-[10px] bg-muted ring-1 ring-inset ring-border">
                <NodeIcon kind={f.kind} name={f.name} contentType={f.content_type} className="size-5" />
              </span>
              <span className="min-w-0 flex-1">
                <span className="block truncate text-[14px] font-semibold text-foreground">{f.name}</span>
                <span className="block truncate text-[12px] text-muted-foreground">
                  {f.kind === "folder" ? "フォルダ" : f.size_bytes != null ? formatBytes(f.size_bytes) : "ファイル"}
                  {" · 更新 "}
                  {dateTimeLabel(f.updated_at)}
                </span>
              </span>
            </Link>
          </li>
        ))}
      </ul>
      <div className="px-5 pb-5 pt-1">
        <Link
          href="/drive"
          className="inline-flex h-8 items-center gap-1.5 rounded-full border border-border px-3 text-[12.5px] font-medium text-foreground/80 outline-none transition-colors hover:bg-accent focus-visible:ring-2 focus-visible:ring-ring"
        >
          <FolderOpen className="size-3.5" aria-hidden />
          ドライブで共有する
        </Link>
      </div>
    </>
  );
}

function PinsTab({
  state,
  canPin,
  onOpenThread,
  onUnpin,
}: {
  state: Loaded<MessageView>;
  canPin: boolean;
  onOpenThread: (threadRootId: string) => void;
  onUnpin: (m: MessageView) => void;
}) {
  return (
    <>
      <p className="px-5 pb-2 pt-4 text-[12.5px] leading-relaxed text-muted-foreground">
        大事な発言・回答を参加者全員で見返せるよう留めておく一覧です。発言の操作から付け外しできます。
      </p>
      <Status
        loading={state.loading}
        error={state.error}
        empty={state.items.length === 0}
        emptyText="ピン留めした発言はまだありません。"
      />
      <ul className="flex flex-col gap-2 px-4 pb-5">
        {state.items.map((m) => (
          <li key={m.id} className="group/pin relative rounded-[12px] border border-border/70 bg-card px-4 py-3" data-testid="pinned-message">
            <div className="flex items-center gap-2">
              <AuthorAvatar author={m.author} size="xs" className="size-6 rounded-[7px]" />
              <span className="flex min-w-0 flex-1 flex-wrap items-baseline gap-x-1.5 [&>span:first-child]:text-[13.5px]">
                <AuthorLabel author={m.author} />
                <time dateTime={m.created_at} className="text-[11.5px] text-muted-foreground/75">
                  {dateTimeLabel(m.created_at)}
                </time>
              </span>
            </div>
            <div className="mt-1.5 line-clamp-4 text-[13.5px] [&_p]:text-[13.5px] [&_p]:leading-[1.65] [&_.markdown]:text-[13.5px]">
              <Blocks message={m} />
            </div>
            <div className="mt-2 flex items-center gap-1">
              <button
                type="button"
                onClick={() => onOpenThread(m.parent_id ?? m.id)}
                className="inline-flex h-7 items-center gap-1 rounded-[7px] px-2 text-[12px] font-medium text-muted-foreground outline-none transition-colors hover:bg-accent hover:text-foreground focus-visible:ring-2 focus-visible:ring-ring"
              >
                <MessageSquareReply className="size-3.5" aria-hidden />
                スレッドで開く
              </button>
              {canPin ? (
                <button
                  type="button"
                  onClick={() => onUnpin(m)}
                  className="inline-flex h-7 items-center gap-1 rounded-[7px] px-2 text-[12px] font-medium text-muted-foreground outline-none transition-colors hover:bg-accent hover:text-foreground focus-visible:ring-2 focus-visible:ring-ring"
                >
                  <PinOff className="size-3.5" aria-hidden />
                  外す
                </button>
              ) : null}
            </div>
          </li>
        ))}
      </ul>
    </>
  );
}

function MembersTab({
  channel,
  members,
  onOpenSettings,
}: {
  channel: ChannelView;
  members: MemberView[];
  onOpenSettings: (tab: SettingsTab) => void;
}) {
  const active = members.filter((m) => m.active);
  const inactive = members.filter((m) => !m.active);
  return (
    <>
      <ul className="flex flex-col px-2 pb-2 pt-3">
        {[...active, ...inactive].map((m) => (
          <li key={m.user_id} className="flex items-center gap-3 rounded-[10px] px-3 py-2">
            <UserAvatar userId={m.user_id} name={m.display_name} size="sm" className="size-8" />
            <span className={cn("min-w-0 flex-1 truncate text-[14px] font-medium", !m.active && "text-muted-foreground")}>
              {m.display_name}
            </span>
            {!m.active ? <span className="text-[11.5px] text-muted-foreground">無効</span> : null}
            {channel.announcement && m.can_post && !m.is_admin ? (
              <span className="rounded-full border border-border px-1.5 py-px text-[10.5px] font-medium leading-4 text-muted-foreground">投稿できる</span>
            ) : null}
            {m.is_admin ? (
              <span className="rounded-full bg-muted px-1.5 py-px text-[10.5px] font-medium leading-4 text-muted-foreground">管理者</span>
            ) : null}
          </li>
        ))}
      </ul>
      <div className="px-5 pb-5 pt-1">
        <button
          type="button"
          onClick={() => onOpenSettings("members")}
          className="inline-flex h-8 items-center gap-1.5 rounded-full border border-border px-3 text-[12.5px] font-medium text-foreground/80 outline-none transition-colors hover:bg-accent focus-visible:ring-2 focus-visible:ring-ring"
        >
          <Settings2 className="size-3.5" aria-hidden />
          {channel.is_admin ? "参加者を招待・管理" : "参加者の詳細"}
        </button>
      </div>
    </>
  );
}

export function ChannelSidePanel({
  channel,
  tab,
  members,
  files,
  pins,
  canPin,
  onTabChange,
  onClose,
  onOpenThread,
  onUnpin,
  onOpenSettings,
}: {
  channel: ChannelView;
  tab: SidePanelTab;
  members: MemberView[];
  files: Loaded<ChannelFileEntry>;
  pins: Loaded<MessageView>;
  canPin: boolean;
  onTabChange: (tab: SidePanelTab) => void;
  onClose: () => void;
  onOpenThread: (threadRootId: string) => void;
  onUnpin: (m: MessageView) => void;
  onOpenSettings: (tab: SettingsTab) => void;
}) {
  const isDm = channel.kind === "dm";
  const tabs = TABS.filter((t) => !(isDm && t.key === "members"));
  const count = (k: SidePanelTab) => (k === "files" ? files.items.length : k === "pins" ? pins.items.length : channel.member_count);

  React.useEffect(() => {
    const onKey = (e: KeyboardEvent) => {
      if (e.key === "Escape" && !e.defaultPrevented && document.activeElement?.tagName !== "TEXTAREA") onClose();
    };
    window.addEventListener("keydown", onKey);
    return () => window.removeEventListener("keydown", onKey);
  }, [onClose]);

  return (
    <aside
      className="flex h-full w-full shrink-0 flex-col border-l border-border bg-background sm:w-[360px] lg:w-[400px]"
      aria-label="チャンネルの資料・ピン・参加者"
      data-testid="channel-side-panel"
    >
      <header className="flex h-[60px] shrink-0 items-end gap-1 border-b border-border/70 pl-3 pr-3">
        <div className="flex min-w-0 flex-1 items-end gap-1" role="tablist" aria-label="一覧の種類">
          {tabs.map((t) => (
            <button
              key={t.key}
              type="button"
              role="tab"
              aria-selected={tab === t.key}
              onClick={() => onTabChange(t.key)}
              className={cn(
                "relative flex h-11 items-center gap-1.5 px-2.5 text-[14px] outline-none transition-colors focus-visible:ring-2 focus-visible:ring-ring",
                tab === t.key ? "font-bold text-foreground" : "text-muted-foreground hover:text-foreground",
              )}
            >
              {t.label}
              <span className="text-[12px] font-medium tabular-nums text-muted-foreground">{count(t.key)}</span>
              {tab === t.key ? <span className="absolute inset-x-2 bottom-0 h-[2px] rounded-full bg-foreground" aria-hidden /> : null}
            </button>
          ))}
        </div>
        <button
          type="button"
          onClick={onClose}
          aria-label="一覧を閉じる"
          className="mb-2.5 flex size-8 items-center justify-center rounded-lg text-muted-foreground outline-none transition-colors hover:bg-accent hover:text-foreground focus-visible:ring-2 focus-visible:ring-ring"
        >
          <X className="size-4" aria-hidden />
        </button>
      </header>
      <div className="scrollbar-subtle min-h-0 flex-1 overflow-y-auto" role="tabpanel">
        {tab === "files" ? (
          <FilesTab state={files} />
        ) : tab === "pins" ? (
          <PinsTab state={pins} canPin={canPin} onOpenThread={onOpenThread} onUnpin={onUnpin} />
        ) : (
          <MembersTab channel={channel} members={members} onOpenSettings={onOpenSettings} />
        )}
      </div>
    </aside>
  );
}

/// ヘッダに並べる入口（資料・ピン・参加者の件数）。押すと右の一覧を開く（開いている種類はもう一度押すと閉じる）。
export function SidePanelPills({
  channel,
  fileCount,
  pinCount,
  members,
  open,
  onOpen,
}: {
  channel: ChannelView;
  fileCount: number;
  pinCount: number;
  members: MemberView[];
  open: SidePanelTab | null;
  onOpen: (tab: SidePanelTab) => void;
}) {
  const isDm = channel.kind === "dm";
  const pill = (key: SidePanelTab, icon: React.ReactNode, label: string, n: number, extra?: React.ReactNode) => (
    <button
      key={key}
      type="button"
      onClick={() => onOpen(key)}
      aria-pressed={open === key}
      aria-label={`${label} ${n} 件`}
      data-testid={`pill-${key}`}
      className={cn(
        "flex h-8 shrink-0 items-center gap-1.5 rounded-full border px-3 text-[12.5px] outline-none transition-colors focus-visible:ring-2 focus-visible:ring-ring",
        open === key
          ? "border-primary bg-primary text-primary-foreground"
          : "border-border bg-background text-foreground/80 hover:bg-accent",
      )}
    >
      {extra ?? icon}
      <span className="hidden sm:inline">{label}</span>
      <span className="font-bold tabular-nums">{n}</span>
    </button>
  );
  const faces = members.filter((m) => m.active).slice(0, 3);
  return (
    <div className="flex items-center gap-1.5">
      {pill("files", <Paperclip className="size-3.5" aria-hidden />, "資料", fileCount)}
      {pill("pins", <Pin className="size-3.5" aria-hidden />, "ピン", pinCount)}
      {!isDm
        ? pill(
            "members",
            <Users className="size-3.5" aria-hidden />,
            "参加者",
            channel.member_count,
            faces.length > 0 ? (
              <span className="-ml-1.5 flex -space-x-1.5" aria-hidden>
                {faces.map((m) => (
                  <UserAvatar
                    key={m.user_id}
                    userId={m.user_id}
                    name={m.display_name}
                    size="xs"
                    className="rounded-full ring-2 ring-background [&>*]:rounded-full"
                  />
                ))}
              </span>
            ) : undefined,
          )
        : null}
    </div>
  );
}
