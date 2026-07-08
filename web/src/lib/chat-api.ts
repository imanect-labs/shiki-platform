/// チャットのクライアント側データ層（実 backend 配線）。
///
/// backend（Phase 3 / #70）の `/threads` REST ＋ `/threads/:id/stream` SSE を叩く。生成は
/// **接続非依存ジョブ**（Task 3.11）で、送信は 202 を受けて即返し、SSE は replay-then-subscribe
/// で購読する（`Last-Event-ID`=seq で再接続時に途中から・重複しない）。ページ離脱しても生成は
/// 継続し、再訪時に `generation_event` から途中経過/確定/失敗/キャンセルを復元表示する。
///
/// 公開 API（型・関数シグネチャ）はモック時代から不変に保つ（UI 側は無改修）。

"use client";

import * as React from "react";

import { apiFetch } from "@/lib/api";
import { newId } from "@/lib/chat-store";

// ── content-block（backend chat::ContentBlock と一致）───────────────────

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
  | { type: "generative_ui"; spec: unknown }
  | { type: "file_ref"; node_id: string; name: string };

export type ChatRole = "user" | "assistant" | "system" | "tool";
export type RunStatus = "queued" | "running" | "done" | "failed" | "cancelled";

export type Thread = {
  id: string;
  title: string;
  agentMode: boolean;
  createdAt: string;
  updatedAt: string;
};

export type Message = {
  id: string;
  role: ChatRole;
  content: ContentBlock[];
  agentMode?: boolean;
  createdAt: string;
};

export type Attachment = { node_id: string; name: string };
export type Citation = Extract<ContentBlock, { type: "citation" }>;

/// 共有語彙（backend chat::ThreadRole / storage::ShareTarget と一致）。
export type ThreadRole = "viewer" | "commenter" | "editor";
export type ShareTarget = { type: "user"; id: string } | { type: "role"; id: string };
export type ThreadShareEntry = { target: ShareTarget; role: ThreadRole };

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

// ── REST ──────────────────────────────────────────────────────────────

type ApiThread = {
  id: string;
  title: string;
  agent_mode: boolean;
  created_at: string;
  updated_at: string;
};

function toThread(t: ApiThread): Thread {
  return {
    id: t.id,
    title: t.title,
    agentMode: t.agent_mode,
    createdAt: t.created_at,
    updatedAt: t.updated_at,
  };
}

async function ok<T>(res: Response): Promise<T> {
  if (!res.ok) throw new Error(`API ${res.status}`);
  return (await res.json()) as T;
}

export async function listThreads(
  cursor?: string,
): Promise<{ threads: Thread[]; nextCursor: string | null }> {
  const qs = cursor ? `?cursor=${encodeURIComponent(cursor)}` : "";
  const data = await ok<{ threads: ApiThread[]; next_cursor: string | null }>(
    await apiFetch(`/threads${qs}`),
  );
  return { threads: data.threads.map(toThread), nextCursor: data.next_cursor };
}

export async function createThread(title?: string, agentMode = false): Promise<Thread> {
  const data = await ok<ApiThread>(
    await apiFetch("/threads", {
      method: "POST",
      headers: { "Content-Type": "application/json" },
      body: JSON.stringify({ title: title?.trim() || undefined, agent_mode: agentMode }),
    }),
  );
  notifyThreadsChanged();
  return toThread(data);
}

export class ThreadNotFound extends Error {
  constructor() {
    super("スレッドが見つかりません");
    this.name = "ThreadNotFound";
  }
}

export async function getThread(id: string): Promise<Thread> {
  const res = await apiFetch(`/threads/${id}`);
  if (res.status === 404 || res.status === 403) throw new ThreadNotFound();
  return toThread(await ok<ApiThread>(res));
}

type ApiMessage = {
  id: string;
  role: ChatRole;
  content: ContentBlock[];
  agent_mode?: boolean;
  created_at: string;
};

export async function getThreadMessages(id: string): Promise<Message[]> {
  const res = await apiFetch(`/threads/${id}/messages`);
  if (res.status === 404 || res.status === 403) throw new ThreadNotFound();
  const data = await ok<{ messages: ApiMessage[] }>(res);
  return data.messages.map((m) => ({
    id: m.id,
    role: m.role,
    content: m.content,
    agentMode: m.agent_mode,
    createdAt: m.created_at,
  }));
}

// ── ストリーミング（SSE・replay-then-subscribe）─────────────────────────

