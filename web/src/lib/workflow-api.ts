/// ワークフローのクライアント側データ層（Phase 10・保存/検証/一覧/レイアウト/有効化）。
///
/// backend の `/workflows` 系 API を叩く。IR の型は codegen（workflow-ir.ts）・API 応答は
/// api.d.ts を正とし、手書きミラーを作らない（camelCase 変換の薄いラッパのみ）。
/// run 系（履歴・cancel/retry・SSE）は workflow-run-api.ts に分離。

"use client";

import { apiFetch } from "@/lib/api";
import { ownedPosition, type ListPage, type PageRequest } from "@/lib/owned-shared-list";
import type { components } from "@/generated/api";
import { WARNING_CODES } from "@/generated/workflow-catalog";
import type { ValidationError, WorkflowIr } from "@/generated/workflow-ir";

export type { ValidationError, WorkflowIr };
export type { WarningCode } from "@/generated/workflow-catalog";

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
  /// 自動実行の起動を見送り続けているか（理由と直し方は登録の `launchSkip`・GAP-67）。
  launchSkipped: boolean;
  updatedAt: string;
};

/// 保存結果（`warnings` は保存を止めない警告・`ir.condition_semantics_changed` など）。
///
/// 応答の型は OpenAPI の `SaveWorkflowResponse`。警告の要素はエディタ全体で使う ts-rs 生成の
/// `ValidationError`（同じ Rust 型 `workflow_engine::ValidationError` から生成）に揃える。
export type SaveResult = Omit<Schemas["SaveWorkflowResponse"], "warnings"> & {
  warnings: ValidationError[];
};

/// 検証の結果（`errors` 空 = 保存可能・`warnings` は保存を止めない）。`declaredScopes` はノードが要する
/// スコープをサーバが補完した宣言（保存するとこの宣言になる・C-P1-2）。
/// `nodePorts` はノードの出口（ノード id → 出口の名前・表示の順）。保存時検証 V2 と同じ Rust の関数が導く
/// （エディタはこれで出口のハンドルを描き、導出を TS に書き写さない・H15）。params が契約に合わず出口を導けない
/// ノードは載らない。
export type ValidateResult = {
  errors: ValidationError[];
  warnings: ValidationError[];
  declaredScopes: string[] | null;
  nodePorts: Record<string, string[]>;
};

/// 保存を止めない警告のコード（Rust の `workflow_engine::SAVE_WARNING_CODES` から生成した定数・手書きしない・
/// BUG-48）。たとえば `WARNING_CODES.SEMANTICS_CHANGED` は意味論の切替で分岐の結果が変わり得る条件、
/// `WARNING_CODES.REQUIRED_TEXT_EMPTY` は必須の文字の欄が空（保存は通り、有効化と実行の前に止まる・D-43。
/// 設定パネルは注意の帯ではなく欄の下に出す）。
export { WARNING_CODES };

/// 警告の呼び名（条件の意味論の切替だけなら「条件」、ほかを含むなら「設定」）。
export function warningNoun(warnings: { code: string }[]): string {
  return warnings.every((w) => w.code === WARNING_CODES.SEMANTICS_CHANGED) ? "条件" : "設定";
}

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
    // 状態コードを添える（読めない＝403・404 を、画面が「読めない」として出し分ける・BUG-122）。
    throw Object.assign(new Error(message), { status: res.status });
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

/// 一覧の 1 ページの頼み（backend の既定 50 件・上限 100 件）。続きの位置は `owned-shared-list.ts` の契約。
export type WorkflowListPage = PageRequest & { limit?: number };

