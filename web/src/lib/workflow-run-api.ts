/// run 系のクライアント側データ層（起動・履歴・キャンセル/再実行・SSE・Task 10.14）。

"use client";

import { apiFetch } from "@/lib/api";
import type { components } from "@/generated/api";

type Schemas = components["schemas"];

async function ok<T>(res: Response): Promise<T> {
  if (!res.ok) {
    let message = `API ${res.status}`;
    try {
      const body = (await res.json()) as { message?: string };
      if (body.message) message = body.message;
    } catch {
      // 本文なし
    }
    throw new Error(message);
  }
  return (await res.json()) as T;
}

function json(method: string, body: unknown): RequestInit {
  return {
    method,
    headers: { "Content-Type": "application/json" },
    body: JSON.stringify(body),
  };
}

/// 対話トリガで実行する（実行主体 = 自分）。
export async function startRun(
  workflowId: string,
  input: unknown,
): Promise<string | null> {
  const res = await apiFetch(`/workflows/${workflowId}/runs`, json("POST", { input }));
  const body = await ok<{ run_id: string | null }>(res);
  return body.run_id;
}

// ── 履歴（一覧・詳細・step・イベント）──────────────────────────────────

export type RunListItem = {
  runId: string;
  status: string;
  triggerKind: string;
  version: number;
  createdAt: string;
  startedAt: string | null;
  finishedAt: string | null;
};

export type RunListFilter = {
  statuses?: string[];
  triggerKinds?: string[];
  before?: { createdAt: string; runId: string };
  limit?: number;
};

export async function listRuns(
  workflowId: string,
  filter: RunListFilter = {},
): Promise<RunListItem[]> {
  const q = new URLSearchParams();
  if (filter.statuses?.length) q.set("status", filter.statuses.join(","));
  if (filter.triggerKinds?.length) q.set("trigger_kind", filter.triggerKinds.join(","));
  if (filter.before) {
    q.set("before_created_at", filter.before.createdAt);
    q.set("before_run_id", filter.before.runId);
  }
  if (filter.limit) q.set("limit", String(filter.limit));
  const res = await apiFetch(`/workflows/${workflowId}/runs?${q.toString()}`);
  const body = await ok<{ items: Schemas["RunListItemDto"][] }>(res);
  return body.items.map((i) => ({
    runId: i.run_id,
    status: i.status,
    triggerKind: i.trigger_kind,
    version: i.version,
    createdAt: i.created_at,
    startedAt: i.started_at ?? null,
    finishedAt: i.finished_at ?? null,
  }));
}

export type StepOverview = {
  stepPath: string;
  nodeId: string;
  status: string;
  attempt: number;
  takenPorts: string[];
  hasOutput: boolean;
  error: unknown;
  nextRetryAt: string | null;
  wakeAt: string | null;
  updatedAt: string;
  langfuseTraceId: string | null;
};

export type RunDetail = {
  runId: string;
  status: string;
  triggerKind: string;
  version: number;
  input: unknown;
  failReason: string | null;
  traceId: string | null;
  cancelRequested: boolean;
  createdAt: string;
  startedAt: string | null;
  finishedAt: string | null;
  steps: StepOverview[];
};

export async function getRun(workflowId: string, runId: string): Promise<RunDetail> {
  const res = await apiFetch(`/workflows/${workflowId}/runs/${runId}`);
  const d = await ok<Schemas["RunDetailResponse"]>(res);
  return {
    runId: d.run_id,
    status: d.status,
    triggerKind: d.trigger_kind,
    version: d.version,
    input: d.input,
    failReason: d.fail_reason ?? null,
    traceId: d.trace_id ?? null,
    cancelRequested: d.cancel_requested,
    createdAt: d.created_at,
    startedAt: d.started_at ?? null,
    finishedAt: d.finished_at ?? null,
    steps: d.steps.map((s) => ({
      stepPath: s.step_path,
      nodeId: s.node_id,
      status: s.status,
      attempt: s.attempt,
      takenPorts: s.taken_ports,
      hasOutput: s.has_output,
      error: s.error,
      nextRetryAt: s.next_retry_at ?? null,
      wakeAt: s.wake_at ?? null,
      updatedAt: s.updated_at,
      langfuseTraceId: s.langfuse_trace_id ?? null,
    })),
  };
}

export type StepDetail = {
  stepPath: string;
  nodeId: string;
  status: string;
  attempt: number;
  takenPorts: string[];
  output: unknown;
  error: unknown;
  langfuseTraceId: string | null;
};

export async function getStep(
  workflowId: string,
  runId: string,
  stepPath: string,
): Promise<StepDetail> {
  const res = await apiFetch(
    `/workflows/${workflowId}/runs/${runId}/steps?path=${encodeURIComponent(stepPath)}`,
  );
  const s = await ok<Schemas["StepDetailResponse"]>(res);
  return {
    stepPath: s.step_path,
    nodeId: s.node_id,
    status: s.status,
    attempt: s.attempt,
    takenPorts: s.taken_ports,
    output: s.output,
    error: s.error,
    langfuseTraceId: s.langfuse_trace_id ?? null,
  };
}

// ── 操作（キャンセル・再実行）────────────────────────────────────────────

export async function cancelRun(workflowId: string, runId: string): Promise<string> {
  const res = await apiFetch(
    `/workflows/${workflowId}/runs/${runId}/cancel`,
    json("POST", {}),
  );
  const body = await ok<{ outcome: string }>(res);
  return body.outcome;
}

export async function retryRun(
  workflowId: string,
  runId: string,
  mode: "resume" | "new",
): Promise<string | null> {
  const res = await apiFetch(
    `/workflows/${workflowId}/runs/${runId}/retry`,
    json("POST", { mode }),
  );
  const body = await ok<{ run_id: string | null }>(res);
  return body.run_id;
}

// ── ライブ更新（SSE・Last-Event-ID リプレイ・terminal で close）──────────

export type RunEventMessage = {
  seq: number;
  kind: string;
  payload: unknown;
  createdAt: string;
};

export function subscribeRunEvents(
  workflowId: string,
  runId: string,
  handlers: {
    onEvent: (e: RunEventMessage) => void;
    onTerminal: (status: string) => void;
    onError?: () => void;
  },
): () => void {
  const es = new EventSource(
    `/api/workflows/${workflowId}/runs/${runId}/events/stream`,
    { withCredentials: true },
  );
  es.addEventListener("run_event", (ev) => {
    try {
      const data = JSON.parse((ev as MessageEvent).data as string) as {
        kind: string;
        payload: unknown;
        created_at: string;
      };
      handlers.onEvent({
        seq: Number((ev as MessageEvent).lastEventId || 0),
        kind: data.kind,
        payload: data.payload,
        createdAt: data.created_at,
      });
    } catch {
      // 壊れたイベントは無視（DB リプレイが正）。
    }
  });
  es.addEventListener("run.terminal", (ev) => {
    try {
      const data = JSON.parse((ev as MessageEvent).data as string) as { status: string };
      handlers.onTerminal(data.status);
    } finally {
      es.close();
    }
  });
  es.onerror = () => handlers.onError?.();
  return () => es.close();
}
