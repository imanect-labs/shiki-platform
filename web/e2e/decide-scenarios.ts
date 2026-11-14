import { readFileSync } from "node:fs";
import path from "node:path";

import { expect, type Page } from "@playwright/test";

import { uniqueName } from "./helpers";

/// 判定の受け入れシナリオ（FEAT-1 の T8・APP-02 問い合わせの仕分け／APP-01 経費の確認）を手元のスタックに組む。
///
/// 表・状態の遷移の規則・規則の行・ワークフロー・同意は、テンプレート（`eval/decision/scenarios/*.json`）が正で、
/// ここには書き写さない。テンプレートの `{{table.<ref>}}` は作った表の id に、それ以外のプレースホルダ
/// （判定用のモデル・部署）は呼び出し側が渡す値に置き換える。作ったものは `Created` に積み、`cleanup` で消す
/// （承認を待つ run を止めてから、ワークフロー・状態の遷移の規則・表の順に消す）。

const EVAL_DIR = path.join(__dirname, "..", "..", "eval", "decision");

export type ScenarioName = "inquiry_triage" | "expense_policy";

type TableTemplate = {
  ref: string;
  name: string;
  schema: Record<string, unknown>;
  fsm?: Record<string, unknown>;
  rows?: Record<string, unknown>[];
};

export type Scenario = {
  scenario: ScenarioName;
  app: string;
  title: string;
  dataset: string;
  theta: number;
  prompt_version: string;
  placeholders: Record<string, string>;
  tables: TableTemplate[];
  grants: { scope: string; table: string; relation: string }[];
  workflow: Record<string, unknown> & { name: string; display_name: string };
};

export function loadScenario(name: ScenarioName): Scenario {
  return JSON.parse(readFileSync(path.join(EVAL_DIR, "scenarios", `${name}.json`), "utf8")) as Scenario;
}

/// 評価データの 1 行（`eval/decision/datasets/*.jsonl`）。
export type DatasetRow = {
  id: string;
  split: string;
  state: Record<string, unknown>;
  labels: Record<string, unknown>;
  tags: Record<string, unknown>;
  rule_flags?: string[];
};

export function loadDataset(name: ScenarioName): DatasetRow[] {
  return readFileSync(path.join(EVAL_DIR, "datasets", `${name}.jsonl`), "utf8")
    .split("\n")
    .filter((l) => l.trim() !== "")
    .map((l) => JSON.parse(l) as DatasetRow);
}

const PLACEHOLDER = /\{\{(?:table|model|role)\.[a-z_]+\}\}/g;

/// 文字列の中のプレースホルダを置き換える（値の全体でも一部でも）。値の無いプレースホルダは止める。
export function fill<T>(v: T, values: Record<string, string>): T {
  if (typeof v === "string") {
    return v.replace(PLACEHOLDER, (p) => {
      const x = values[p];
      if (x === undefined) throw new Error(`値の無いプレースホルダ: ${p}`);
      return x;
    }) as T;
  }
  if (Array.isArray(v)) return v.map((x) => fill(x, values)) as T;
  if (v && typeof v === "object") {
    return Object.fromEntries(Object.entries(v).map(([k, x]) => [k, fill(x, values)])) as T;
  }
  return v;
}

export async function csrfHeaders(page: Page): Promise<Record<string, string>> {
  const csrf = (await page.context().cookies()).find((c) => c.name === "shiki_csrf")?.value ?? "";
  return { "x-csrf-token": csrf, "content-type": "application/json" };
}

/// 組んだもの（片付けに使う・途中で落ちても作った分だけ消す）。
export type Created = {
  tables: Record<string, { id: string; name: string }>;
  fsms: string[];
  workflowId?: string;
};

export function created(): Created {
  return { tables: {}, fsms: [] };
}

