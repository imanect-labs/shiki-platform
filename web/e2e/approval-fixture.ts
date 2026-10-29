import { expect, type Page } from "@playwright/test";

import { uniqueName } from "./helpers";

/// 承認待ち（human.approval・GAP-08）の e2e が共有する下ごしらえ（approvals.spec.ts・workflow-delete-stops.spec.ts）:
/// 申請の表（状態の遷移の規則つき）と、申請中になった行を受け付けて承認を待つワークフローを作り、1 件申請する。
/// 承認者は申請者と同じ alice（テーブルの共有は e2e の外・状態の遷移の規則は「承認者の列の人」）。

export async function csrfHeaders(page: Page): Promise<Record<string, string>> {
  const csrf = (await page.context().cookies()).find((c) => c.name === "shiki_csrf")?.value ?? "";
  return { "x-csrf-token": csrf, "content-type": "application/json" };
}

export type Setup = {
  tableId: string;
  tableName: string;
  workflowId: string;
  /// ワークフローの名前（一覧に出る表示名と同じ）。
  workflowName: string;
  recordId: string;
  title: string;
  fsmId: string;
};

/// 申請の表（下書き → 申請中 → 受付 は誰でも、受付 → 承認・却下 は承認者の列の人）と、申請中になった行を受け付けて
/// 承認を待つワークフローを作り、1 件申請する。
export async function setup(page: Page, runTimeoutSec = 1209600): Promise<Setup> {
  const headers = await csrfHeaders(page);
  const me = (await (await page.request.get("/api/me")).json()) as { id: string };
  const states = ["下書き", "申請中", "受付", "承認", "却下"];
  const tableName = uniqueName("e2e-経費申請");
  const created = await page.request.post("/api/data/tables", {
    headers,
    data: {
      name: tableName,
      schema: {
        fields: [
          { key: "title", display_name: "件名", type: "text", indexed: true },
          { key: "amount", display_name: "金額", type: "number" },
          { key: "approver", display_name: "承認者", type: "user_ref", indexed: true },
          { key: "status", display_name: "状態", type: "select", options: states, indexed: true },
        ],
        status_field: "status",
      },
    },
  });
  expect(created.status(), await created.text()).toBe(201);
  const table = (await created.json()) as { id: string; schema: Record<string, unknown>; schema_version: number };
  const approverOnly = { field_cmp: { field: "approver", op: "eq", value: "user_id" } };
  const fsm = await page.request.post("/api/data/fsms", {
    headers,
    data: {
      name: uniqueName("e2e-承認の流れ"),
      table_id: table.id,
      body: {
        states,
        transitions: [
          { from: "下書き", to: "申請中", actor: "public" },
          { from: "申請中", to: "受付", actor: "public" },
          { from: "受付", to: "承認", actor: approverOnly },
          { from: "受付", to: "却下", actor: approverOnly },
        ],
      },
    },
  });
  expect(fsm.status(), await fsm.text()).toBe(201);
  const f = (await fsm.json()) as { id: string; version?: number };
  const put = await page.request.put(`/api/data/tables/${table.id}/schema`, {
    headers,
    data: {
      schema: { ...table.schema, fsm_ref: { artifact_id: f.id, version: f.version ?? 1 } },
      expected_schema_version: table.schema_version,
    },
  });
  expect(put.status(), await put.text()).toBe(200);

  const name = uniqueName("expense-approval");
  const flow = await page.request.post("/api/workflows", {
    headers,
    data: {
      ir: {
        ir_version: 1,
        name,
        display_name: name,
        triggers: [
          {
            kind: "event",
            source: "data.transition",
            scope: { table: table.id },
            filter: { cmp: { left: { $from: "trigger", path: "/to" }, op: "eq", right: "申請中" } },
          },
        ],
        nodes: [
          {
            id: "accept",
            type: "data.transition",
            label: "受け付ける",
            params: { table: table.id, record: { $from: "trigger", path: "/record_id" }, to: "受付" },
          },
          {
            id: "ok",
            type: "human.approval",
            label: "上長の承認",
            params: {
              title: "経費申請の承認",
              instructions: "規程どおりか確認してください。",
              record: { table: table.id, id: { $from: "trigger", path: "/record_id" } },
              assignees: [{ field: "approver" }],
              approve: { to: "承認" },
              reject: { to: "却下", comment: "required" },
              show_fields: ["title", "amount"],
            },
          },
          { id: "done", type: "control.join", label: "承認された", params: {} },
          { id: "ng", type: "control.join", label: "却下された", params: {} },
        ],
        edges: [
          { from: "accept", to: "ok" },
          { from: "ok", from_port: "approved", to: "done" },
          { from: "ok", from_port: "rejected", to: "ng" },
        ],
        policies: { run_timeout_sec: runTimeoutSec },
      },
    },
  });
  expect(flow.ok(), await flow.text()).toBeTruthy();
  const workflowId = ((await flow.json()) as { id: string }).id;
  const enabled = await page.request.post(`/api/workflows/${workflowId}/enable`, {
    headers,
    data: {
      version: 1,
      grants: [
        { scope: "data.read", object_type: "data_table", object_id: table.id, relation: "viewer" },
        { scope: "data.write", object_type: "data_table", object_id: table.id, relation: "editor" },
      ],
    },
  });
  expect(enabled.ok(), await enabled.text()).toBeTruthy();

  // 申請する（下書きを作って申請中へ進める）。
  const title = uniqueName("出張旅費");
  const rec = await page.request.post(`/api/data/tables/${table.id}/records`, {
    headers,
    data: { data: { title, amount: 12000, approver: me.id, status: "下書き" } },
  });
  expect(rec.status(), await rec.text()).toBe(201);
  const record = (await rec.json()) as { id: string; rev: number };
  const moved = await page.request.post(`/api/data/tables/${table.id}/records/${record.id}/transition`, {
    headers,
    data: { to: "申請中", expected_rev: record.rev },
  });
  expect(moved.status(), await moved.text()).toBe(200);
  return { tableId: table.id, tableName, workflowId, workflowName: name, recordId: record.id, title, fsmId: f.id };
}

/// ワークフローが受け付けて、承認を待つまで（relay の tick 5 秒）。
export async function waitForApproval(page: Page, s: Setup): Promise<void> {
  await expect
    .poll(
      async () => {
        const list = (await (await page.request.get("/api/approvals")).json()) as {
          items: { record: { id: string } }[];
        };
        return list.items.some((i) => i.record.id === s.recordId);
      },
      { timeout: 60_000, intervals: [1_000] },
    )
    .toBe(true);
}

export async function runOf(page: Page, s: Setup): Promise<string> {
  const list = (await (await page.request.get(`/api/workflows/${s.workflowId}/runs`)).json()) as {
    items: { run_id: string }[];
  };
  expect(list.items.length).toBeGreaterThan(0);
  return list.items[0].run_id;
}
