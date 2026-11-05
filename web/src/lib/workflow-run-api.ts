/// run 系のクライアント側データ層（起動・履歴・キャンセル/再実行・SSE・Task 10.14）。

"use client";

import { apiFetch } from "@/lib/api";
import type { components } from "@/generated/api";

type Schemas = components["schemas"];

async function ok<T>(res: Response): Promise<T> {
  if (!res.ok) {
    let message = `API ${res.status}`;
    try {
      // 実行できない設定（`not_ready`・必須の文字の欄が空など・D-43）は理由を全件 `errors` で返す。
      const body = (await res.json()) as { message?: string; errors?: { message?: string }[] };
      const reasons = (body.errors ?? []).flatMap((e) => (e.message ? [e.message] : []));
      if (reasons.length) message = reasons.join(" / ");
      else if (body.message) message = body.message;
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

/// 実行の入力の誤り 1 件（項目のキーと理由・サーバーの `InvalidRunInputBody`・BUG-109）。
export type RunInputIssue = Schemas["InputIssueBody"];

/// 実行を始められなかった理由（実行のダイアログが、欄の下と誤りの帯に出し分ける・BUG-109）。
///
/// - `issues`: 入力の誤り（`invalid_input`・項目のキーと理由）。
/// - `field`: 誤りのある欄（`trigger` はきっかけの内容）。
/// - `reasons`: 実行できない設定（`not_ready`・ブロックの id と理由）。
/// `message` は状態コードの既定の文言か、サーバーの文言（付録 C の §3.4.3）。
export class RunStartError extends Error {
  constructor(
    public status: number,
    public code: string | null,
    message: string,
    public issues: RunInputIssue[] = [],
    public field: string | null = null,
    public reasons: { nodeId: string | null; message: string }[] = [],
  ) {
    super(message);
    this.name = "RunStartError";
  }
}

/// 状態コードの既定の文言（サーバーが code を返さないときの最後の手段・付録 C の §3.4.3）。
function runStatusMessage(status: number): string {
  if (status === 401) return "ログインが切れました。もう一度ログインしてください。";
  if (status === 403) return "この操作をする権限がありません。必要なら、ワークフローの持ち主か管理者に頼んでください。";
  if (status === 404) return "ワークフローが見つかりません。削除されたか、見る権限がありません。";
  if (status === 429) return "利用の上限か、操作の集中のため受け付けられませんでした。少し待ってから、もう一度実行してください。";
  if (status === 503) return "ワークフローの実行はいま使えません。少し待ってから、もう一度実行してください。";
  if (status >= 500) return "サーバーで問題が起きました。続くときは管理者に連絡してください。";
  return "内容に誤りがあるため、受け付けられませんでした。入力した値を見直してください。";
}

/// 手動の実行で試す値（どれも省けば今までどおり）。
export type RunTrial = {
  /// 試すきっかけの内容（省略すると入力と同じ・D-93）。
  trigger?: Record<string, unknown>;
  /// 試す予定の日時（`2026-11-09T09:00` のような時差の無い日時は日本時間で読む。省略すると実行した日時・GAP-88）。
  scheduledAt?: string;
};

/// 対話トリガで実行する（実行主体 = 自分）。`trial` は試す値（きっかけの内容と予定の日時）。
/// 始められなければ [`RunStartError`] を投げる（本文の理由を捨てない・BUG-109）。
export async function startRun(
  workflowId: string,
  input: unknown,
  trial: RunTrial = {},
): Promise<string | null> {
  // 項目の名前は生成した型（`StartRunRequest`）で縛る。省いた試す値は送らない（サーバーは今までどおりに動く）。
  const request: { [K in keyof Schemas["StartRunRequest"]]?: unknown } = { input };
  if (trial.trigger !== undefined) request.trigger = trial.trigger;
  if (trial.scheduledAt !== undefined) request.scheduled_at = trial.scheduledAt;
  let res: Response;
  try {
    res = await apiFetch(`/workflows/${workflowId}/runs`, json("POST", request));
  } catch {
    throw new RunStartError(
      0,
      null,
      "通信できませんでした。ネットワークにつながっているか確かめて、もう一度実行してください。",
    );
  }
  if (!res.ok) {
    let body: {
      code?: string;
      message?: string;
      field?: string;
      errors?: { key?: string; message?: string; node_id?: string | null }[];
    } = {};
    try {
      body = (await res.json()) as typeof body;
    } catch {
      // 本文なし
    }
    const errors = body.errors ?? [];
    const code = body.code ?? null;
    throw new RunStartError(
      res.status,
      code,
      body.message ?? runStatusMessage(res.status),
      code === "invalid_input"
        ? errors.flatMap((e) => (e.message ? [{ key: e.key ?? "", message: e.message }] : []))
        : [],
      body.field ?? null,
      code === "not_ready"
        ? errors.flatMap((e) => (e.message ? [{ nodeId: e.node_id ?? null, message: e.message }] : []))
        : [],
    );
  }
  const body = (await res.json()) as { run_id: string | null };
  return body.run_id;
}

// ── 履歴（一覧・詳細・step・イベント）──────────────────────────────────

/// 待機中の run・step が待っているもの（サーバーの run の状態から導いた値・engine.md §3.1・BUG-119）。
export type WaitingFor = Schemas["WaitingFor"];

/// 実行の予定の日時の由来（スケジュールの予定の時刻・手動の実行で指定した日時・始めた実行から引き継いだ日時・GAP-89）。
export type ScheduledAtSource = Schemas["ScheduledAtSource"];

/// 実行の予定の日時（予定の日時の無い実行は null）。
export type RunScheduledAt = { at: string; source: ScheduledAtSource };

function scheduledOf(at: string | null | undefined, source: ScheduledAtSource | null | undefined): RunScheduledAt | null {
  return at && source ? { at, source } : null;
}

export type RunListItem = {
  runId: string;
  status: string;
  /// 待機中（承認・変更・時刻を待っている）なら待っているもの。実行中や終わった run は null。
  waitingFor: WaitingFor | null;
  triggerKind: string;
  version: number;
  createdAt: string;
  startedAt: string | null;
  finishedAt: string | null;
  /// 予定の日時（スケジュールの予定の時刻など。無い実行は null）。
  scheduled: RunScheduledAt | null;
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
    waitingFor: i.waiting_for ?? null,
    triggerKind: i.trigger_kind,
    version: i.version,
    createdAt: i.created_at,
    startedAt: i.started_at ?? null,
    finishedAt: i.finished_at ?? null,
    scheduled: scheduledOf(i.scheduled_at, i.scheduled_at_source),
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
  /// 待っている step なら待っているもの（承認・変更・時刻）。
  waitingFor: WaitingFor | null;
};

/// 失敗した run の理由（利用者向けの分類と文言・参照番号・Rust の単一定義から組んだもの・BUG-113）。
export type RunFailure = Schemas["RunFailure"];

/// 「失敗したところから再開」を勧めるかと理由（BUG-132）。
export type ResumeHint = Schemas["ResumeHint"];

export type RunDetail = {
  runId: string;
  status: string;
  /// 待機中なら待っているもの（[`RunListItem.waitingFor`] と同じ）。
  waitingFor: WaitingFor | null;
  triggerKind: string;
  version: number;
  input: unknown;
  failReason: string | null;
  /// 失敗していれば理由（文言はサーバーが組む。web に対応表を持たない）。
  failure: RunFailure | null;
  /// 失敗していれば、再開を勧めるかと理由。
  resume: ResumeHint | null;
  traceId: string | null;
  cancelRequested: boolean;
  createdAt: string;
  startedAt: string | null;
  finishedAt: string | null;
  /// 予定の日時（[`RunListItem.scheduled`] と同じ）。
  scheduled: RunScheduledAt | null;
  steps: StepOverview[];
};

export async function getRun(workflowId: string, runId: string): Promise<RunDetail> {
  const res = await apiFetch(`/workflows/${workflowId}/runs/${runId}`);
  const d = await ok<Schemas["RunDetailResponse"]>(res);
  return {
    runId: d.run_id,
    status: d.status,
    waitingFor: d.waiting_for ?? null,
    triggerKind: d.trigger_kind,
    version: d.version,
    input: d.input,
    failReason: d.fail_reason ?? null,
    failure: d.failure ?? null,
    resume: d.resume ?? null,
    traceId: d.trace_id ?? null,
    cancelRequested: d.cancel_requested,
    createdAt: d.created_at,
    startedAt: d.started_at ?? null,
    finishedAt: d.finished_at ?? null,
    scheduled: scheduledOf(d.scheduled_at, d.scheduled_at_source),
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
      waitingFor: s.waiting_for ?? null,
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