/// 見られるワークフロー（自分のもの＋共有されたもの・更新の新しい順）の 1 ページ。
///
/// 自分のものの続きは `ownedNext` を `before` に、共有された分の続きは `sharedNext` を `sharedCursor` に渡して読む
/// （合わせたページの最後の 1 件を続きの位置に使わない・BUG-185）。
export async function listWorkflows(page: WorkflowListPage = {}): Promise<ListPage<WorkflowSummary>> {
  const q = new URLSearchParams();
  if (page.before) {
    q.set("before_updated_at", page.before.updatedAt);
    q.set("before_id", page.before.id);
  }
  if (page.sharedCursor) q.set("shared_cursor", page.sharedCursor);
  if (page.limit) q.set("limit", String(page.limit));
  const qs = q.toString();
  const res = await apiFetch(`/workflows${qs ? `?${qs}` : ""}`);
  const body = await ok<Schemas["WorkflowListResponse"]>(res);
  return {
    items: body.items.map((i) => ({
      id: i.id,
      name: i.name,
      displayName: i.display_name ?? null,
      description: i.description ?? null,
      currentVersion: i.current_version,
      triggerKinds: i.trigger_kinds,
      enabledStatus: i.enabled_status as WorkflowSummary["enabledStatus"],
      enabledVersion: i.enabled_version ?? null,
      launchSkipped: i.launch_skipped,
      updatedAt: i.updated_at,
    })),
    ownedNext: ownedPosition(body.owned_next),
    sharedNext: body.shared_next_cursor ?? null,
  };
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
export async function validateWorkflow(ir: WorkflowIr): Promise<ValidateResult> {
  const res = await apiFetch("/workflows/validate", json("POST", { ir }));
  const body = await ok<
    Omit<Schemas["ValidateWorkflowResponse"], "errors" | "warnings"> & {
      errors: ValidationError[];
      warnings?: ValidationError[];
    }
  >(res);
  return {
    errors: body.errors,
    warnings: body.warnings ?? [],
    declaredScopes: body.declared_scopes ?? null,
    nodePorts: body.node_ports ?? {},
  };
}

// ── AI エージェントの設定の天井（BUG-6）──────────────────────────────

/// 読んでよいサイトの天井・手順と時間の上限（管理者の設定・保存時の照合と同じ値）。
export type AgentSettings = Schemas["AgentSettingsResponse"];

let agentSettings: Promise<AgentSettings> | null = null;

/// AI エージェントの設定の天井（画面を開いている間は 1 回だけ取る・失敗したら次に取り直す）。
export function getAgentSettings(): Promise<AgentSettings> {
  agentSettings ??= apiFetch("/workflows/agent-settings")
    .then((res) => ok<AgentSettings>(res))
    .catch((e: unknown) => {
      agentSettings = null;
      throw e;
    });
  return agentSettings;
}

// ── スケジュールの次の実行（BUG-95）──────────────────────────────────

/// 次の実行の見通し（`next` は UTC の時刻・読めない式は `error` に保存時と同じ文言）。
export type SchedulePreview = Schemas["SchedulePreviewResponse"];

/// スケジュールの式が次に実行する時刻を、サーバーの発火と同じ読み方で数える（ブラウザーで cron を読まない。
/// 曜日の読み方がサーバーとずれるため）。
export async function previewSchedule(cron: string, tz: string): Promise<SchedulePreview> {
  const res = await apiFetch("/workflows/schedule-preview", json("POST", { cron, tz }));
  return ok<SchedulePreview>(res);
}

// ── 試運転の下見（会話の AI の試運転〔try_workflow〕の承認カード・D-197）──────────

/// 固定した版を試運転すると何が起きるか（`GET /workflows/try-preview` の応答・OpenAPI の `TryPreviewResponse`）。
/// 副作用の分け方（`effects`）はサーバーが IR から導く。web に対応表を持たない。
export type TryPreview = Schemas["TryPreviewResponse"];

/// 試運転の下見を、承認する本人の権限で読む（保存も実行もしない）。
///
/// `workflow` は id（UUID）だけで、名前は受けない。id の読み方は試運転の道具と同じなので、道具が起動を断る入力
/// （名前・読めない版）は状態コード 400 と欄（`field`: `"workflow"` か `"version"`）を添えた Error になる。無い・
/// 見えない・ワークフローでない・版が無いは、どれも状態コード 404 を添えた Error になる。
export async function fetchTryPreview(workflow: string, version: number): Promise<TryPreview> {
  const q = new URLSearchParams({ workflow, version: String(version) });
  const res = await apiFetch(`/workflows/try-preview?${q.toString()}`);
  if (res.status === 400) {
    const body = (await res.json().catch(() => null)) as Schemas["BadRequestBody"] | null;
    throw Object.assign(new Error(body?.message ?? "API 400"), { status: 400, field: body?.field ?? null });
  }
  return ok<TryPreview>(res);
}

// ── エディタレイアウト（ノード座標・IR 外・非バージョン）──────────────

export type EditorLayout = {
  positions?: Record<string, { x: number; y: number }>;
  triggers?: Record<string, { x: number; y: number }>;
  /// 繰り返し（control.map）の枠を広げた大きさ（ブロックの id → 幅と高さ・中身より小さくはならない）。
  sizes?: Record<string, { width: number; height: number }>;
  /// 繰り返しの枠を描く版のエディタで保存した印（無ければ前の版の配置として、中のある繰り返しを含む定義は開くときに
  /// 一度並べ直す・auto-layout.ts）。
  frames?: 1;
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
  /// 対象の種類（folder / file / secret / workflow / data_table）と id。読めない台帳の参照は null。
  objectKind: string | null;
  objectId: string | null;
  relation: string;
  grantedAt: string;
};

/// いま見送り続けている自動実行（GAP-67）。文はサーバーが作る作成者向けのもの（理由と直し方）。
export type LaunchSkip = {
  /// 理由の code（definition_unreadable / delegation_invalid / not_ready）。
  code: string;
  message: string;
  fix: string;
  version: number;
  firstAt: string;
  lastAt: string;
  count: number;
};

export type Registration = {
  status: "enabled" | "disabled" | "suspended_reconsent" | "none";
  enabledVersion: number | null;
  consentedScopes: string[];
  enabledBy: string | null;
  delegations: DelegationEntry[];
  /// 自動実行の起動を見送り続けていれば、その理由と回数（無ければ null）。
  launchSkip: LaunchSkip | null;
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
      objectKind: d.object_kind ?? null,
      objectId: d.object_id ?? null,
      relation: d.relation,
      grantedAt: d.granted_at,
    })),
    launchSkip: body.launch_skip
      ? {
          code: body.launch_skip.code,
          message: body.launch_skip.message,
          fix: body.launch_skip.fix,
          version: body.launch_skip.version,
          firstAt: body.launch_skip.first_at,
          lastAt: body.launch_skip.last_at,
          count: body.launch_skip.count,
        }
      : null,
  };
}

