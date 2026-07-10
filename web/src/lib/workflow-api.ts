/// ワークフローのクライアント側データ層（Phase 10・保存/検証/一覧/レイアウト/有効化）。
///
/// backend の `/workflows` 系 API を叩く。IR の型は codegen（workflow-ir.ts）・API 応答は
/// api.d.ts を正とし、手書きミラーを作らない（camelCase 変換の薄いラッパのみ）。
/// run 系（履歴・cancel/retry・SSE）は workflow-run-api.ts に分離。

"use client";

import { apiFetch } from "@/lib/api";
import type { components } from "@/generated/api";
import type { ValidationError, WorkflowIr } from "@/generated/workflow-ir";

export type { ValidationError, WorkflowIr };

type Schemas = components["schemas"];

/// 一覧 1 行の要約（GET /workflows）。
export type WorkflowSummary = {
  id: string;
  name: string;
  displayName: string | null;
  description: string | null;
  currentVersion: number;
  triggerKinds: string[];
  enabledStatus: "enabled" | "disabled" | "suspended_reconsent" | "none";
  enabledVersion: number | null;
  updatedAt: string;
};

/// 保存結果。
export type SaveResult = { id: string; version: number; name: string };

/// 検証エラー（保存 400 / validate 200 の両方で同じ形）。
export class WorkflowValidationError extends Error {
  constructor(public errors: ValidationError[]) {
    super(errors.map((e) => e.message).join(" / ") || "検証エラー");
    this.name = "WorkflowValidationError";
  }
}

async function readErrors(res: Response): Promise<ValidationError[] | null> {
  try {
    const body = (await res.clone().json()) as { errors?: ValidationError[] };
    return body.errors ?? null;
  } catch {
    return null;
  }
}

