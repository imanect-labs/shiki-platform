"use client";

/// 職員間メッセージ（Phase 14 Stage 4）の 3 ペイン画面。
///
/// **バックエンド未実装の UI モック。** 状態はこのコンポーネントに閉じており、API も SSE も
/// 呼ばない。実装時は `channels` の更新をサーバ由来のイベントへ差し替える。

import * as React from "react";
import { MessageSquareText, PanelLeft } from "lucide-react";

import { cn } from "@/lib/utils";
import { seasonVar } from "@/lib/season";
import { EmptyState } from "@/components/ui/empty-state";
import { Sheet, SheetContent, SheetTitle } from "@/components/ui/sheet";
import {
  CHANNELS,
  ME,
  channelTitle,
  findMember,
  plainText,
  type Block,
  type Channel,
  type Message,
} from "@/lib/messages-mock";
import { ChannelListPane } from "./channel-list-pane";
import { ChannelHeader } from "./channel-header";
import { CreateChannelDialog } from "./create-channel-dialog";
import { Composer } from "./composer";
import { MessageList } from "./message-list";
import { SearchView } from "./search-view";
import { ThreadPane } from "./thread-pane";

/// いま送った発言に付ける時刻（クライアント操作なので現在時刻で安全に採番できる）。
function nowTime(): string {
  const d = new Date();
  return `${String(d.getHours()).padStart(2, "0")}:${String(d.getMinutes()).padStart(2, "0")}`;
}

let localSeq = 0;
function newMessage(authorId: string, blocks: Block[], extra: Partial<Message> = {}): Message {
  localSeq += 1;
  return {
    id: `local-${localSeq}`,
    authorId,
    dayLabel: "今日",
    time: nowTime(),
    blocks,
    reactions: [],
    replies: [],
    ...extra,
  };
}

/// リアクションの付け外し（同じ絵文字を再度押すと外れ、誰も居なくなったら消える）。
function toggleReaction(msg: Message, emoji: string, viewerId: string): Message {
  const existing = msg.reactions.find((r) => r.emoji === emoji);
  if (!existing) return { ...msg, reactions: [...msg.reactions, { emoji, by: [viewerId] }] };
  const by = existing.by.includes(viewerId)
    ? existing.by.filter((id) => id !== viewerId)
    : [...existing.by, viewerId];
  return {
    ...msg,
    reactions: msg.reactions
      .map((r) => (r.emoji === emoji ? { ...r, by } : r))
      .filter((r) => r.by.length > 0),
  };
}

/// スレッドの親／返信のどちらにも同じ更新をかけるためのヘルパ。
function mapMessages(messages: Message[], id: string, fn: (m: Message) => Message): Message[] {
  return messages.map((m) => {
    if (m.id === id) return fn(m);
    if (m.replies.some((r) => r.id === id)) {
      return { ...m, replies: m.replies.map((r) => (r.id === id ? fn(r) : r)) };
    }
    return m;
  });
}

