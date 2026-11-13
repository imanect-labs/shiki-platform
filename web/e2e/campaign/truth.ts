import { expect, type Page } from "@playwright/test";

import { call, ok, type Json } from "./http";

/// 裏取り（台帳 §0.1 (3): 画面の文言だけで判定しない）。行・run・承認・ファイル・監査を、読む人の権限で読む。

export type DataRecord = {
  id: string;
  rev: number;
  data: Record<string, unknown>;
  created_by?: string;
  created_at?: string;
  updated_at?: string;
};

/// 条件木の葉（data の query IR v2・`crates/data/src/query/ir.rs`）。
export function cmp(field: string, op: string, value?: unknown): Json {
  return value === undefined ? { cmp: { field, op } } : { cmp: { field, op, value } };
}

/// すべてを満たす。
export function all(...conds: Json[]): Json {
  return { all: conds };
}

// ---- 行 ----

/// `POST /data/tables/{id}/query` の 1 ページ（状態コードも返す・権限の否定に使う）。
async function queryPage(page: Page, tableId: string, query: Json): Promise<{ status: number; items: DataRecord[]; next?: string }> {
  const r = await call<{ items?: DataRecord[]; next_cursor?: string }>(page, "POST", `/data/tables/${tableId}/query`, query);
  return { status: r.status, items: r.body?.items ?? [], next: r.body?.next_cursor ?? undefined };
}

export const rows = {
  /// 条件に合う行を全部（200 件ずつ続きを読む・読めなければ落とす）。
  async query(page: Page, tableId: string, filter?: Json, sort?: Json[]): Promise<DataRecord[]> {
    const out: DataRecord[] = [];
    let cursor: string | undefined;
    for (let i = 0; i < 50; i++) {
      const q: Json = { limit: 200 };
      if (filter) q.filter = filter;
      if (sort) q.sort = sort;
      if (cursor) q.cursor = cursor;
      const p = await queryPage(page, tableId, q);
      expect(p.status, `query ${tableId} → ${p.status}`).toBe(200);
      out.push(...p.items);
      if (!p.next) break;
      cursor = p.next;
    }
    return out;
  },
  /// 1 ページだけ読み、状態コードも返す（権限の否定の確かめ）。
  async try(page: Page, tableId: string, filter?: Json): Promise<{ status: number; items: DataRecord[] }> {
    return queryPage(page, tableId, filter ? { filter, limit: 200 } : { limit: 200 });
  },
  async count(page: Page, tableId: string, filter?: Json): Promise<number> {
    return (await rows.query(page, tableId, filter)).length;
  },
  async get(page: Page, tableId: string, id: string): Promise<DataRecord> {
    return ok<DataRecord>(page, "GET", `/data/tables/${tableId}/records/${id}`);
  },
  /// 状態の列が `states` のどれかになるまで待つ。
  async waitForStatus(
    page: Page,
    tableId: string,
    id: string,
    states: string[],
    opts: { field?: string; timeout?: number } = {},
  ): Promise<DataRecord> {
    let rec: DataRecord | undefined;
    await expect
      .poll(
        async () => {
          rec = await rows.get(page, tableId, id);
          return String(rec.data[opts.field ?? "status"] ?? "");
        },
        { timeout: opts.timeout ?? 90_000, intervals: [1_000, 2_000] },
      )
      .toMatch(new RegExp(`^(${states.join("|")})$`));
    return rec!;
  },
  async revisions(page: Page, tableId: string, id: string): Promise<Json[]> {
    return (await ok<{ items: Json[] }>(page, "GET", `/data/tables/${tableId}/records/${id}/revisions`)).items;
  },
};

/// 条件に合う行がちょうど 1 行であることを確かめて返す（二重にならない・台帳がいつも 1 行、の判定）。
export async function expectSingleRow(page: Page, tableId: string, filter: Json, what = "行"): Promise<DataRecord> {
  const found = await rows.query(page, tableId, filter);
  expect(found.length, `${what}はちょうど 1 行（${JSON.stringify(filter)} → ${found.length} 行）`).toBe(1);
  return found[0];
}

// ---- run ----