/// テンプレートの表・状態の遷移の規則・規則の行・ワークフローを作り、同意して有効化する。
export async function instantiate(
  page: Page,
  s: Scenario,
  values: Record<string, string>,
  out: Created,
): Promise<void> {
  const headers = await csrfHeaders(page);
  const all: Record<string, string> = { ...values };
  const made: Record<string, { schema: Record<string, unknown>; schema_version: number }> = {};
  for (const t of s.tables) {
    const name = uniqueName(`e2e-${t.name}`);
    const res = await page.request.post("/api/data/tables", { headers, data: { name, schema: fill(t.schema, all) } });
    expect(res.status(), await res.text()).toBe(201);
    const table = (await res.json()) as { id: string; schema: Record<string, unknown>; schema_version: number };
    out.tables[t.ref] = { id: table.id, name };
    made[t.ref] = table;
    all[`{{table.${t.ref}}}`] = table.id;
  }
  for (const t of s.tables) {
    const id = out.tables[t.ref].id;
    if (t.fsm) {
      const fsm = await page.request.post("/api/data/fsms", {
        headers,
        data: { name: uniqueName(`e2e-${t.name}の状態`), table_id: id, body: fill(t.fsm, all) },
      });
      expect(fsm.status(), await fsm.text()).toBe(201);
      const f = (await fsm.json()) as { id: string; version?: number };
      out.fsms.push(f.id);
      const put = await page.request.put(`/api/data/tables/${id}/schema`, {
        headers,
        data: {
          schema: { ...made[t.ref].schema, fsm_ref: { artifact_id: f.id, version: f.version ?? 1 } },
          expected_schema_version: made[t.ref].schema_version,
        },
      });
      expect(put.status(), await put.text()).toBe(200);
    }
    for (const row of t.rows ?? []) {
      const res = await page.request.post(`/api/data/tables/${id}/records`, { headers, data: { data: fill(row, all) } });
      expect(res.status(), await res.text()).toBe(201);
    }
  }
  const ir = { ...fill(s.workflow, all), name: uniqueName(s.workflow.name) };
  const flow = await page.request.post("/api/workflows", { headers, data: { ir } });
  expect(flow.ok(), await flow.text()).toBeTruthy();
  out.workflowId = ((await flow.json()) as { id: string }).id;
  const enabled = await page.request.post(`/api/workflows/${out.workflowId}/enable`, {
    headers,
    data: {
      version: 1,
      grants: s.grants.map((g) => ({
        scope: g.scope,
        object_type: "data_table",
        object_id: out.tables[g.table].id,
        relation: g.relation,
      })),
    },
  });
  expect(enabled.ok(), await enabled.text()).toBeTruthy();
}

type RunItem = { run_id: string; status: string };

async function listRuns(page: Page, workflowId: string, status?: string): Promise<RunItem[]> {
  const q = status ? `?status=${status}&limit=100` : "?limit=100";
  const res = await page.request.get(`/api/workflows/${workflowId}/runs${q}`);
  if (!res.ok()) return [];
  return ((await res.json()) as { items: RunItem[] }).items;
}

/// 承認を待つ run を止め、ワークフロー・状態の遷移の規則・表を消す（消せないものは落とす）。
export async function cleanup(page: Page, out: Created): Promise<void> {
  const headers = await csrfHeaders(page);
  if (out.workflowId) {
    for (let i = 0; i < 5; i++) {
      const live = await listRuns(page, out.workflowId, "queued,running");
      if (live.length === 0) break;
      for (const r of live) {
        await page.request.post(`/api/workflows/${out.workflowId}/runs/${r.run_id}/cancel`, { headers });
      }
    }
    const res = await page.request.delete(`/api/artifacts/${out.workflowId}`, { headers });
    expect([204, 404], `DELETE /artifacts/${out.workflowId} → ${res.status()}`).toContain(res.status());
  }
  for (const id of out.fsms) {
    const res = await page.request.delete(`/api/artifacts/${id}`, { headers });
    expect([204, 404], `DELETE /artifacts/${id} → ${res.status()}`).toContain(res.status());
  }
  for (const t of Object.values(out.tables)) {
    const res = await page.request.delete(`/api/data/tables/${t.id}`, { headers });
    expect([200, 204, 404], `DELETE /data/tables/${t.id} → ${res.status()}`).toContain(res.status());
  }
}

export type DataRecord = { id: string; rev: number; data: Record<string, unknown> };

export async function addRecord(page: Page, tableId: string, data: Record<string, unknown>): Promise<DataRecord> {
  const res = await page.request.post(`/api/data/tables/${tableId}/records`, {
    headers: await csrfHeaders(page),
    data: { data },
  });
  expect(res.status(), await res.text()).toBe(201);
  return (await res.json()) as DataRecord;
}