/// シキ（AI）の回答文。モックなので固定の要約を返すが、**参照した文脈は
/// 照会者が読めるものだけ**を並べる（PIT-63 の説明に使う）。
function aiAnswer(channel: Channel, root: Message, viewerId: string) {
  const readable = channel.messages.length;
  const body =
    channel.id === "ch-soumu"
      ? "このチャンネルの直近のやり取りでは、2027年度予算案が共有され、情報システム部のサーバ更改分が上乗せ済みです。備品費の内訳だけ未確定で、営業部への共有は部長確認待ちになっています。次の論点は「営業部への共有可否」と「備品費の内訳」の 2 点です。"
      : channel.id === "ch-all"
        ? "直近の周知は 3 件です。就業規則の改定版（10月1日施行・第3章と第7章が変更）、9月5日(木) 14:00 の防災訓練、今夜22時からのネットワーク定期メンテナンスです。既存契約への第7章適用には経過措置があります。"
        : `このチャンネルの直近 ${readable} 件を読みました。${plainText(root).slice(0, 40)}… に関するやり取りが中心です。`;

  const label = channel.kind === "dm" ? channelTitle(channel, viewerId) : `#${channel.name}`;
  const sources = [
    `${label} の直近 ${readable} 件（あなたが読める発言のみ）`,
    ...channel.messages
      .flatMap((m) => m.blocks)
      .filter((b) => b.kind === "file_ref")
      .map((b) => (b.kind === "file_ref" ? b.fileId : ""))
      .map((id) => (id === "f-budget" ? "2027年度予算案.xlsx" : id === "f-rule" ? "就業規則_2027改定版.docx" : "情シス定例_8月報告.pptx"))
      .filter((name, i, arr) => arr.indexOf(name) === i)
      .filter((name) => !(name === "2027年度予算案.xlsx" && viewerId === "suzuki"))
      .map((name) => `ドライブの文書: ${name}`),
  ];

  return { body, sources };
}