export type RunItem = {
  run_id: string;
  status: string;
  trigger_kind: string;
  version: number;
  created_at: string;
  scheduled_at?: string | null;
  scheduled_at_source?: string | null;
};
export type StepOverview = {
  step_path: string;
  node_id: string;
  status: string;
  attempt: number;
  taken_ports: string[];
  has_output: boolean;
  error: { code?: string; message?: string } | null;
};
export type RunDetail = {
  run_id: string;
  status: string;
  trigger_kind: string;
  input: unknown;
  failure?: { code?: string; message: string } | null;
  scheduled_at?: string | null;
  steps: StepOverview[];
};

const RUN_DONE = ["succeeded", "failed", "cancelled", "skipped"];

export const runs = {
  /// ワークフローの run の一覧（新しい順・`trigger_kind`・`status` で絞る）。読めなければ落とす。
  async list(page: Page, workflowId: string, f: { trigger_kind?: string; status?: string } = {}): Promise<RunItem[]> {
    const q = new URLSearchParams({ limit: "100" });
    if (f.trigger_kind) q.set("trigger_kind", f.trigger_kind);
    if (f.status) q.set("status", f.status);
    return (await ok<{ items: RunItem[] }>(page, "GET", `/workflows/${workflowId}/runs?${q}`)).items;
  },
  /// `trigger_kind` の run が `count` 本以上できるまで待つ（新しい順で返す）。
  async waitFor(
    page: Page,
    workflowId: string,
    f: { trigger_kind: string; count?: number; timeout?: number; message?: string },
  ): Promise<RunItem[]> {
    let items: RunItem[] = [];
    await expect
      .poll(
        async () => {
          items = await runs.list(page, workflowId, { trigger_kind: f.trigger_kind });
          return items.length;
        },
        { timeout: f.timeout ?? 90_000, intervals: [2_000, 5_000], message: f.message ?? `${f.trigger_kind} の run が出る` },
      )
      .toBeGreaterThanOrEqual(f.count ?? 1);
    return items;
  },
  async get(page: Page, workflowId: string, runId: string): Promise<RunDetail> {
    return ok<RunDetail>(page, "GET", `/workflows/${workflowId}/runs/${runId}`);
  },
  /// run が終わるまで待って詳細を返す。
  async waitEnd(page: Page, workflowId: string, runId: string, timeout = 90_000): Promise<RunDetail> {
    let d: RunDetail | undefined;
    await expect
      .poll(
        async () => {
          d = await runs.get(page, workflowId, runId);
          return RUN_DONE.includes(d.status);
        },
        { timeout, intervals: [1_000, 2_000], message: `run ${runId} が終わる` },
      )
      .toBe(true);
    return d!;
  },
  /// step の概要（ノード id で引く・map の中は最初の 1 つ）。無ければ落とす。
  step(d: RunDetail, nodeId: string): StepOverview {
    const s = d.steps.find((x) => x.node_id === nodeId);
    expect(s, `run ${d.run_id} に step ${nodeId} がある（${d.steps.map((x) => x.node_id).join(",")}）`).toBeDefined();
    return s!;
  },
  /// step が通った出口。step が通らなかった（一覧に無い・`skipped`）なら null。
  takenPorts(d: RunDetail, nodeId: string): string[] | null {
    const s = d.steps.find((x) => x.node_id === nodeId);
    return !s || s.status === "skipped" ? null : s.taken_ports;
  },
  /// step の状態（`succeeded`・`failed`・`skipped` など・一覧に無ければ null）。
  stepStatus(d: RunDetail, nodeId: string): string | null {
    return d.steps.find((x) => x.node_id === nodeId)?.status ?? null;
  },
  /// step の誤りの code（無ければ null）。
  errorCode(d: RunDetail, nodeId: string): string | null {
    return d.steps.find((x) => x.node_id === nodeId)?.error?.code ?? null;
  },
  /// step の出力（`path` は step_path・map の外ならノード id）。
  async stepOutput<T = Record<string, unknown>>(page: Page, workflowId: string, runId: string, path: string): Promise<T> {
    return (await ok<{ output: T }>(page, "GET", `/workflows/${workflowId}/runs/${runId}/steps?path=${encodeURIComponent(path)}`)).output;
  },
};

