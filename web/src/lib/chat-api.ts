/// チャットのクライアント側データ層（モック）。
///
/// main にはチャット backend（threads / SSE）が無く、チャットはクライアント側モックで動く。
/// 本ファイルは backend 実装が入るまでの差し替え点で、公開 API（型・関数シグネチャ）は
/// 本番相当に保ち、実装だけ localStorage ＋ `mock-assistant` に閉じている。backend が入ったら
/// この関数群の中身を fetch/SSE に置換すればよい（UI 側は無改修）。

"use client";

import * as React from "react";

import { newId } from "@/lib/chat-store";
import { mockReplyText, streamMockReply } from "@/lib/mock-assistant";

// ── content-block（将来の backend chat::ContentBlock と一致させる形）──────────
// モックは text ブロックのみ生成するが、UI は thinking/tool_call/citation/file_ref も
// 描画できるよう union を保つ（backend 実装時にそのまま拡張される）。

export type ContentBlock =
  | { type: "text"; text: string }
  | { type: "thinking"; text: string }
  | { type: "tool_call"; id: string; name: string; input: unknown }
  | { type: "tool_result"; tool_call_id: string; content: string }
  | {
      type: "citation";
      node_id: string;
      chunk_id: string;
      snippet: string;
      page?: number | null;
      heading_path?: string[];
      score: number;
    }
  | { type: "file_ref"; node_id: string; name: string };

export type ChatRole = "user" | "assistant" | "system" | "tool";

export type Thread = {
  id: string;
  title: string;
  createdAt: string;
  updatedAt: string;
};

export type Message = {
  id: string;
  role: ChatRole;
  content: ContentBlock[];
  createdAt: string;
};

export type Attachment = { node_id: string; name: string };

export type Citation = Extract<ContentBlock, { type: "citation" }>;

// ── localStorage 永続化（モックストア）────────────────────────────────

type StoredThread = Thread & { messages: Message[] };

const STORAGE_KEY = "shiki:mock-threads:v1";

let cache: StoredThread[] | null = null;
const threadListeners = new Set<() => void>();

function isBrowser(): boolean {
  return typeof window !== "undefined";
}

function readAll(): StoredThread[] {
  if (cache) return cache;
  if (!isBrowser()) return (cache = []);
  try {
    const raw = window.localStorage.getItem(STORAGE_KEY);
    const parsed = raw ? (JSON.parse(raw) as unknown) : [];
    cache = Array.isArray(parsed)
      ? parsed.filter(
          (t): t is StoredThread =>
            !!t &&
            typeof t === "object" &&
            typeof (t as StoredThread).id === "string" &&
            Array.isArray((t as StoredThread).messages),
        )
      : [];
  } catch {
    cache = [];
  }
  return cache;
}

function persist(next: StoredThread[]): void {
  cache = next;
  if (isBrowser()) {
    try {
      window.localStorage.setItem(STORAGE_KEY, JSON.stringify(next));
    } catch {
      /* 容量超過等は無視（in-memory cache で継続） */
    }
  }
  notifyThreadsChanged();
}

function toThread(t: StoredThread): Thread {
  return { id: t.id, title: t.title, createdAt: t.createdAt, updatedAt: t.updatedAt };
}

// ── スレッド一覧の購読（サイドバー履歴）────────────────────────────────

/// スレッド一覧が変わったことを購読者へ通知する（作成・更新時に呼ぶ）。
export function notifyThreadsChanged(): void {
  for (const l of threadListeners) l();
}

const DAY_MS = 86_400_000;
const GROUP_ORDER = ["今日", "昨日", "過去 7 日間", "それ以前"] as const;
export type ThreadGroupLabel = (typeof GROUP_ORDER)[number];

/// スレッドを更新日で「今日 / 昨日 / 過去 7 日間 / それ以前」に分ける（サイドバー共用）。
export function groupThreadsByDate(
  threads: Thread[],
  now = Date.now(),
): { label: ThreadGroupLabel; threads: Thread[] }[] {
  const start = new Date(now);
  start.setHours(0, 0, 0, 0);
  const today = start.getTime();
  const yesterday = today - DAY_MS;
  const week = today - 6 * DAY_MS;
  const buckets: Record<ThreadGroupLabel, Thread[]> = {
    今日: [],
    昨日: [],
    "過去 7 日間": [],
    それ以前: [],
  };
  for (const t of threads) {
    const ts = Date.parse(t.updatedAt);
    if (ts >= today) buckets["今日"].push(t);
    else if (ts >= yesterday) buckets["昨日"].push(t);
    else if (ts >= week) buckets["過去 7 日間"].push(t);
    else buckets["それ以前"].push(t);
  }
  return GROUP_ORDER.map((label) => ({ label, threads: buckets[label] })).filter(
    (g) => g.threads.length > 0,
  );
}