/// 行の列を書き換える（応答の状態コードと本文を返す・権限の試験に使う）。
export async function patchRecord(
  page: Page,
  tableId: string,
  rec: DataRecord,
  data: Record<string, unknown>,
): Promise<{ status: number; record?: DataRecord }> {
  const res = await page.request.patch(`/api/data/tables/${tableId}/records/${rec.id}`, {
    headers: await csrfHeaders(page),
    data: { patch: data, expected_rev: rec.rev },
  });
  return { status: res.status(), record: res.ok() ? ((await res.json()) as DataRecord) : undefined };
}

/// 行の状態を進める（状態の遷移の API・応答の状態コードを返す）。
export async function transitionRecord(page: Page, tableId: string, rec: DataRecord, to: string): Promise<number> {
  const res = await page.request.post(`/api/data/tables/${tableId}/records/${rec.id}/transition`, {
    headers: await csrfHeaders(page),
    data: { to, expected_rev: rec.rev },
  });
  return res.status();
}

/// テンプレートの規程の script（`rules` ノード）を、ワークフローの外で関数として読む（評価データの rule_flags・
/// 規程の例と照らすため）。script は `main(input)` を定義する素の JavaScript。
export function expenseRulesScript(s: Scenario): (input: Record<string, unknown>) => {
  flags: string[];
  state: Record<string, unknown>;
  applicant: unknown;
  approver: unknown;
} {
  const nodes = (s.workflow as unknown as { nodes: { id: string; params: { source?: { inline?: string } } }[] }).nodes;
  const src = nodes.find((n) => n.id === "rules")?.params.source?.inline;
  if (!src) throw new Error("rules の script が無い");
  return new Function(`${src}\nreturn main;`)() as ReturnType<typeof expenseRulesScript>;
}

export async function getRecord(page: Page, tableId: string, id: string): Promise<DataRecord> {
  const res = await page.request.get(`/api/data/tables/${tableId}/records/${id}`);
  expect(res.ok(), await res.text()).toBeTruthy();
  return (await res.json()) as DataRecord;
}

/// 行の状態が `states` のどれかになるまで待つ（ワークフローが自動で進める分の終わり）。
export async function waitForStatus(
  page: Page,
  tableId: string,
  id: string,
  states: string[],
  timeout = 90_000,
): Promise<DataRecord> {
  let rec: DataRecord | undefined;
  await expect
    .poll(
      async () => {
        rec = await getRecord(page, tableId, id);
        return String(rec.data.status ?? "");
      },
      { timeout, intervals: [1_000, 2_000] },
    )
    .toMatch(new RegExp(`^(${states.join("|")})$`));
  return rec!;
}

export type RunStep = { node_id: string; status: string; taken_ports: string[]; error?: { code?: string } | null };
export type RunDetail = {
  run_id: string;
  status: string;
  input: { record_id?: string };
  steps: RunStep[];
};

/// ワークフローの run を、起こした行の id で引けるようにする（run の入力の record_id）。
export async function runsByRecord(page: Page, workflowId: string): Promise<Map<string, RunDetail>> {
  const out = new Map<string, RunDetail>();
  for (const r of await listRuns(page, workflowId)) {
    const res = await page.request.get(`/api/workflows/${workflowId}/runs/${r.run_id}`);
    if (!res.ok()) continue;
    const d = (await res.json()) as RunDetail;
    if (d.input.record_id) out.set(d.input.record_id, d);
  }
  return out;
}

export async function stepOutput<T>(page: Page, workflowId: string, runId: string, node: string): Promise<T> {
  const res = await page.request.get(`/api/workflows/${workflowId}/runs/${runId}/steps?path=${node}`);
  expect(res.ok(), await res.text()).toBeTruthy();
  return ((await res.json()) as { output: T }).output;
}