// ---- 承認（workflow-decide-scenarios.spec.ts:75-106 から写した。spec を import しない） ----

export type Approval = {
  approval_id: string;
  title: string;
  from_state: string;
  record: { id: string; rev: number; table_id?: string };
};

export const approvals = {
  async inbox(page: Page): Promise<Approval[]> {
    return (await ok<{ items: Approval[] }>(page, "GET", "/approvals")).items;
  },
  /// 行の承認が受信箱に出るまで待つ（承認の停止は relay の tick で数秒かかる）。
  async approvalFor(page: Page, recordId: string, title?: string, timeout = 60_000): Promise<Approval> {
    let found: Approval | undefined;
    await expect
      .poll(
        async () => {
          found = (await approvals.inbox(page)).find((a) => a.record.id === recordId && (title === undefined || a.title === title));
          return Boolean(found);
        },
        { timeout, intervals: [1_000, 2_000], message: `承認 ${title ?? ""} が受信箱に出る` },
      )
      .toBe(true);
    return found!;
  },
  async decideStatus(
    page: Page,
    a: Approval,
    decision: "approve" | "reject" | "return",
    comment?: string,
  ): Promise<number> {
    const r = await call(page, "POST", `/approvals/${a.approval_id}/decision`, { decision, comment, expected_rev: a.record.rev });
    return r.status;
  },
  async decide(page: Page, a: Approval, decision: "approve" | "reject" | "return", comment?: string): Promise<void> {
    expect(await approvals.decideStatus(page, a, decision, comment)).toBe(202);
  },
};

// ---- ファイル ----

export type NodeItem = { id: string; name: string; kind: string; parent_id: string | null; version: number; created_by: string };

export const files = {
  /// フォルダーの子（読めるものだけ・続きも読む）。読めなければ落とす。
  async list(page: Page, folderId: string): Promise<NodeItem[]> {
    const out: NodeItem[] = [];
    let cursor = "";
    for (let i = 0; i < 20; i++) {
      const r = await ok<{ items: NodeItem[]; next_cursor?: string | null }>(
        page,
        "GET",
        `/nodes?parent_id=${folderId}&limit=200${cursor ? `&cursor=${encodeURIComponent(cursor)}` : ""}`,
      );
      out.push(...r.items);
      if (!r.next_cursor) break;
      cursor = r.next_cursor;
    }
    return out;
  },
  /// 名前が `name` に合うファイルが `count` 個以上になるまで待って返す。
  async waitFor(page: Page, folderId: string, name: RegExp, count = 1, timeout = 60_000): Promise<NodeItem[]> {
    let hit: NodeItem[] = [];
    await expect
      .poll(
        async () => {
          hit = (await files.list(page, folderId)).filter((n) => n.kind === "file" && name.test(n.name));
          return hit.length;
        },
        { timeout, intervals: [1_000, 2_000], message: `${name} のファイルが出る` },
      )
      .toBeGreaterThanOrEqual(count);
    return hit;
  },
  /// テキストのファイルの中身。
  async text(page: Page, fileId: string): Promise<string> {
    return (await ok<{ text: string }>(page, "GET", `/files/${fileId}/text`)).text;
  },
  /// 表のファイル（CSV・Excel）の行（`query` は `?offset=0&limit=100` などをそのまま付ける）。
  async tabularRows(page: Page, fileId: string, query = ""): Promise<Json> {
    return ok<Json>(page, "GET", `/files/${fileId}/tabular/rows${query}`);
  },
};

// ---- 監査 ----

export type AuditEvent = { action: string; decision: string; actor?: string; object_type?: string; object_id?: string; at?: string } & Json;

export const audit = {
  /// 組織の監査ログ（新しい順・管理者か監査の閲覧者の画面で呼ぶ）。
  async events(
    page: Page,
    q: { action?: string; object_type?: string; object_id?: string; actor?: string; limit?: number } = {},
  ): Promise<AuditEvent[]> {
    const p = new URLSearchParams();
    for (const [k, v] of Object.entries(q)) if (v !== undefined) p.set(k, String(v));
    return (await ok<{ events: AuditEvent[] }>(page, "GET", `/org-audit/events?${p}`)).events;
  },
};
