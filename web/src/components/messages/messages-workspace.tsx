"use client";

/// 職員間メッセージ（Phase 14・design §4.14）の 3 ペイン画面（チャンネル一覧／発言／スレッド）。
///
/// 開いているチャンネル・スレッド・表示（検索・メンション・承認）は URL（`?c=&t=&v=`）に持ち、
/// リンクや再読み込みで同じ場所に戻れるようにする。認可はすべてサーバが判定し、画面は操作の出し分けだけ。

import * as React from "react";
import { useRouter, useSearchParams } from "next/navigation";
import { AlertCircle, Bell, Loader2, LogOut, MessageSquareText, PanelLeft, Settings2, ShieldCheck, UserPlus, WifiOff } from "lucide-react";

import { cn } from "@/lib/utils";
import { seasonVar } from "@/lib/season";
import { Button } from "@/components/ui/button";
import { EmptyState } from "@/components/ui/empty-state";
import { Sheet, SheetContent, SheetTitle } from "@/components/ui/sheet";
import { toast } from "@/components/ui/use-toast";
import { MenuMarkButton } from "@/components/shell/header";
import { useSidebar } from "@/components/shell/sidebar/sidebar-context";
import { useMe } from "@/hooks/use-me";
import { notifyMentionsChanged, useUnreadMentions } from "@/hooks/use-unread-mentions";
import {
  DEFAULT_AGENT_NAME,
  deleteMessage,
  editMessage,
  getAgentProfile,
  getMessage,
  hideDm,
  joinChannel,
  listAccessRequests,
  listChannels,
  markRead,
  mentionsAgent,
  MessagingError,
  postMessage,
  react as reactTo,
  type AgentProfileView,
  type ChannelView,
  type MessageBlock,
  type MessageView,
} from "@/lib/messaging-api";
import { cachedUserName, resolveUserNames } from "@/lib/user-names";
import { cachedRoleName, resolveRoleNames } from "@/lib/role-names";
import { AccessRequestList } from "./access-requests";
import { BrowseChannelsDialog, NewDmDialog } from "./browse-dialogs";
import { ChannelHeader } from "./channel-header";
import { ChannelListPane, type SideView } from "./channel-list-pane";
import { ChannelSettingsDialog, type SettingsTab } from "./channel-settings-dialog";
import { Composer } from "./composer";
import { CreateChannelDialog } from "./create-channel-dialog";
import { MentionsView } from "./mentions-view";
import { MessageList, type MessageActions } from "./message-list";
import { ChannelTile, MessagingDisplayContext, UserAvatar, type MessagingDisplay } from "./primitives";
import { SearchView } from "./search-view";
import { ThreadPane } from "./thread-pane";
import { useChannel } from "./use-channel";

const LIST_POLL_MS = 30_000;

function errorText(e: unknown): string {
  return e instanceof Error ? e.message : String(e);
}