async function ok<T>(res: Response): Promise<T> {
  if (!res.ok) {
    const errors = await readErrors(res);
    if (errors?.length) throw new WorkflowValidationError(errors);
    let message = `API ${res.status}`;
    try {
      const body = (await res.json()) as { message?: string; missing_scopes?: string[] };
      if (body.message) message = body.message;
      if (body.missing_scopes?.length) {
        message += `（不足スコープ: ${body.missing_scopes.join(", ")}）`;
      }
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

// ── 一覧・取得・保存・検証 ──────────────────────────────────────────

/// keyset ページング（backend の既定 50 件・上限 100 件と同じ契約）。
export type WorkflowListPage = {
  before?: { updatedAt: string; id: string };
  limit?: number;
};

export async function listWorkflows(
  page: WorkflowListPage = {},
): Promise<WorkflowSummary[]> {
  const q = new URLSearchParams();
  if (page.before) {
    q.set("before_updated_at", page.before.updatedAt);
    q.set("before_id", page.before.id);
  }
  if (page.limit) q.set("limit", String(page.limit));
  const qs = q.toString();
  const res = await apiFetch(`/workflows${qs ? `?${qs}` : ""}`);
  const body = await ok<{ items: Schemas["WorkflowSummaryDto"][] }>(res);
  return body.items.map((i) => ({
    id: i.id,
    name: i.name,
    displayName: i.display_name ?? null,
    description: i.description ?? null,
    currentVersion: i.current_version,
    triggerKinds: i.trigger_kinds,
    enabledStatus: i.enabled_status as WorkflowSummary["enabledStatus"],
    enabledVersion: i.enabled_version ?? null,
    updatedAt: i.updated_at,
  }));
}

export async function getWorkflow(
  id: string,
): Promise<{ id: string; version: number; ir: WorkflowIr }> {
  const res = await apiFetch(`/workflows/${id}`);
  const body = await ok<Schemas["WorkflowVersionResponse"]>(res);
  return { id: body.id, version: body.version, ir: body.ir as unknown as WorkflowIr };
}

export async function getWorkflowVersion(
  id: string,
  version: number,
): Promise<{ id: string; version: number; ir: WorkflowIr }> {
  const res = await apiFetch(`/workflows/${id}/versions/${version}`);
  const body = await ok<Schemas["WorkflowVersionResponse"]>(res);
  return { id: body.id, version: body.version, ir: body.ir as unknown as WorkflowIr };
}

export async function createWorkflow(ir: WorkflowIr): Promise<SaveResult> {
  const res = await apiFetch("/workflows", json("POST", { ir }));
  return ok<SaveResult>(res);
}

export async function updateWorkflow(
  id: string,
  ir: WorkflowIr,
  expectedVersion: number,
): Promise<SaveResult> {
  const res = await apiFetch(
    `/workflows/${id}`,
    json("PUT", { ir, expected_version: expectedVersion }),
  );
  return ok<SaveResult>(res);
}

/// 保存せず検証のみ（dnd のライブ検証・600ms debounce で呼ぶ）。
export async function validateWorkflow(ir: WorkflowIr): Promise<ValidationError[]> {
  const res = await apiFetch("/workflows/validate", json("POST", { ir }));
  const body = await ok<{ errors: ValidationError[] }>(res);
  return body.errors;
}

// ── エディタレイアウト（ノード座標・IR 外・非バージョン）──────────────

export type EditorLayout = {
  positions?: Record<string, { x: number; y: number }>;
  triggers?: Record<string, { x: number; y: number }>;
};

export async function getLayout(id: string): Promise<EditorLayout> {
  const res = await apiFetch(`/workflows/${id}/layout`);
  const body = await ok<{ layout: EditorLayout }>(res);
  return body.layout ?? {};
}

export async function putLayout(id: string, layout: EditorLayout): Promise<void> {
  const res = await apiFetch(`/workflows/${id}/layout`, json("PUT", { layout }));
  await ok<unknown>(res);
}

// ── 有効化・同意（schedule/event トリガの委譲）─────────────────────────

export type DelegationEntry = {
  delegator: string;
  scope: string;
  objectRef: string;
  relation: string;
  grantedAt: string;
};

export type Registration = {
  status: "enabled" | "disabled" | "suspended_reconsent" | "none";
  enabledVersion: number | null;
  consentedScopes: string[];
  enabledBy: string | null;
  delegations: DelegationEntry[];
};

export type SuggestedGrant = {
  scope: string;
  objectKind: string;
  objectId: string | null;
  objectName: string | null;
  relation: string;
  source: string;
  needsUserPick: boolean;
};

export type GrantInput = {
  scope: string;
  object_type: string;
  object_id: string;
  relation: string;
};

export async function getRegistration(id: string): Promise<Registration> {
  const res = await apiFetch(`/workflows/${id}/registration`);
  const body = await ok<Schemas["RegistrationResponse"]>(res);
  return {
    status: body.status as Registration["status"],
    enabledVersion: body.enabled_version ?? null,
    consentedScopes: body.consented_scopes,
    enabledBy: body.enabled_by ?? null,
    delegations: body.delegations.map((d) => ({
      delegator: d.delegator,
      scope: d.scope,
      objectRef: d.object_ref,
      relation: d.relation,
      grantedAt: d.granted_at,
    })),
  };
}

export async function getConsentPlan(
  id: string,
  version: number,
): Promise<{ declaredScopes: string[]; grants: SuggestedGrant[] }> {
  const res = await apiFetch(`/workflows/${id}/versions/${version}/consent-plan`);
  const body = await ok<Schemas["ConsentPlanResponse"]>(res);
  return {
    declaredScopes: body.declared_scopes,
    grants: body.grants.map((g) => ({
      scope: g.scope,
      objectKind: g.object_kind,
      objectId: g.object_id ?? null,
      objectName: g.object_name ?? null,
      relation: g.relation,
      source: g.source,
      needsUserPick: g.needs_user_pick,
    })),
  };
}

export async function enableWorkflow(
  id: string,
  version: number,
  grants: GrantInput[],
): Promise<void> {
  const res = await apiFetch(
    `/workflows/${id}/enable`,
    json("POST", { version, grants }),
  );
  await ok<unknown>(res);
}

export async function disableWorkflow(id: string): Promise<void> {
  const res = await apiFetch(`/workflows/${id}/disable`, json("POST", {}));
  await ok<unknown>(res);
}
