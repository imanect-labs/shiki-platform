import { expect, test, type Page } from "@playwright/test";

import { loginViaKeycloak, uniqueName } from "./helpers";

/// 承認待ち（human.approval・GAP-08）の e2e:
/// - 申請（テーブルの行を申請へ進める）→ ワークフローが受け付けて承認を待つ → 承認者が /approvals の受信箱から
///   承認する → run が approved の出口へ進む（裏取りは run の step の出口と、表の行の状態）。
/// - サイドバーの「承認」に件数が出る。スマホ幅でも受信箱が横にはみ出さず、全画面のシートで判断できる。
/// 前提: shiki-server が SHIKI__WORKFLOW__ENABLED=true・LLM は stub。承認者は申請者と同じ alice
/// （テーブルの共有は e2e の外・状態の遷移の規則は「承認者の列の人」）。

async function csrfHeaders(page: Page): Promise<Record<string, string>> {
  const csrf = (await page.context().cookies()).find((c) => c.name === "shiki_csrf")?.value ?? "";
  return { "x-csrf-token": csrf, "content-type": "application/json" };
}

type Setup = { tableId: string; tableName: string; workflowId: string; recordId: string; title: string };

/// 申請の表（下書き → 申請中 → 受付 は誰でも、受付 → 承認・却下 は承認者の列の人）と、申請中になった行を受け付けて
/// 承認を待つワークフローを作り、1 件申請する。
async function setup(page: Page, runTimeoutSec = 1209600): Promise<Setup> {
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
  return { tableId: table.id, tableName, workflowId, recordId: record.id, title };
}

