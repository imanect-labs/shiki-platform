"use client";

/// 開いているチャンネルの状態（発言・スレッド・参加者・自分だけの回答）と配信（SSE）の購読。
///
/// 配信の `message` は発言の追加・変更（編集・削除・リアクション）で、id で上書きする。返信が届いたら
/// 親の返信数を引き直す（親の行は配信に乗らないため）。`leave` を受けたら購読を止め、画面に知らせる。

import * as React from "react";

import {
  getChannel,
  getMessage,
  getThread,
  listAgentReplies,
  listMembers,
  listMessages,
  MessagingError,
  subscribeChannel,
  type AgentReplyView,
  type ChannelView,
  type MemberView,
  type MessageView,
} from "@/lib/messaging-api";

const PAGE = 50;

/// id で上書き（無ければ足して時刻順に並べ直す）。
function upsert(list: MessageView[], m: MessageView): MessageView[] {
  const i = list.findIndex((x) => x.id === m.id);
  if (i >= 0) {
    // 古い版で新しい版を上書きしない（配信と操作の応答が前後しても戻らない）。
    if (list[i].updated_at > m.updated_at) return list;
    const next = list.slice();
    next[i] = m;
    return next;
  }
  const next = [...list, m];
  next.sort((a, b) => (a.created_at < b.created_at ? -1 : a.created_at > b.created_at ? 1 : 0));
  return next;
}

export type StreamStatus = "connecting" | "open" | "reconnecting";