/// 計画のサブタスク（自律エージェント・Task 5.2）。
export type PlanSubtask = { id: string; title: string; status: string };

/// 承認要求（破壊系/egress/高コスト・Task 5.6）。
export type ApprovalRequest = {
  tool_call_id: string;
  name: string;
  input: unknown;
  reason: string;
};

export type StreamHandlers = {
  onToken?: (text: string) => void;
  onThinking?: (text: string) => void;
  onToolCall?: (call: { id: string; name: string; input: unknown }) => void;
  onToolResult?: (res: { id: string; ok: boolean }) => void;
  onCitation?: (c: Citation) => void;
  onFileRef?: (f: Attachment) => void;
  onStatus?: (status: RunStatus) => void;
  // 自律エージェント（Phase 5）。
  onPlan?: (subtasks: PlanSubtask[]) => void;
  onBudgetWarning?: (w: { kind: string; used: number; limit: number }) => void;
  onApprovalRequested?: (req: ApprovalRequest) => void;
  onApprovalResolved?: (res: { tool_call_id: string; approved: boolean }) => void;
  onFailureRecovery?: (r: { detail: string; action: string }) => void;
  /// 生成 run_id（承認 API 呼び出しに使う）。
  onRunId?: (runId: string) => void;
  onDone?: () => void;
  onError?: (message: string) => void;
};

/// 生成イベント種別（backend chat::StreamEventKind と一致・内部タグ `type`）。
type StreamEventKind =
  | { type: "token"; text: string }
  | { type: "thinking"; text: string }
  | { type: "tool_call"; id: string; name: string; input: unknown }
  | { type: "tool_result"; tool_call_id: string; ok: boolean; content: string }
  | ({ type: "citation" } & Omit<Citation, "type">)
  | { type: "file_ref"; node_id: string; name: string }
  | { type: "generative_ui"; spec: unknown }
  | { type: "plan"; subtasks: PlanSubtask[] }
  | { type: "budget_warning"; kind: string; used: number; limit: number }
  | ({ type: "approval_requested" } & ApprovalRequest)
  | { type: "approval_resolved"; tool_call_id: string; approved: boolean }
  | { type: "failure_recovery"; detail: string; action: string }
  | { type: "status"; status: RunStatus }
  | { type: "error"; message: string }
  | { type: "done"; message_id: string };

/// SSE 購読を開始し、イベントを handlers へ振り分ける。返り値でストリームを閉じる。
function subscribe(threadId: string, handlers: StreamHandlers): () => void {
  const es = new EventSource(`/api/threads/${threadId}/stream`, { withCredentials: true });
  let closed = false;
  const finish = () => {
    if (closed) return;
    closed = true;
    es.close();
  };
  es.onmessage = (ev) => {
    let kind: StreamEventKind;
    try {
      kind = JSON.parse(ev.data) as StreamEventKind;
    } catch {
      return;
    }
    switch (kind.type) {
      case "token":
        handlers.onToken?.(kind.text);
        break;
      case "thinking":
        handlers.onThinking?.(kind.text);
        break;
      case "tool_call":
        handlers.onToolCall?.({ id: kind.id, name: kind.name, input: kind.input });
        break;
      case "tool_result":
        handlers.onToolResult?.({ id: kind.tool_call_id, ok: kind.ok });
        break;
      case "citation":
        handlers.onCitation?.({
          type: "citation",
          node_id: kind.node_id,
          chunk_id: kind.chunk_id,
          snippet: kind.snippet,
          page: kind.page,
          heading_path: kind.heading_path,
          score: kind.score,
        });
        break;
      case "file_ref":
        handlers.onFileRef?.({ node_id: kind.node_id, name: kind.name });
        break;
      case "plan":
        handlers.onPlan?.(kind.subtasks);
        break;
      case "budget_warning":
        handlers.onBudgetWarning?.({ kind: kind.kind, used: kind.used, limit: kind.limit });
        break;
      case "approval_requested":
        handlers.onApprovalRequested?.({
          tool_call_id: kind.tool_call_id,
          name: kind.name,
          input: kind.input,
          reason: kind.reason,
        });
        break;
      case "approval_resolved":
        handlers.onApprovalResolved?.({
          tool_call_id: kind.tool_call_id,
          approved: kind.approved,
        });
        break;
      case "failure_recovery":
        handlers.onFailureRecovery?.({ detail: kind.detail, action: kind.action });
        break;
      case "status":
        handlers.onStatus?.(kind.status);
        // キャンセル/失敗は端末状態。途中までを確定させて閉じる。
        if (kind.status === "cancelled" || kind.status === "failed") {
          handlers.onDone?.();
          finish();
        }
        break;
      case "error":
        handlers.onError?.(kind.message);
        finish();
        break;
      case "done":
        handlers.onDone?.();
        finish();
        break;
      default:
        break;
    }
  };
  // ネットワーク断は EventSource が Last-Event-ID 付きで自動再接続する（接続非依存）。
  // 端末イベントで既に閉じている場合のみ、無駄な再接続を止める。
  es.onerror = () => {
    if (closed) es.close();
  };
  return finish;
}