/// ワークフローが受け付けて、承認を待つまで（relay の tick 5 秒）。
async function waitForApproval(page: Page, s: Setup): Promise<void> {
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

async function runOf(page: Page, s: Setup): Promise<string> {
  const list = (await (await page.request.get(`/api/workflows/${s.workflowId}/runs`)).json()) as {
    items: { run_id: string }[];
  };
  expect(list.items.length).toBeGreaterThan(0);
  return list.items[0].run_id;
}

test("承認待ち: 申請 → 受信箱で承認 → ワークフローが「承認」の出口へ進む（GAP-08）", async ({ page }) => {
  test.setTimeout(180_000);
  await loginViaKeycloak(page);
  const s = await setup(page);
  await waitForApproval(page, s);

  // サイドバーの「承認」に件数が出る（読み上げのラベルに件数を含める）。
  await page.goto("/approvals");
  await expect(page.getByRole("link", { name: /承認（\d+ 件）/ }).or(page.getByRole("button", { name: /承認（\d+ 件）/ })).first()).toBeVisible({
    timeout: 15_000,
  });
  const item = page.getByTestId("approval-item").filter({ hasText: s.tableName });
  await expect(item).toBeVisible();
  await expect(item).toContainText("経費申請の承認");
  await expect(item).toContainText("状態: 受付");
  await expect(item).toContainText(s.title);

  // 詳細のシートで判断する。判断のボタンには進める状態が出る。却下はコメントが無いと送れない。
  await item.click();
  const sheet = page.getByTestId("approval-sheet");
  await expect(sheet).toBeVisible();
  await expect(sheet).toContainText("規程どおりか確認してください。");
  await expect(sheet.getByText("12,000")).toBeVisible();
  const reject = sheet.getByRole("radio", { name: /却下する/ });
  await expect(reject).toContainText("状態を「却下」へ（コメント必須）");
  await reject.click();
  await expect(sheet.getByTestId("approval-send")).toBeDisabled();
  const approve = sheet.getByRole("radio", { name: /承認する/ });
  await expect(approve).toContainText("状態を「承認」へ");
  await approve.click();
  await sheet.getByLabel(/コメント/).fill("規程どおりです");
  const decided = page.waitForResponse((r) => r.url().includes("/decision") && r.request().method() === "POST");
  // Ctrl+Enter で送る。
  await sheet.getByLabel(/コメント/).press("Control+Enter");
  expect((await decided).status()).toBe(202);
  await expect(sheet).toBeHidden();
  // 判断した項目は一覧から消える（読み直してもサーバの状態で出ない）。
  await expect(page.getByTestId("approval-item").filter({ hasText: s.tableName })).toHaveCount(0);

  // run が「承認」の出口へ進み（承認の step の出力に判断とコメント）、表の行は「承認」。
  const runId = await runOf(page, s);
  await expect
    .poll(
      async () => {
        const run = (await (await page.request.get(`/api/workflows/${s.workflowId}/runs/${runId}`)).json()) as {
          status: string;
          steps: { node_id: string; status: string; taken_ports: string[] }[];
        };
        const ok = run.steps.find((st) => st.node_id === "ok");
        return `${run.status}:${ok?.status}:${ok?.taken_ports.join(",")}`;
      },
      { timeout: 60_000, intervals: [1_000] },
    )
    .toBe("succeeded:succeeded:approved");
  const step = (await (
    await page.request.get(`/api/workflows/${s.workflowId}/runs/${runId}/steps?path=ok`)
  ).json()) as { output: { decision: string; comment: string; to: string } };
  expect(step.output).toMatchObject({ decision: "approve", comment: "規程どおりです", to: "承認" });
  const row = (await (await page.request.get(`/api/data/tables/${s.tableId}/records/${s.recordId}`)).json()) as {
    data: { status: string };
  };
  expect(row.data.status).toBe("承認");
});

test("承認待ち: スマホ幅の受信箱は横にはみ出さず、全画面のシートで判断できる（GAP-08）", async ({ browser }) => {
  test.setTimeout(180_000);
  const ctx = await browser.newContext({ viewport: { width: 390, height: 844 }, hasTouch: true, isMobile: true });
  const page = await ctx.newPage();
  await loginViaKeycloak(page);
  const s = await setup(page);
  await waitForApproval(page, s);

  await page.goto("/approvals");
  const item = page.getByTestId("approval-item").filter({ hasText: s.tableName });
  await expect(item).toBeVisible({ timeout: 15_000 });
  const width = await page.evaluate(() => document.documentElement.scrollWidth);
  expect(width).toBeLessThanOrEqual(390);

  await item.click();
  const sheet = page.getByTestId("approval-sheet");
  await expect(sheet).toBeVisible();
  const box = (await sheet.boundingBox())!;
  expect(box.width).toBeGreaterThanOrEqual(389);
  expect(box.height).toBeGreaterThanOrEqual(800);
  await sheet.getByRole("radio", { name: /却下する/ }).click();
  await sheet.getByLabel(/コメント/).fill("領収書が足りません");
  await sheet.getByTestId("approval-send").click();
  await expect(sheet).toBeHidden();

  const runId = await runOf(page, s);
  await expect
    .poll(
      async () => {
        const run = (await (await page.request.get(`/api/workflows/${s.workflowId}/runs/${runId}`)).json()) as {
          steps: { node_id: string; taken_ports: string[] }[];
        };
        return run.steps.find((st) => st.node_id === "ok")?.taken_ports.join(",") ?? "";
      },
      { timeout: 60_000, intervals: [1_000] },
    )
    .toBe("rejected");

  // スマホのドロワのナビにも「承認」が出る。
  await page.getByRole("button", { name: "メニューを開く" }).click();
  const nav = page.getByRole("navigation", { name: "メインナビゲーション" });
  await expect(nav.getByRole("link", { name: /^承認/ })).toBeVisible();
  const drawerWidth = await page.evaluate(() => document.documentElement.scrollWidth);
  expect(drawerWidth).toBeLessThanOrEqual(390);
  await ctx.close();
});

test("承認を待つブロック: テーブルの状態から行き先を選べ、実行の期限を 14 日に延ばす提案が出る（GAP-08）", async ({ page }) => {
  test.setTimeout(120_000);
  await loginViaKeycloak(page);
  // 実行の期限は既定の 3 日のまま作る（承認を待つ間に切れやすい）。
  const s = await setup(page, 259200);
  await page.goto(`/workflows/${s.workflowId}`);
  const canvas = page.locator(".react-flow");
  await expect(canvas.getByText("上長の承認").first()).toBeVisible({ timeout: 15_000 });
  // 出口は承認・却下・取り下げ（差し戻しは指定していないので出ない）。
  await expect(canvas.getByText("取り下げ").first()).toBeVisible();
  await expect(canvas.getByText("差し戻し")).toHaveCount(0);

  await canvas.getByText("上長の承認").first().click();
  const panel = page.getByRole("complementary", { name: "ブロックの設定" });
  await expect(panel.getByRole("combobox", { name: "承認するテーブル" })).toContainText(s.tableName);
  // 行き先の状態はテーブルの状態から選ぶ。
  await expect(panel.getByRole("combobox", { name: "承認したときに進める状態" })).toContainText("承認");
  // 差し戻しを足すと出口が増える。
  await panel.getByRole("switch", { name: "差し戻したときの出口を使う" }).click();
  await panel.getByRole("combobox", { name: "差し戻したときに進める状態" }).click();
  await page.getByRole("option", { name: "下書き" }).click();
  await expect(canvas.getByText("差し戻し").first()).toBeVisible();

  // 実行の期限を延ばす提案から 14 日にして保存する。
  const suggestion = panel.getByTestId("approval-run-timeout");
  await expect(suggestion).toContainText("実行の期限はいま 3 日です");
  await suggestion.getByRole("button", { name: "実行の期限を 14 日にする" }).click();
  await expect(suggestion).toHaveCount(0);
  await page.getByRole("button", { name: "保存", exact: true }).click();
  await expect(page.getByText(/保存しました/).first()).toBeVisible({ timeout: 15_000 });
  const saved = (await (await page.request.get(`/api/workflows/${s.workflowId}`)).json()) as {
    ir: { policies: { run_timeout_sec: number }; nodes: { id: string; params: Record<string, unknown> }[] };
  };
  expect(saved.ir.policies.run_timeout_sec).toBe(1209600);
  expect(saved.ir.nodes.find((n) => n.id === "ok")?.params.return).toEqual({ to: "下書き" });
});