export function useChannel(channelId: string | null, initial: ChannelView | null) {
  const [channel, setChannel] = React.useState<ChannelView | null>(initial);
  const [messages, setMessages] = React.useState<MessageView[]>([]);
  const [loading, setLoading] = React.useState(false);
  const [error, setError] = React.useState<string | null>(null);
  const [hasMore, setHasMore] = React.useState(false);
  const [loadingOlder, setLoadingOlder] = React.useState(false);
  const [members, setMembers] = React.useState<MemberView[]>([]);
  const [agentReplies, setAgentReplies] = React.useState<AgentReplyView[]>([]);
  const [left, setLeft] = React.useState(false);
  const [stream, setStream] = React.useState<StreamStatus>("connecting");
  const [threadId, setThreadId] = React.useState<string | null>(null);
  const [thread, setThread] = React.useState<MessageView[]>([]);
  const [threadLoading, setThreadLoading] = React.useState(false);
  const [threadError, setThreadError] = React.useState<string | null>(null);
  /// 自分のエージェントを呼んだ発言（応答＝その後のエージェントの発言・自分だけの回答が届くまで「作成中」）。
  const [agentCall, setAgentCall] = React.useState<{ trigger: string; at: string } | null>(null);

  const threadRef = React.useRef<string | null>(null);
  threadRef.current = threadId;
  const channelRef = React.useRef(channelId);
  channelRef.current = channelId;
  const parentTimers = React.useRef(new Map<string, ReturnType<typeof setTimeout>>());
  const agentTimer = React.useRef<ReturnType<typeof setTimeout> | null>(null);

  const refreshChannel = React.useCallback(async () => {
    if (!channelId) return;
    try {
      const c = await getChannel(channelId);
      if (channelRef.current === channelId) setChannel(c);
    } catch (e) {
      if (e instanceof MessagingError && e.status === 404 && channelRef.current === channelId) setLeft(true);
    }
  }, [channelId]);

  const refreshMembers = React.useCallback(async () => {
    if (!channelId) return;
    try {
      const m = await listMembers(channelId);
      if (channelRef.current === channelId) setMembers(m);
    } catch {
      // 参加者が読めない（外された）なら leave で知らせる。ここでは黙る。
    }
  }, [channelId]);

  const refreshAgentReplies = React.useCallback(async () => {
    if (!channelId) return;
    try {
      const r = await listAgentReplies(channelId);
      if (channelRef.current === channelId) setAgentReplies(r);
    } catch {
      // 出せないだけ（チャンネル自体は読める）。
    }
  }, [channelId]);

  const settleAgent = React.useCallback(() => {
    setAgentCall(null);
    if (agentTimer.current) clearTimeout(agentTimer.current);
    agentTimer.current = null;
  }, []);

  /// 親の返信数・最終返信時刻を引き直す（返信が届くたびに呼ぶので少しまとめる）。
  const refreshParent = React.useCallback((parentId: string) => {
    const timers = parentTimers.current;
    const prev = timers.get(parentId);
    if (prev) clearTimeout(prev);
    timers.set(
      parentId,
      setTimeout(() => {
        timers.delete(parentId);
        getMessage(parentId)
          .then((p) => {
            setMessages((list) => (list.some((x) => x.id === p.id) ? upsert(list, p) : list));
            if (threadRef.current === p.id) setThread((t) => (t.length > 0 ? upsert(t, p) : t));
          })
          .catch(() => undefined);
      }, 300),
    );
  }, []);

  /// 発言の追加・変更を画面へ反映する（配信と、自分の操作の応答の両方から呼ぶ）。
  const apply = React.useCallback(
    (m: MessageView) => {
      if (!m.parent_id) {
        setMessages((list) => upsert(list, m));
        if (threadRef.current === m.id) setThread((t) => (t.length > 0 ? upsert(t, m) : t));
      } else {
        if (threadRef.current === m.parent_id) setThread((t) => upsert(t, m));
        refreshParent(m.parent_id);
      }
    },
    [refreshParent],
  );

  // チャンネルを開いたら読み込み、購読する。
  React.useEffect(() => {
    setChannel(initial && initial.id === channelId ? initial : null);
    setMessages([]);
    setMembers([]);
    setAgentReplies([]);
    setLeft(false);
    setError(null);
    setHasMore(false);
    setThreadId(null);
    setThread([]);
    settleAgent();
    if (!channelId) return;
    let active = true;
    setLoading(true);
    Promise.all([getChannel(channelId), listMessages(channelId, undefined, PAGE)])
      .then(([c, page]) => {
        if (!active) return;
        setChannel(c);
        setMessages(page.slice().reverse());
        setHasMore(page.length >= PAGE);
      })
      .catch((e: unknown) => {
        if (!active) return;
        if (e instanceof MessagingError && e.status === 404) setError("このチャンネルは見つからないか、読む権限がありません。");
        else setError(e instanceof Error ? e.message : String(e));
      })
      .finally(() => active && setLoading(false));
    void refreshMembers();
    void refreshAgentReplies();

    setStream("connecting");
    const close = subscribeChannel(channelId, {
      onMessage: (m) => active && apply(m),
      onAgentReply: () => {
        if (!active) return;
        void refreshAgentReplies();
      },
      onChannel: () => {
        if (!active) return;
        void refreshChannel();
        void refreshMembers();
      },
      onLeave: () => active && setLeft(true),
      onOpen: () => active && setStream("open"),
      onError: () => active && setStream("reconnecting"),
    });
    const timers = parentTimers.current;
    return () => {
      active = false;
      close();
      timers.forEach(clearTimeout);
      timers.clear();
    };
    // `initial` は開いた瞬間の表示用（変わっても読み直さない）。
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [channelId]);

  const loadOlder = React.useCallback(async () => {
    if (!channelId || loadingOlder || !hasMore) return;
    const oldest = messages[0];
    if (!oldest) return;
    setLoadingOlder(true);
    try {
      const page = await listMessages(channelId, oldest.created_at, PAGE);
      if (channelRef.current !== channelId) return;
      setMessages((list) => page.reduce((acc, m) => upsert(acc, m), list));
      setHasMore(page.length >= PAGE);
    } catch {
      // 次のスクロールで引き直す。
    } finally {
      setLoadingOlder(false);
    }
  }, [channelId, loadingOlder, hasMore, messages]);

  const openThread = React.useCallback(
    (parentId: string | null) => {
      setThreadId(parentId);
      setThread([]);
      setThreadError(null);
      if (!parentId || !channelId) return;
      setThreadLoading(true);
      getThread(channelId, parentId)
        .then((t) => {
          if (threadRef.current === parentId) setThread(t);
        })
        .catch((e: unknown) => {
          if (threadRef.current === parentId) setThreadError(e instanceof Error ? e.message : String(e));
        })
        .finally(() => setThreadLoading(false));
    },
    [channelId],
  );

  /// 自分のエージェントを呼んだ（応答が届くまで「作成中」を出す・届かなければ 2 分で消す）。
  /// 応答は投稿の応答より先に配信で届くことがあるので、「届いたか」は状態から導く。
  const expectAgent = React.useCallback((trigger: MessageView) => {
    setAgentCall({ trigger: trigger.id, at: trigger.created_at });
    if (agentTimer.current) clearTimeout(agentTimer.current);
    agentTimer.current = setTimeout(() => setAgentCall(null), 120_000);
  }, []);

  const agentPending =
    agentCall !== null &&
    !agentReplies.some((r) => r.trigger_message_id === agentCall.trigger) &&
    ![...messages, ...thread].some((m) => m.author.kind === "assistant" && m.created_at >= agentCall.at);

  return {
    channel,
    setChannel,
    messages,
    loading,
    error,
    hasMore,
    loadingOlder,
    loadOlder,
    members,
    setMembers,
    agentReplies,
    left,
    stream,
    threadId,
    thread,
    threadLoading,
    threadError,
    openThread,
    agentPending,
    expectAgent,
    apply,
    refreshChannel,
    refreshMembers,
    refreshAgentReplies,
  };
}