/// メッセージを送信し、生成イベントを SSE で受け取る（返り値で停止できる）。
/// `cancelServer=true`（明示停止）ではサーバ側もキャンセルする。ページ離脱（既定）は継続する。
export function streamMessage(
  threadId: string,
  text: string,
  attachments: Attachment[],
  handlers: StreamHandlers,
  agentMode?: boolean,
  autonomous?: boolean,
): (opts?: { cancelServer?: boolean }) => void {
  let unsub: (() => void) | null = null;
  let runId: string | null = null;
  let stopped = false;

  apiFetch(`/threads/${threadId}/messages`, {
    method: "POST",
    headers: { "Content-Type": "application/json" },
    body: JSON.stringify({ text, attachments, agent_mode: agentMode, autonomous }),
  })
    .then(async (res) => {
      if (!res.ok) throw new Error(`送信に失敗しました (${res.status})`);
      const body = (await res.json()) as { run_id: string };
      runId = body.run_id;
      // 承認 API 呼び出しのため run_id を UI へ渡す（自律プロファイル・Task 5.6）。
      handlers.onRunId?.(runId);
      if (stopped) return;
      unsub = subscribe(threadId, handlers);
    })
    .catch((e) => handlers.onError?.(e instanceof Error ? e.message : "送信に失敗しました"));

  return (opts) => {
    stopped = true;
    unsub?.();
    if (opts?.cancelServer && runId) void cancelRun(threadId, runId);
  };
}

/// 既存 run の生成イベントを購読して復元表示する（ページ再訪・POST しない）。
export function resumeMessage(threadId: string, handlers: StreamHandlers): () => void {
  return subscribe(threadId, handlers);
}

/// 生成をユーザー明示停止する（サーバ側キャンセル）。
export async function cancelRun(threadId: string, runId: string): Promise<void> {
  await apiFetch(`/threads/${threadId}/runs/${runId}/cancel`, { method: "POST" });
}

/// 自律エージェントの承認要求へ決定を下す（承認/却下・Task 5.6）。
export async function submitApproval(
  threadId: string,
  runId: string,
  decision: { toolCallId: string; toolName: string; approved: boolean },
): Promise<void> {
  await apiFetch(`/threads/${threadId}/runs/${runId}/approvals`, {
    method: "POST",
    headers: { "Content-Type": "application/json" },
    body: JSON.stringify({
      tool_call_id: decision.toolCallId,
      tool_name: decision.toolName,
      approved: decision.approved,
    }),
  });
}

// ── 共有（ReBAC）───────────────────────────────────────────────────────

export async function shareThread(
  threadId: string,
  target: ShareTarget,
  role: ThreadRole,
): Promise<void> {
  const res = await apiFetch(`/threads/${threadId}/shares`, {
    method: "POST",
    headers: { "Content-Type": "application/json" },
    body: JSON.stringify({ target, role }),
  });
  if (!res.ok) throw new Error(`共有に失敗しました (${res.status})`);
}

export async function unshareThread(
  threadId: string,
  target: ShareTarget,
  role: ThreadRole,
): Promise<void> {
  const res = await apiFetch(`/threads/${threadId}/shares`, {
    method: "DELETE",
    headers: { "Content-Type": "application/json" },
    body: JSON.stringify({ target, role }),
  });
  if (!res.ok) throw new Error(`共有解除に失敗しました (${res.status})`);
}

export async function listThreadShares(threadId: string): Promise<ThreadShareEntry[]> {
  const data = await ok<{ shares: ThreadShareEntry[] }>(
    await apiFetch(`/threads/${threadId}/shares`),
  );
  return data.shares;
}

/// content-block が空（生成前のプレースホルダ）か。復元判定に使う。
export function isEmptyContent(content: ContentBlock[]): boolean {
  return content.length === 0;
}

export { newId };