export function MessagesWorkspace() {
  const router = useRouter();
  const params = useSearchParams();
  const me = useMe();
  const { setMobileOpen } = useSidebar();
  const myId = me.data?.id ?? "";
  const activeId = params.get("c");
  const threadParam = params.get("t");
  const viewParam = params.get("v");
  const view: SideView =
    viewParam === "search" || viewParam === "mentions" || viewParam === "approvals" ? viewParam : "channel";

  const [channels, setChannels] = React.useState<ChannelView[] | null>(null);
  const [listError, setListError] = React.useState<{ status: number; message: string } | null>(null);
  const [profile, setProfile] = React.useState<AgentProfileView | null>(null);
  const [canApprovals, setCanApprovals] = React.useState(false);
  const [listOpen, setListOpen] = React.useState(false);
  const [createOpen, setCreateOpen] = React.useState(false);
  const [browseOpen, setBrowseOpen] = React.useState(false);
  const [dmOpen, setDmOpen] = React.useState(false);
  const [settings, setSettings] = React.useState<{ open: boolean; tab: SettingsTab }>({ open: false, tab: "about" });
  const [namesTick, setNamesTick] = React.useState(0);
  const [joining, setJoining] = React.useState(false);
  const [firstUnreadId, setFirstUnreadId] = React.useState<string | null>(null);
  const unreadMentions = useUnreadMentions();
  const scroller = React.useRef<HTMLDivElement>(null);
  const sentinel = React.useRef<HTMLDivElement>(null);

  const go = React.useCallback(
    (next: { c?: string | null; t?: string | null; v?: SideView }) => {
      const q = new URLSearchParams();
      const c = next.c === undefined ? activeId : next.c;
      const v = next.v ?? "channel";
      if (c) q.set("c", c);
      if (next.t) q.set("t", next.t);
      if (v !== "channel") q.set("v", v);
      const s = q.toString();
      router.replace(s ? `/messages?${s}` : "/messages", { scroll: false });
    },
    [router, activeId],
  );

  const loadChannels = React.useCallback(async () => {
    try {
      const list = await listChannels();
      setChannels(list);
      setListError(null);
      return list;
    } catch (e) {
      setListError({ status: e instanceof MessagingError ? e.status : 0, message: errorText(e) });
      return null;
    }
  }, []);

  React.useEffect(() => {
    void loadChannels();
    getAgentProfile().then(setProfile, () => undefined);
    listAccessRequests().then(
      () => setCanApprovals(true),
      () => setCanApprovals(false),
    );
    const t = window.setInterval(() => {
      if (document.visibilityState === "visible") void loadChannels();
    }, LIST_POLL_MS);
    return () => window.clearInterval(t);
  }, [loadChannels]);

  const listed = channels?.find((c) => c.id === activeId) ?? null;
  const ch = useChannel(activeId, listed);
  const { apply, expectAgent, refreshAgentReplies, hasMore, loadingOlder, loadOlder } = ch;
  const active = ch.channel;

  // 何も開いていなければ、最初のチャンネル（無ければ DM）を開く。
  React.useEffect(() => {
    if (activeId || view !== "channel" || !channels || channels.length === 0) return;
    const first = channels.find((c) => c.kind !== "dm" && !c.archived) ?? channels[0];
    go({ c: first.id });
  }, [activeId, view, channels, go]);

  // URL のスレッドを開く（チャンネルが変わると useChannel が閉じるので、その後に開き直す）。
  React.useEffect(() => {
    if ((threadParam ?? null) !== ch.threadId) ch.openThread(threadParam ?? null);
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [threadParam, activeId]);

  // 未読の境目は開いた時点の件数で決める（既読にした後も「どこから未読だったか」を残す）。
  const unreadAtOpen = React.useRef<{ id: string | null; unread: number }>({ id: null, unread: 0 });
  React.useEffect(() => {
    if (unreadAtOpen.current.id === activeId) return;
    if (!listed && channels !== null) {
      unreadAtOpen.current = { id: activeId, unread: 0 };
      return;
    }
    if (listed) unreadAtOpen.current = { id: activeId, unread: listed.notify === "all" ? listed.unread : 0 };
    setFirstUnreadId(null);
  }, [activeId, listed, channels]);
  const loadedFor = React.useRef<string | null>(null);
  React.useEffect(() => {
    if (ch.loading || ch.messages.length === 0 || loadedFor.current === activeId) return;
    loadedFor.current = activeId;
    const n = unreadAtOpen.current.id === activeId ? unreadAtOpen.current.unread : 0;
    if (n > 0) setFirstUnreadId(ch.messages[Math.max(0, ch.messages.length - n)]?.id ?? null);
    // 開いたら最下部から読み始める。
    requestAnimationFrame(() => {
      const el = scroller.current;
      if (el) el.scrollTop = el.scrollHeight;
    });
  }, [ch.loading, ch.messages, activeId]);

  // 見ているチャンネルを既読にする（発言が増えるたびに少し置いて）。
  const lastId = ch.messages[ch.messages.length - 1]?.id;
  React.useEffect(() => {
    if (!activeId || view !== "channel" || !active?.joined || ch.loading) return;
    const t = setTimeout(() => {
      if (document.visibilityState !== "visible") return;
      markRead(activeId)
        .then(() => {
          setChannels((prev) => prev?.map((c) => (c.id === activeId ? { ...c, unread: 0, mentions: 0 } : c)) ?? prev);
          notifyMentionsChanged();
        })
        .catch(() => undefined);
    }, 800);
    return () => clearTimeout(t);
  }, [activeId, view, active?.joined, ch.loading, lastId]);

  // 新しい発言が届いたら、最下部付近にいる時だけ追従する（遡って読んでいる手を奪わない）。
  const prevLast = React.useRef<string | undefined>(undefined);
  React.useLayoutEffect(() => {
    const el = scroller.current;
    if (!el || !lastId || prevLast.current === lastId) return;
    const first = prevLast.current === undefined;
    prevLast.current = lastId;
    if (first) return;
    const nearBottom = el.scrollHeight - el.scrollTop - el.clientHeight < 240;
    const mine = ch.messages[ch.messages.length - 1]?.author.user_id === myId;
    if (nearBottom || mine) el.scrollTo({ top: el.scrollHeight, behavior: "smooth" });
  }, [lastId, ch.messages, myId]);
  React.useEffect(() => {
    prevLast.current = undefined;
  }, [activeId]);
  // 自分だけに表示の回答（行の下に差し込む）が増えたら、最下部付近にいる時だけ追従する。
  const replyCount = ch.agentReplies.length;
  React.useEffect(() => {
    const el = scroller.current;
    if (!el || replyCount === 0) return;
    if (el.scrollHeight - el.scrollTop - el.clientHeight < 480) el.scrollTo({ top: el.scrollHeight });
  }, [replyCount]);

  // 古い発言は上端に近づいたら読む。読み足した分だけ位置をずらし、見ていた行を動かさない。
  const restore = React.useRef<number | null>(null);
  const firstId = ch.messages[0]?.id;
  React.useLayoutEffect(() => {
    const el = scroller.current;
    if (el && restore.current !== null) {
      el.scrollTop = el.scrollHeight - restore.current;
      restore.current = null;
    }
  }, [firstId]);
  React.useEffect(() => {
    const el = sentinel.current;
    const root = scroller.current;
    if (!el || !root || !hasMore) return;
    const io = new IntersectionObserver(
      (entries) => {
        if (entries.some((e) => e.isIntersecting) && !loadingOlder) {
          restore.current = root.scrollHeight - root.scrollTop;
          void loadOlder();
        }
      },
      { root, rootMargin: "200px 0px 0px 0px" },
    );
    io.observe(el);
    return () => io.disconnect();
  }, [hasMore, loadingOlder, loadOlder]);

  // メンションの表記に使う名前（参加者・書き手・ディレクトリ・ロール）。
  React.useEffect(() => {
    const users = new Set<string>();
    const roles = new Set<string>();
    for (const m of [...ch.messages, ...ch.thread]) {
      for (const b of m.blocks) {
        if (b.type !== "mention") continue;
        if (b.mention.kind === "user") users.add(b.mention.user_id);
        if (b.mention.kind === "role") roles.add(b.mention.role_id);
      }
    }
    for (const mem of ch.members) users.delete(mem.user_id);
    void Promise.all([resolveUserNames(users), resolveRoleNames(roles)]).then(([a, b]) => {
      if (a || b) setNamesTick((n) => n + 1);
    });
  }, [ch.messages, ch.thread, ch.members]);

  const display: MessagingDisplay = React.useMemo(() => {
    const names = new Map<string, string>();
    for (const m of [...ch.messages, ...ch.thread]) {
      if (m.author.kind === "user") names.set(m.author.user_id, m.author.display_name);
      for (const b of m.blocks) {
        if (b.type !== "mention") continue;
        if (b.mention.kind === "user") {
          const n = cachedUserName(b.mention.user_id);
          if (n) names.set(b.mention.user_id, n);
        } else if (b.mention.kind === "role") {
          const n = cachedRoleName(b.mention.role_id);
          if (n) names.set(`role:${b.mention.role_id}`, n);
        }
      }
    }
    for (const mem of ch.members) names.set(mem.user_id, mem.display_name);
    return {
      me: myId,
      agentName: profile?.name || DEFAULT_AGENT_NAME,
      agentHasIcon: profile?.has_icon ?? false,
      iconVersion: 0,
      names,
    };
    // namesTick: ディレクトリ・ロールの名前が分かったら作り直す。
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [ch.messages, ch.thread, ch.members, myId, profile, namesTick]);

  const send = React.useCallback(
    async (blocks: MessageBlock[], opts: { agentPrivate: boolean }, parentId: string | null): Promise<boolean> => {
      if (!activeId) return false;
      try {
        const m = await postMessage(activeId, { blocks, parent_id: parentId, agent_private: opts.agentPrivate });
        apply(m);
        if (mentionsAgent(blocks)) expectAgent(m);
        return true;
      } catch (e) {
        toast({ title: "送信できませんでした", description: errorText(e), variant: "destructive" });
        return false;
      }
    },
    [activeId, apply, expectAgent],
  );

  const actions: MessageActions = React.useMemo(
    () => ({
      onReact: (m: MessageView, emoji: string, on: boolean) => {
        reactTo(m.id, emoji, on)
          .then(apply)
          .catch((e: unknown) =>
            toast({ title: "リアクションできませんでした", description: errorText(e), variant: "destructive" }),
          );
      },
      onOpenThread: (m: MessageView) => go({ t: m.id }),
      onEdit: async (m: MessageView, blocks: MessageBlock[]) => {
        try {
          apply(await editMessage(m.id, blocks));
          return true;
        } catch (e) {
          toast({ title: "編集できませんでした", description: errorText(e), variant: "destructive" });
          return false;
        }
      },
      onDelete: async (m: MessageView) => {
        try {
          await deleteMessage(m.id);
          apply(await getMessage(m.id));
        } catch (e) {
          toast({ title: "削除できませんでした", description: errorText(e), variant: "destructive" });
        }
      },
      onAgentReplyChanged: () => void refreshAgentReplies(),
    }),
    [apply, refreshAgentReplies, go],
  );

  // 狭い画面の先頭の操作: アプリのメニュー（スマホ・シェルの浮いたボタンの代わり）とチャンネル一覧。
  const openList = (
    <>
      <span className="shrink-0 md:hidden">
        <MenuMarkButton onClick={() => setMobileOpen(true)} />
      </span>
      <button
        type="button"
        onClick={() => setListOpen(true)}
        aria-label="チャンネル一覧を開く"
        className="flex size-9 shrink-0 items-center justify-center rounded-lg text-muted-foreground outline-none transition-colors hover:bg-accent hover:text-foreground focus-visible:ring-2 focus-visible:ring-ring lg:hidden"
      >
        <PanelLeft className="size-[18px]" aria-hidden />
      </button>
    </>
  );

  const list = (drawer: boolean) => (
    <ChannelListPane
      channels={channels}
      activeId={activeId}
      view={view}
      drawer={drawer}
      unreadMentions={unreadMentions}
      canSeeApprovals={canApprovals}
      onSelect={(id) => {
        setListOpen(false);
        go({ c: id, v: "channel" });
      }}
      onView={(v) => {
        setListOpen(false);
        go({ v });
      }}
      onCreateChannel={() => setCreateOpen(true)}
      onBrowse={() => setBrowseOpen(true)}
      onNewDm={() => setDmOpen(true)}
    />
  );

  const added = (c: ChannelView) => {
    setChannels((prev) => (prev ? [c, ...prev.filter((x) => x.id !== c.id)] : [c]));
    go({ c: c.id, v: "channel" });
    void loadChannels();
  };

  const jump = (channelId: string, messageId: string, parentId: string | null) =>
    go({ c: channelId, t: parentId ?? messageId, v: "channel" });

  const composerBlocked: string | null = ch.left
    ? "このチャンネルから外れました"
    : !active
      ? "読み込んでいます"
      : active.archived
        ? "アーカイブされたチャンネルには投稿できません"
        : !active.joined
          ? "参加すると投稿できます"
          : !active.can_post
            ? "このチャンネルに投稿できるのは管理者と投稿者だけです"
            : null;

  let main: React.ReactNode;
  if (listError && channels === null) {
    main = (
      <div className="flex min-w-0 flex-1 flex-col">
        <div className="flex h-14 items-center px-2 lg:hidden">{openList}</div>
        <div className="flex flex-1 items-center justify-center p-8">
          <EmptyState
            icon={AlertCircle}
            title={listError.status === 503 ? "メッセージは利用できません" : "メッセージを読み込めませんでした"}
            description={listError.message}
            action={
              <Button variant="outline" size="sm" onClick={() => void loadChannels()}>
                再読み込み
              </Button>
            }
          />
        </div>
      </div>
    );
  } else if (view === "search") {
    main = <SearchView leading={openList} onClose={() => go({ v: "channel" })} onJump={jump} />;
  } else if (view === "mentions") {
    main = <MentionsView leading={openList} onJump={jump} onChanged={notifyMentionsChanged} />;
  } else if (view === "approvals") {
    main = (
      <div className="flex h-full min-w-0 flex-1 flex-col bg-background">
        <header className="shiki-dash-bottom flex h-14 shrink-0 items-center gap-2 pl-2 pr-3 lg:px-5">
          {openList}
          <ShieldCheck className="size-4 text-muted-foreground" aria-hidden />
          <h1 className="flex-1 text-[15px] font-semibold">閲覧の申請と承認</h1>
        </header>
        <div className="scrollbar-subtle min-h-0 flex-1 overflow-y-auto">
          <div className="mx-auto w-full max-w-3xl px-4 py-4 sm:px-5">
            <p className="mb-3 text-[12.5px] leading-relaxed text-muted-foreground">
              監査担当者が DM・非公開チャンネルを読むには、指名された承認者の承認が要ります。読まれた本人には通知されず、
              閲覧は監査ログに残ります。申請者本人は承認できません。
            </p>
            <AccessRequestList me={myId} />
          </div>
        </div>
      </div>
    );
  } else if (!activeId || (!active && !ch.error && (ch.loading || channels === null))) {
    main =
      channels !== null && channels.length === 0 && !activeId ? (
        <div className="flex min-w-0 flex-1 flex-col">
          <div className="flex h-14 items-center px-2 lg:hidden">{openList}</div>
          <div className="flex flex-1 items-center justify-center p-8">
            <EmptyState
              seasonal
              icon={MessageSquareText}
              title="まだチャンネルがありません"
              description="チャンネルを作るか、公開チャンネルに参加すると、ここに会話が並びます。"
              action={
                <div className="flex flex-wrap justify-center gap-2">
                  <Button size="sm" onClick={() => setCreateOpen(true)}>
                    チャンネルを作成
                  </Button>
                  <Button size="sm" variant="outline" onClick={() => setBrowseOpen(true)}>
                    公開チャンネルを探す
                  </Button>
                </div>
              }
            />
          </div>
        </div>
      ) : (
        <div className="flex min-w-0 flex-1 items-center justify-center gap-2 text-[12.5px] text-muted-foreground" aria-busy="true">
          <Loader2 className="size-4 animate-spin" aria-hidden />
          読み込んでいます
        </div>
      );
  } else if (ch.error || !active) {
    main = (
      <div className="flex min-w-0 flex-1 flex-col">
        <div className="flex h-14 items-center px-2 lg:hidden">{openList}</div>
        <div className="flex flex-1 items-center justify-center p-8">
          <EmptyState icon={AlertCircle} title="このチャンネルを開けません" description={ch.error ?? undefined} />
        </div>
      </div>
    );
  } else {
    main = (
      <div className="flex min-w-0 flex-1 flex-col" data-testid="channel-view">
        <ChannelHeader
          channel={active}
          members={ch.members}
          leading={openList}
          onOpenSettings={(tab) => setSettings({ open: true, tab })}
          onHideDm={() => {
            hideDm(active.id)
              .then(async () => {
                const next = await loadChannels();
                const first = next?.find((c) => c.id !== active.id);
                go({ c: first?.id ?? null });
              })
              .catch((e: unknown) => toast({ title: "隠せませんでした", description: errorText(e), variant: "destructive" }));
          }}
        />
        {ch.left ? (
          <div className="flex items-center gap-2 border-b border-border/60 bg-muted/40 px-5 py-2 text-[12.5px]" role="status">
            <LogOut className="size-4 text-muted-foreground" aria-hidden />
            <span className="flex-1">このチャンネルから外れました。新しい発言は届きません。</span>
            <Button size="sm" variant="ghost" onClick={() => void loadChannels().then(() => go({ c: null }))}>
              一覧に戻る
            </Button>
          </div>
        ) : ch.stream === "reconnecting" ? (
          <div className="flex items-center gap-2 border-b border-border/60 bg-muted/30 px-5 py-1.5 text-[12px] text-muted-foreground" role="status">
            <WifiOff className="size-3.5" aria-hidden />
            接続が切れました。再接続しています…
          </div>
        ) : null}

        <div ref={scroller} className="scrollbar-subtle min-h-0 flex-1 overflow-y-auto" aria-label="発言" role="log">
          {ch.loading && ch.messages.length === 0 ? (
            <div className="flex h-full items-center justify-center gap-2 text-[12.5px] text-muted-foreground" aria-busy="true">
              <Loader2 className="size-4 animate-spin" aria-hidden />
              読み込んでいます
            </div>
          ) : ch.messages.length === 0 ? (
            <div className="flex h-full items-center justify-center p-8">
              <EmptyState
                seasonal
                icon={MessageSquareText}
                title={active.kind === "dm" ? `${active.name} との会話を始めましょう` : `「${active.name}」を始めましょう`}
                description="最初の発言を書くと、参加者に届きます。ドライブの文書もそのまま共有できます。"
              />
            </div>
          ) : (
            // 発言が少ないときも最新が下端に貼り付くよう、下寄せで積む。
            <div className="flex min-h-full flex-col justify-end">
              <div ref={sentinel} aria-hidden className="h-px" />
              {ch.loadingOlder ? (
                <div className="flex items-center justify-center gap-2 py-2 text-[12px] text-muted-foreground">
                  <Loader2 className="size-3.5 animate-spin" aria-hidden />
                  以前の発言を読み込んでいます
                </div>
              ) : !ch.hasMore ? (
                <ChannelIntro channel={active} onOpenSettings={(tab) => setSettings({ open: true, tab })} />
              ) : null}
              <MessageList
                messages={ch.messages}
                members={ch.members}
                canModerate={active.is_admin}
                canReact={active.joined && !active.archived && !ch.left}
                firstUnreadId={firstUnreadId}
                activeThreadId={ch.threadId}
                agentReplies={ch.agentReplies.filter((r) => ch.messages.some((m) => m.id === r.trigger_message_id))}
                actions={actions}
              />
            </div>
          )}
        </div>

        <div>
          {/* エージェントの作成中の表示。高さを常に確保し、出入りで本文が跳ねないようにする。 */}
          <div className="flex h-[18px] items-center px-6" aria-live="polite">
            {ch.agentPending ? (
              <p className="flex items-center gap-1.5 text-[11.5px] text-muted-foreground">
                <span className="inline-block size-1.5 animate-pulse rounded-full" style={{ backgroundColor: seasonVar(1) }} aria-hidden />
                {display.agentName} が回答を作成中…
              </p>
            ) : null}
          </div>
          {!active.joined && !active.archived && active.kind === "public" && !ch.left ? (
            <div className="px-4 pb-4 pt-1">
              <div className="flex flex-wrap items-center gap-3 rounded-xl border border-border/60 bg-card/40 px-4 py-3">
                <p className="min-w-0 flex-1 text-[12.5px] text-muted-foreground">
                  公開チャンネルは参加しなくても読めます。参加すると一覧に出て、投稿できます。
                </p>
                <Button
                  size="sm"
                  loading={joining}
                  onClick={async () => {
                    setJoining(true);
                    try {
                      const c = await joinChannel(active.id);
                      ch.setChannel(c);
                      added(c);
                      void ch.refreshMembers();
                    } catch (e) {
                      toast({ title: "参加できませんでした", description: errorText(e), variant: "destructive" });
                    } finally {
                      setJoining(false);
                    }
                  }}
                >
                  参加する
                </Button>
              </div>
            </div>
          ) : (
            // key で宛先ごとに作り直す（持ち越すと、別チャンネルへ下書きと添付がそのまま飛ぶ）。
            <Composer
              key={active.id}
              placeholder={active.kind === "dm" ? `${active.name} へメッセージを送る` : `#${active.name} へメッセージを送る`}
              members={ch.members}
              allowChannelMention={active.kind !== "dm"}
              disabledReason={composerBlocked}
              hint="発言は監査の対象です。共有した文書は、相手の権限に従って表示されます。"
              onSubmit={(blocks, opts) => send(blocks, opts, null)}
            />
          )}
        </div>
      </div>
    );
  }

  const threadOpen = view === "channel" && active && ch.threadId;

  return (
    <MessagingDisplayContext.Provider value={display}>
      <div className="relative flex h-full min-h-0 w-full overflow-hidden">
        <div className="hidden lg:flex">{list(false)}</div>

        {/* 狭い画面では一覧をドロワで出す（本体は常に 1 カラム）。 */}
        <Sheet open={listOpen} onOpenChange={setListOpen}>
          <SheetContent side="left" className="w-[280px] max-w-[85vw] p-0">
            <SheetTitle className="sr-only">チャンネル一覧</SheetTitle>
            {list(true)}
          </SheetContent>
        </Sheet>

        {main}

        {threadOpen && active ? (
          <div className={cn("absolute inset-0 z-30 flex justify-end bg-background/40 sm:inset-y-0 sm:left-auto sm:shadow-lg", "xl:static xl:z-auto xl:bg-transparent xl:shadow-none")}>
            <ThreadPane
              channel={active}
              threadId={ch.threadId ?? ""}
              items={ch.thread}
              loading={ch.threadLoading}
              error={ch.threadError}
              members={ch.members}
              agentReplies={ch.agentReplies.filter((r) => ch.thread.some((m) => m.id === r.trigger_message_id))}
              composerDisabledReason={composerBlocked}
              actions={{ ...actions, onOpenThread: undefined }}
              onClose={() => go({ t: null })}
              onReply={(blocks, opts) => send(blocks, opts, ch.threadId)}
            />
          </div>
        ) : null}

        <CreateChannelDialog open={createOpen} onOpenChange={setCreateOpen} onCreated={added} />
        <BrowseChannelsDialog
          open={browseOpen}
          onOpenChange={setBrowseOpen}
          onOpen={(c) => go({ c: c.id, v: "channel" })}
          onJoined={added}
        />
        <NewDmDialog open={dmOpen} onOpenChange={setDmOpen} onOpened={added} />
        {active ? (
          <ChannelSettingsDialog
            open={settings.open}
            onOpenChange={(open) => setSettings((s) => ({ ...s, open }))}
            tab={active.kind === "dm" ? "notify" : settings.tab}
            onTabChange={(tab) => setSettings((s) => ({ ...s, tab }))}
            channel={active}
            members={ch.members}
            onChannelChanged={(c) => {
              if (c) {
                ch.setChannel(c);
                setChannels((prev) => prev?.map((x) => (x.id === c.id ? c : x)) ?? prev);
              } else void ch.refreshChannel();
              void loadChannels();
            }}
            onMembersChanged={(m) => (m ? ch.setMembers(m) : void ch.refreshMembers())}
            onLeft={() => {
              void loadChannels().then((next) => {
                // 公開チャンネルは退出しても読めるので開いたまま、非公開は一覧に戻る。
                if (active.kind === "public") void ch.refreshChannel();
                else go({ c: next?.[0]?.id ?? null });
              });
            }}
          />
        ) : null}
      </div>
    </MessagingDisplayContext.Provider>
  );
}

const INTRO_ACTION =
  "inline-flex h-8 items-center gap-1.5 rounded-full border border-border/70 bg-card px-3 text-[12px] font-medium text-foreground/85 outline-none transition-colors hover:border-ring/40 hover:bg-accent focus-visible:ring-2 focus-visible:ring-ring";

/// 履歴の先頭（これ以上古い発言が無いとき）に出す、チャンネルの紹介。何の場所か・誰がいるか・次に何をするかを示す。
function ChannelIntro({ channel, onOpenSettings }: { channel: ChannelView; onOpenSettings: (tab: SettingsTab) => void }) {
  const isDm = channel.kind === "dm";
  const kindLabel = isDm
    ? "ダイレクトメッセージ"
    : channel.kind === "private"
      ? "非公開チャンネル"
      : channel.announcement
        ? "アナウンス専用チャンネル"
        : "公開チャンネル";
  const lead = isDm
    ? `${channel.name} との DM の始まりです。ここでの発言は 2 人だけが読めます。`
    : channel.kind === "private"
      ? "招待された参加者だけが読めるチャンネルです。"
      : channel.announcement
        ? "管理者と投稿者だけが投稿でき、参加者は読んでリアクションできます。"
        : "組織のメンバーなら誰でも読めて、参加すると投稿できます。";
  return (
    <div className="px-5 pb-5 pt-10" data-testid="channel-intro">
      {isDm ? (
        <UserAvatar userId={channel.dm_peer ?? channel.id} name={channel.name} size="lg" className="size-14 rounded-[16px] text-[20px]" />
      ) : (
        <ChannelTile channelId={channel.id} kind={channel.kind} announcement={channel.announcement} size="lg" />
      )}
      <h2 className="mt-3 text-[21px] font-semibold tracking-tight text-foreground text-balance">{channel.name}</h2>
      <p className="mt-0.5 text-[12px] font-medium text-muted-foreground">
        {kindLabel}
        {!isDm ? ` · 参加者 ${channel.member_count} 人` : ""}
      </p>
      <p className="mt-2 max-w-[62ch] text-[13px] leading-relaxed text-foreground/80">
        {channel.topic ? `${channel.topic}${/[。．！？.!?]$/.test(channel.topic) ? "" : "。"}` : ""}
        {lead}
      </p>
      {channel.joined && !channel.archived ? (
        <div className="mt-3.5 flex flex-wrap gap-2">
          {!isDm ? (
            <button type="button" className={INTRO_ACTION} onClick={() => onOpenSettings("members")}>
              <UserPlus className="size-3.5 text-muted-foreground" aria-hidden />
              参加者を招待
            </button>
          ) : null}
          <button type="button" className={INTRO_ACTION} onClick={() => onOpenSettings("notify")}>
            <Bell className="size-3.5 text-muted-foreground" aria-hidden />
            通知の受け方
          </button>
          {!isDm && channel.is_admin ? (
            <button type="button" className={INTRO_ACTION} onClick={() => onOpenSettings("about")}>
              <Settings2 className="size-3.5 text-muted-foreground" aria-hidden />
              チャンネルの設定
            </button>
          ) : null}
        </div>
      ) : null}
    </div>
  );
}