/// 自分のスレッド一覧を購読する React フック（更新日降順の先頭ページ）。
export function useThreads(): Thread[] {
  const [threads, setThreads] = React.useState<Thread[]>([]);
  const reload = React.useCallback(() => {
    listThreads()
      .then((r) => setThreads(r.threads))
      .catch(() => setThreads([]));
  }, []);
  React.useEffect(() => {
    reload();
    threadListeners.add(reload);
    return () => {
      threadListeners.delete(reload);
    };
  }, [reload]);
  return threads;
}

export async function listThreads(
  before?: string,
): Promise<{ threads: Thread[]; nextCursor: string | null }> {
  const sorted = [...readAll()].sort((a, b) => Date.parse(b.updatedAt) - Date.parse(a.updatedAt));
  const from = before ? sorted.findIndex((t) => t.id === before) + 1 : 0;
  const PAGE = 30;
  const page = sorted.slice(from, from + PAGE);
  const nextCursor = from + PAGE < sorted.length ? (page[page.length - 1]?.id ?? null) : null;
  return { threads: page.map(toThread), nextCursor };
}

// ── REST 相当（モック）────────────────────────────────────────────────

export async function createThread(title?: string): Promise<Thread> {
  const now = new Date().toISOString();
  const thread: StoredThread = {
    id: newId(),
    title: title?.trim() || "新しいチャット",
    createdAt: now,
    updatedAt: now,
    messages: [],
  };
  persist([thread, ...readAll()]);
  return toThread(thread);
}

export class ThreadNotFound extends Error {
  constructor() {
    super("スレッドが見つかりません");
    this.name = "ThreadNotFound";
  }
}

export async function getThreadMessages(id: string): Promise<Message[]> {
  const thread = readAll().find((t) => t.id === id);
  if (!thread) throw new ThreadNotFound();
  return thread.messages;
}

/// スレッドへメッセージを追記して永続化する（履歴・リロード表示のため）。
function appendMessage(threadId: string, message: Message): void {
  persist(
    readAll().map((t) =>
      t.id === threadId
        ? { ...t, updatedAt: message.createdAt, messages: [...t.messages, message] }
        : t,
    ),
  );
}

// ── ストリーミング（モック）────────────────────────────────────────────

export type StreamHandlers = {
  onToken?: (text: string) => void;
  onThinking?: (text: string) => void;
  onToolCall?: (call: { id: string; name: string; input: unknown }) => void;
  onToolResult?: (res: { id: string; ok: boolean }) => void;
  onCitation?: (c: Citation) => void;
  onDone?: () => void;
  onError?: (message: string) => void;
};

/// メッセージを送り、モック応答を擬似ストリーミングで返す。返り値の関数で中断できる。
/// backend 実装後はここを fetch + SSE パースへ置換する（handlers 契約は不変）。
export function streamMessage(
  threadId: string,
  text: string,
  attachments: Attachment[],
  handlers: StreamHandlers,
): () => void {
  // 送信メッセージを永続化（file_ref ＋ text）。UI 側は楽観表示済み。
  const userBlocks: ContentBlock[] = [
    ...attachments.map((a) => ({ type: "file_ref" as const, node_id: a.node_id, name: a.name })),
    { type: "text" as const, text },
  ];
  if (threadId) {
    appendMessage(threadId, {
      id: newId(),
      role: "user",
      content: userBlocks,
      createdAt: new Date().toISOString(),
    });
  }

  const full = mockReplyText(text);
  let last = "";
  // streamMockReply は累積テキストを渡すので、差分へ変換して onToken に流す。
  return streamMockReply(
    full,
    (partial) => {
      const delta = partial.slice(last.length);
      last = partial;
      if (delta) handlers.onToken?.(delta);
    },
    () => {
      if (threadId) {
        appendMessage(threadId, {
          id: newId(),
          role: "assistant",
          content: [{ type: "text", text: full }],
          createdAt: new Date().toISOString(),
        });
      }
      handlers.onDone?.();
    },
  );
}