/// 評価データの経費の申請（判定の材料の形）を、経費の表の 1 行にする（申請者が入れる列だけ。申請者と上長の列は
/// ワークフローが行の作成者と承認者の表から入れる）。
export function expenseRecord(state: Record<string, unknown>): Record<string, unknown> {
  const used = String(state["利用日"]).slice(0, 10);
  const data: Record<string, unknown> = {
    title: `${state["費目"]} ${state["品名・内容"]}`,
    applicant_name: state["申請者"],
    department: state["部署"],
    used_on: used,
    category: state["費目"],
    amount: state["金額"],
    payee: state["支払先"],
    item: state["品名・内容"],
    purpose: state["摘要"],
    receipt: state["領収書"],
  };
  if (state["人数"] !== undefined) data.people = state["人数"];
  if (state["参加者"] !== undefined) data.attendees = state["参加者"];
  return data;
}

/// 評価データの問い合わせ（判定の材料の形）を、問い合わせの表の 1 行にする。
export function inquiryRecord(state: Record<string, unknown>): Record<string, unknown> {
  return { subject: state["件名"], body: state["本文"], sender: state["差出人"] };
}

/// 画面を撮る（PC 1440px とスマホ 390px）: ワークフローの判定のノードの出口・実行履歴（`runId` の詳細も）・
/// 受信箱（`inbox` のとき）。スマホはログインし直した別の文脈で撮り、横にはみ出さないことも確かめる。
export async function shootViews(
  page: Page,
  phone: Page,
  dir: string,
  prefix: string,
  opts: { workflowId: string; decideNode: string; runId?: string; inbox?: boolean; tableId?: string },
): Promise<void> {
  const shot = (p: Page, name: string) => p.screenshot({ path: path.join(dir, `${prefix}-${name}.png`) });
  await page.setViewportSize({ width: 1440, height: 1000 });
  await page.goto(`/workflows/${opts.workflowId}`);
  const node = page.getByTestId(`rf__node-${opts.decideNode}`);
  await expect(node).toBeVisible({ timeout: 20_000 });
  await expect(node.locator('[data-port="uncertain"]')).toHaveText("要確認", { timeout: 10_000 });
  await shot(page, "editor-pc");
  await node.click();
  await expect(page.getByRole("complementary", { name: "ブロックの設定" }).getByTestId("decide-exits")).toBeVisible();
  await shot(page, "decide-panel-pc");

  await phone.goto(`/workflows/${opts.workflowId}`);
  const flow = phone.getByTestId("workflow-mobile-flow");
  await expect(flow).toBeVisible({ timeout: 20_000 });
  await expect(flow.getByText("要確認", { exact: true }).first()).toBeVisible({ timeout: 10_000 });
  expect(await phone.evaluate(() => document.documentElement.scrollWidth <= window.innerWidth + 1)).toBe(true);
  await shot(phone, "editor-phone");

  const runs = `/workflows/${opts.workflowId}/runs${opts.runId ? `?run=${opts.runId}` : ""}`;
  // 詳細は step の一覧（ブロックごとの記録）が読み込まれるまで待つ。
  const loaded = (p: Page) =>
    opts.runId
      ? expect(p.locator('[role="dialog"]').getByText("ブロックごとの記録")).toBeVisible({ timeout: 20_000 })
      : expect(p.locator("table")).toBeVisible({ timeout: 20_000 });
  await page.goto(runs);
  await loaded(page);
  await shot(page, "runs-pc");
  await phone.goto(runs);
  await loaded(phone);
  expect(await phone.evaluate(() => document.documentElement.scrollWidth <= window.innerWidth + 1)).toBe(true);
  await shot(phone, "runs-phone");

  if (opts.tableId) {
    await page.goto(`/data/${opts.tableId}`);
    await expect(page.getByTestId("data-grid")).toBeVisible({ timeout: 20_000 });
    await shot(page, "table-pc");
  }
  if (opts.inbox) {
    await page.goto("/approvals");
    await expect(page.getByTestId("approval-item").first()).toBeVisible({ timeout: 20_000 });
    await shot(page, "inbox-pc");
    await phone.goto("/approvals");
    await expect(phone.getByTestId("approval-item").first()).toBeVisible({ timeout: 20_000 });
    expect(await phone.evaluate(() => document.documentElement.scrollWidth <= window.innerWidth + 1)).toBe(true);
    await shot(phone, "inbox-phone");
  }
}
