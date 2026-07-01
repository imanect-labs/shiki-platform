/// チャット backend クライアント（Phase 3）。スレッド CRUD と SSE ストリーミング。
///
/// REST 型は生成型（`@/generated/api`）に従う。message の content だけは OpenAPI 上 `any`
/// （content-block 配列の JSONB）なので、描画用の判別共用体をここで定義する。

import * as React from "react";

import { apiFetch } from "@/lib/api";

// ── content-block（バックエンドの chat::ContentBlock と一致）──────────────

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

// ── REST ────────────────────────────────────────────────────────────

export async function createThread(title?: string): Promise<Thread> {
  const res = await apiFetch("/threads", {
    method: "POST",
    headers: { "Content-Type": "application/json" },
    body: JSON.stringify({ title }),
  });
  if (!res.ok) throw new Error(`スレッド作成に失敗しました (${res.status})`);
  const thread = toThread(await res.json());
  notifyThreadsChanged();
  return thread;
}

// ── スレッド一覧の購読（サイドバー履歴）────────────────────────────────

const threadListeners = new Set<() => void>();

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

/// 自分のスレッド一覧を購読する React フック（最初の 1 ページ）。
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

export async function listThreads(before?: string): Promise<{ threads: Thread[]; nextCursor: string | null }> {
  const params = new URLSearchParams();
  if (before) params.set("before", before);
  const qs = params.toString();
  const res = await apiFetch(`/threads${qs ? `?${qs}` : ""}`);
  if (!res.ok) throw new Error(`スレッド一覧の取得に失敗しました (${res.status})`);
  const data = await res.json();
  return {
    threads: (data.threads ?? []).map(toThread),
    nextCursor: data.next_cursor ?? null,
  };
}

export async function getThreadMessages(id: string): Promise<Message[]> {
  const res = await apiFetch(`/threads/${id}`);
  if (res.status === 404) throw new ThreadNotFound();
  if (!res.ok) throw new Error(`メッセージ取得に失敗しました (${res.status})`);
  const data = await res.json();
  return (data.messages ?? []).map(toMessage);
}

export class ThreadNotFound extends Error {
  constructor() {
    super("スレッドが見つかりません");
    this.name = "ThreadNotFound";
  }
}

// eslint-disable-next-line @typescript-eslint/no-explicit-any
function toThread(t: any): Thread {
  return { id: t.id, title: t.title, createdAt: t.created_at, updatedAt: t.updated_at };
}

// eslint-disable-next-line @typescript-eslint/no-explicit-any
function toMessage(m: any): Message {
  return {
    id: m.id,
    role: m.role,
    content: Array.isArray(m.content) ? (m.content as ContentBlock[]) : [],
    createdAt: m.created_at,
  };
}

// ── SSE ストリーミング ────────────────────────────────────────────────

export type Citation = Extract<ContentBlock, { type: "citation" }>;

export type StreamHandlers = {
  onToken?: (text: string) => void;
  onThinking?: (text: string) => void;
  onToolCall?: (call: { id: string; name: string; input: unknown }) => void;
  onToolResult?: (res: { id: string; ok: boolean }) => void;
  onCitation?: (c: Citation) => void;
  onDone?: () => void;
  onError?: (message: string) => void;
};

/// メッセージを送り、SSE 応答を購読する。返り値の関数で中断できる。
export function streamMessage(
  threadId: string,
  text: string,
  attachments: Attachment[],
  handlers: StreamHandlers,
): () => void {
  const controller = new AbortController();

  (async () => {
    let res: Response;
    try {
      res = await apiFetch(`/threads/${threadId}/messages`, {
        method: "POST",
        headers: { "Content-Type": "application/json" },
        body: JSON.stringify({ text, attachments }),
        signal: controller.signal,
      });
    } catch (e) {
      if (!controller.signal.aborted) handlers.onError?.(asMessage(e));
      return;
    }

    if (!res.ok || !res.body) {
      handlers.onError?.(`応答の取得に失敗しました (${res.status})`);
      return;
    }

    const reader = res.body.getReader();
    const decoder = new TextDecoder();
    let buffer = "";

    try {
      // SSE はイベントを空行（\n\n）で区切る。逐次パースしてハンドラに振り分ける。
      for (;;) {
        const { value, done } = await reader.read();
        if (done) break;
        buffer += decoder.decode(value, { stream: true });
        let sep: number;
        while ((sep = buffer.indexOf("\n\n")) !== -1) {
          const raw = buffer.slice(0, sep);
          buffer = buffer.slice(sep + 2);
          dispatch(raw, handlers);
        }
      }
      handlers.onDone?.();
    } catch (e) {
      if (!controller.signal.aborted) handlers.onError?.(asMessage(e));
    }
  })();

  return () => controller.abort();
}

function dispatch(raw: string, handlers: StreamHandlers): void {
  let event = "message";
  const dataLines: string[] = [];
  for (const line of raw.split("\n")) {
    if (line.startsWith("event:")) event = line.slice(6).trim();
    else if (line.startsWith("data:")) dataLines.push(line.slice(5).trim());
  }
  const data = dataLines.join("\n");
  let payload: Record<string, unknown> = {};
  try {
    payload = data ? JSON.parse(data) : {};
  } catch {
    return;
  }

  switch (event) {
    case "token":
      handlers.onToken?.(String(payload.text ?? ""));
      break;
    case "thinking":
      handlers.onThinking?.(String(payload.text ?? ""));
      break;
    case "tool_call":
      handlers.onToolCall?.({
        id: String(payload.id ?? ""),
        name: String(payload.name ?? ""),
        input: payload.input,
      });
      break;
    case "tool_result":
      handlers.onToolResult?.({ id: String(payload.id ?? ""), ok: Boolean(payload.ok) });
      break;
    case "citation":
      handlers.onCitation?.({
        type: "citation",
        node_id: String(payload.node_id ?? ""),
        chunk_id: String(payload.chunk_id ?? ""),
        snippet: String(payload.snippet ?? ""),
        page: (payload.page as number | null) ?? null,
        heading_path: (payload.heading_path as string[]) ?? [],
        score: Number(payload.score ?? 0),
      });
      break;
    case "done":
      handlers.onDone?.();
      break;
    case "error":
      handlers.onError?.(String(payload.message ?? "エラーが発生しました"));
      break;
  }
}

function asMessage(e: unknown): string {
  return e instanceof Error ? e.message : "通信エラーが発生しました";
}