/// 同意計画（`blockers` は有効化できない理由・空なら有効化できる・D-40・D-43）。
export async function getConsentPlan(
  id: string,
  version: number,
): Promise<{ declaredScopes: string[]; grants: SuggestedGrant[]; blockers: ValidationError[] }> {
  const res = await apiFetch(`/workflows/${id}/versions/${version}/consent-plan`);
  const body = await ok<Schemas["ConsentPlanResponse"]>(res);
  return {
    declaredScopes: body.declared_scopes,
    // 要素は ts-rs 生成の `ValidationError`（OpenAPI と同じ Rust 型 `workflow_engine::ValidationError`）。
    blockers: (body.blockers ?? []) as ValidationError[],
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

/// 同意の画面からの有効化。`consent: true` で送り、対象（grants）が空でも同意し直す（使ってよい操作が増えた版へも
/// 切り替わる・BUG-102・D-90）。
export async function enableWorkflow(
  id: string,
  version: number,
  grants: GrantInput[],
): Promise<void> {
  const body: Schemas["EnableRequest"] = { version, grants, consent: true };
  const res = await apiFetch(`/workflows/${id}/enable`, json("POST", body));
  await ok<unknown>(res);
}

/// 自動実行をオフにする。`revokeDelegations` なら、ワークフローが使えるようにした権限もすべて取り消す（D-90）。
/// 取り消した数を返す。
export async function disableWorkflow(
  id: string,
  opts: { revokeDelegations?: boolean } = {},
): Promise<number> {
  const body: Schemas["DisableRequest"] = { revoke_delegations: Boolean(opts.revokeDelegations) };
  const res = await apiFetch(`/workflows/${id}/disable`, json("POST", body));
  const out = await ok<Schemas["DisableResponse"]>(res);
  return out.revoked_delegations;
}