export function MessagesWorkspace() {
  const [channels, setChannels] = React.useState<Channel[]>(CHANNELS);
  const [viewerId, setViewerId] = React.useState<string>(ME);
  const [activeId, setActiveId] = React.useState<string>("ch-soumu");
  const [threadId, setThreadId] = React.useState<string | null>(null);
  const [searchOpen, setSearchOpen] = React.useState(false);
  const [createOpen, setCreateOpen] = React.useState(false);
  const [listOpen, setListOpen] = React.useState(false);
  const [typingBy, setTypingBy] = React.useState<string | null>(null);
  const scroller = React.useRef<HTMLDivElement>(null);
  const timers = React.useRef<ReturnType<typeof setTimeout>[]>([]);

  React.useEffect(() => {
    const list = timers.current;
    return () => list.forEach(clearTimeout);
  }, []);

  const later = (fn: () => void, ms: number) => {
    timers.current.push(setTimeout(fn, ms));
  };

  const visible = channels.filter((c) => c.memberIds.includes(viewerId));
  const active = visible.find((c) => c.id === activeId) ?? visible[0] ?? null;
  const thread = active?.messages.find((m) => m.id === threadId) ?? null;

  // 表示者を切り替えたとき、その人が参加していないチャンネルを開いたままにしない。
  React.useEffect(() => {
    if (active && active.id !== activeId) setActiveId(active.id);
    if (threadId && !active?.messages.some((m) => m.id === threadId)) setThreadId(null);
  }, [active, activeId, threadId]);

  // チャンネルを開いたら最下部から読み始める（未読ラインは残したまま）。
  React.useEffect(() => {
    const el = scroller.current;
    if (el) el.scrollTop = el.scrollHeight;
  }, [activeId, viewerId]);

  const patch = (channelId: string, fn: (c: Channel) => Channel) =>
    setChannels((prev) => prev.map((c) => (c.id === channelId ? fn(c) : c)));

  const openChannel = (id: string) => {
    setActiveId(id);
    setThreadId(null);
    setSearchOpen(false);
    setListOpen(false);
    // 既読化は `read_state.last_read_at` の更新に相当する。少し置いてから消し、
    // 「未読だった場所」を利用者が目で追えるようにする。
    later(() => patch(id, (c) => ({ ...c, firstUnreadId: undefined })), 1600);
  };

  const react = (messageId: string, emoji: string) => {
    if (!active) return;
    patch(active.id, (c) => ({
      ...c,
      messages: mapMessages(c.messages, messageId, (m) => toggleReaction(m, emoji, viewerId)),
    }));
  };

  const send = (blocks: Block[]) => {
    if (!active) return;
    const mine = newMessage(viewerId, blocks);
    patch(active.id, (c) => ({ ...c, messages: [...c.messages, mine] }));
    later(() => {
      const el = scroller.current;
      if (el) el.scrollTo({ top: el.scrollHeight, behavior: "smooth" });
    }, 30);

    // 相手が返す演出（配信は SSE になる箇所）。DM と、自分以外の参加者が居る場で 1 回だけ。
    const other = active.memberIds.find((m) => m !== viewerId && m !== "shiki");
    if (!other) return;
    later(() => setTypingBy(other), 1400);
    later(() => {
      setTypingBy(null);
      patch(active.id, (c) => ({
        ...c,
        messages: [
          ...c.messages,
          newMessage(other, [{ kind: "text", text: "確認しました。ありがとうございます。" }]),
        ],
      }));
      const el = scroller.current;
      if (el) el.scrollTo({ top: el.scrollHeight + 200, behavior: "smooth" });
    }, 3600);
  };

  const reply = (blocks: Block[]) => {
    if (!active || !threadId) return;
    patch(active.id, (c) => ({
      ...c,
      messages: mapMessages(c.messages, threadId, (m) => ({
        ...m,
        replies: [...m.replies, newMessage(viewerId, blocks)],
      })),
    }));
  };

  /// 発言の文脈でシキに聞く。**照会者の権限で読み直した発言だけ**が文脈になる（PIT-63）。
  /// 回答はスレッドに積み、生成中は 1 文字ずつ埋めて「動いている」ことを示す。
  const askAi = (messageId: string) => {
    if (!active) return;
    setThreadId(messageId);
    const root = active.messages.find((m) => m.id === messageId);
    if (!root) return;
    const { body, sources } = aiAnswer(active, root, viewerId);

    const question = newMessage(viewerId, [
      { kind: "text", text: "この流れの要点と、次に決めることを教えてください。" },
    ]);
    const answerId = `local-ai-${Date.now()}`;
    const answer: Message = {
      ...newMessage("shiki", [{ kind: "text", text: "" }], { ai: true, pending: true }),
      id: answerId,
    };

    patch(active.id, (c) => ({
      ...c,
      messages: mapMessages(c.messages, messageId, (m) => ({
        ...m,
        replies: [...m.replies, question, answer],
      })),
    }));

    // 1 文字ずつ埋める（生成中の体感を出すためだけの演出）。
    let i = 0;
    const step = () => {
      i = Math.min(i + 2, body.length);
      const done = i >= body.length;
      patch(active.id, (c) => ({
        ...c,
        messages: mapMessages(c.messages, messageId, (m) => ({
          ...m,
          replies: m.replies.map((r) =>
            r.id === answerId
              ? {
                  ...r,
                  blocks: [{ kind: "text", text: body.slice(0, i) }],
                  pending: !done,
                  sources: done ? sources : undefined,
                }
              : r,
          ),
        })),
      }));
      if (!done) later(step, 26);
    };
    later(step, 700);
  };

  const createChannel = (input: {
    kind: "public" | "private";
    name: string;
    topic: string;
    memberIds: string[];
  }) => {
    const id = `ch-local-${Date.now()}`;
    setChannels((prev) => [
      ...prev,
      { id, kind: input.kind, name: input.name, topic: input.topic || undefined, memberIds: input.memberIds, messages: [] },
    ]);
    setActiveId(id);
    setThreadId(null);
    setSearchOpen(false);
  };

  const list = (
    <ChannelListPane
      channels={channels}
      activeId={active?.id ?? null}
      viewerId={viewerId}
      searchOpen={searchOpen}
      onSelect={openChannel}
      onOpenSearch={() => {
        setSearchOpen(true);
        setListOpen(false);
      }}
      onCreateChannel={() => setCreateOpen(true)}
    />
  );

  return (
    <div className="flex h-full min-h-0 w-full">
      <div className="hidden md:flex">{list}</div>

      {/* 狭い画面では一覧をドロワで出す（本体は常に 1 カラム）。 */}
      <Sheet open={listOpen} onOpenChange={setListOpen}>
        <SheetContent side="left" className="w-[236px] p-0">
          <SheetTitle className="sr-only">チャンネル一覧</SheetTitle>
          {list}
        </SheetContent>
      </Sheet>

      {searchOpen ? (
        <SearchView
          channels={channels}
          viewerId={viewerId}
          onClose={() => setSearchOpen(false)}
          onJump={(channelId, messageId) => {
            setSearchOpen(false);
            setActiveId(channelId);
            setThreadId(messageId);
          }}
        />
      ) : !active ? (
        <div className="flex min-w-0 flex-1 items-center justify-center p-8">
          <EmptyState
            seasonal
            icon={MessageSquareText}
            title="チャンネルがありません"
            description="左上の＋からチャンネルを作成すると、ここに会話が並びます。"
          />
        </div>
      ) : (
        <div className="flex min-w-0 flex-1 flex-col">
          <div className="flex items-center">
            <button
              type="button"
              onClick={() => setListOpen(true)}
              aria-label="チャンネル一覧を開く"
              className="ml-2 flex size-9 shrink-0 items-center justify-center rounded-lg text-muted-foreground outline-none transition-colors hover:bg-accent hover:text-foreground focus-visible:ring-2 focus-visible:ring-ring md:hidden"
            >
              <PanelLeft className="size-[18px]" aria-hidden />
            </button>
            <div className="min-w-0 flex-1">
              <ChannelHeader
                channel={active}
                viewerId={viewerId}
                onViewerChange={setViewerId}
                onInvite={() => setCreateOpen(true)}
              />
            </div>
          </div>

          <div ref={scroller} className="scrollbar-subtle min-h-0 flex-1 overflow-y-auto">
            {active.messages.length === 0 ? (
              <div className="flex h-full items-center justify-center p-8">
                <EmptyState
                  seasonal
                  icon={MessageSquareText}
                  title={`「${active.name}」を始めましょう`}
                  description="最初の発言を書くと、参加者に届きます。ドライブの文書もそのまま共有できます。"
                />
              </div>
            ) : (
              // 発言が少ないときも最新が下端に貼り付くよう、下寄せで積む。
              <div className="flex min-h-full flex-col justify-end">
              <MessageList
                messages={active.messages}
                viewerId={viewerId}
                firstUnreadId={active.firstUnreadId}
                activeThreadId={threadId}
                onToggleReaction={react}
                onOpenThread={setThreadId}
                onAskAi={askAi}
              />
              </div>
            )}
          </div>

          <div>
            {/* 入力中の表示。高さを常に確保し、出入りで本文が跳ねないようにする。 */}
            <div className="flex h-[18px] items-center px-6" aria-live="polite">
              {typingBy ? (
                <p className="flex items-center gap-1.5 text-[11.5px] text-muted-foreground">
                  <span
                    className="inline-block size-1.5 animate-pulse rounded-full"
                    style={{ backgroundColor: seasonVar(1) }}
                    aria-hidden
                  />
                  {findMember(typingBy).name} が入力中…
                </p>
              ) : null}
            </div>
            <Composer
              placeholder={`${active.kind === "dm" ? "" : "#"}${active.name} へメッセージを送る`}
              channelMemberIds={active.memberIds}
              onSend={send}
            />
          </div>
        </div>
      )}

      {active && thread ? (
        <div
          className={cn(
            "absolute inset-y-0 right-0 z-30 shadow-lg",
            "lg:static lg:z-auto lg:shadow-none",
          )}
        >
          <ThreadPane
            channel={active}
            root={thread}
            viewerId={viewerId}
            onClose={() => setThreadId(null)}
            onReply={reply}
            onToggleReaction={react}
          />
        </div>
      ) : null}

      <CreateChannelDialog
        open={createOpen}
        viewerId={viewerId}
        onOpenChange={setCreateOpen}
        onCreate={createChannel}
      />
    </div>
  );
}
