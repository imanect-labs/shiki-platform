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
  // 出口は承認・却下・取り下げ（差し戻しは指定していないので出ない）。出口のラベルは完全一致で数える
  // （ブロックの説明「…承認・却下・差し戻しを待って分岐する」も部分一致で拾ってしまう）。
  await expect(canvas.getByText("取り下げ", { exact: true }).first()).toBeVisible();
  await expect(canvas.getByText("差し戻し", { exact: true })).toHaveCount(0);

  await canvas.getByText("上長の承認").first().click();
  const panel = page.getByRole("complementary", { name: "ブロックの設定" });
  await expect(panel.getByRole("combobox", { name: "承認するテーブル" })).toContainText(s.tableName);
  // 行き先の状態はテーブルの状態から選ぶ。
  await expect(panel.getByRole("combobox", { name: "承認したときに進める状態" })).toContainText("承認");
  // 差し戻しを足すと出口が増える。
  await panel.getByRole("switch", { name: "差し戻したときの出口を使う" }).click();
  await panel.getByRole("combobox", { name: "差し戻したときに進める状態" }).click();
  await page.getByRole("option", { name: "下書き" }).click();
  await expect(canvas.getByText("差し戻し", { exact: true })).toHaveCount(1);

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

/// 状態の遷移の規則（FSM）で管理する別のテーブル（状態の名前・列が申請の表と違う）。
async function otherFsmTable(page: Page): Promise<{ id: string; name: string }> {
  const headers = await csrfHeaders(page);
  const states = ["受付中", "対応済み", "差し戻し中"];
  const name = uniqueName("e2e-問い合わせ");
  const created = await page.request.post("/api/data/tables", {
    headers,
    data: {
      name,
      schema: {
        fields: [
          { key: "title", display_name: "件名", type: "text" },
          { key: "owner_user", display_name: "担当者", type: "user_ref" },
          { key: "status", display_name: "状態", type: "select", options: states },
        ],
        status_field: "status",
      },
    },
  });
  expect(created.status(), await created.text()).toBe(201);
  const table = (await created.json()) as { id: string; schema: Record<string, unknown>; schema_version: number };
  const fsm = await page.request.post("/api/data/fsms", {
    headers,
    data: {
      name: uniqueName("e2e-問い合わせの流れ"),
      table_id: table.id,
      body: {
        states,
        transitions: [
          { from: "受付中", to: "対応済み", actor: "public" },
          { from: "受付中", to: "差し戻し中", actor: "public" },
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
  return { id: table.id, name };
}

test("承認を待つブロック: テーブルを変えると前のテーブルの状態・列が残らず、Undo 1 回で戻る（GAP-08）", async ({ page }) => {
  test.setTimeout(120_000);
  await loginViaKeycloak(page);
  const s = await setup(page);
  const other = await otherFsmTable(page);
  await page.goto(`/workflows/${s.workflowId}`);
  const canvas = page.locator(".react-flow");
  await expect(canvas.getByText("上長の承認").first()).toBeVisible({ timeout: 15_000 });
  await canvas.getByText("上長の承認").first().click();
  const panel = page.getByRole("complementary", { name: "ブロックの設定" });
  const approveTo = panel.getByRole("combobox", { name: "承認したときに進める状態" });
  const rejectTo = panel.getByRole("combobox", { name: "却下したときに進める状態" });
  const assignee = panel.getByRole("combobox", { name: "相手 1 の列" });
  await expect(approveTo).toContainText("承認");
  await expect(assignee).toContainText("承認者");

  // 状態も列も違うテーブルへ変える: 判断の行き先・担当者の列は空に戻り、両方にある列（件名）だけ残る。
  await panel.getByRole("combobox", { name: "承認するテーブル" }).click();
  await page.getByRole("option", { name: other.name }).click();
  await expect(approveTo).toContainText("状態を選ぶ");
  await expect(rejectTo).toContainText("状態を選ぶ");
  await expect(assignee).toContainText("列を選ぶ");
  await expect(panel.getByText(/このテーブルに無い(状態|列)/)).toHaveCount(0);
  await expect(panel.getByRole("button", { name: "件名", pressed: true })).toBeVisible();
  await expect(panel.getByRole("button", { name: "金額" })).toHaveCount(0);

  // テーブルの変更は 1 手（Undo 1 回で、前のテーブルと行き先・列が揃って戻る）。
  await page.locator(".react-flow__pane").click({ position: { x: 24, y: 24 } });
  await page.keyboard.press("Control+z");
  await canvas.getByText("上長の承認").first().click();
  await expect(panel.getByRole("combobox", { name: "承認するテーブル" })).toContainText(s.tableName);
  await expect(approveTo).toContainText("承認");
  await expect(assignee).toContainText("承認者");

  // もう一度変えて、新しいテーブルの状態・列を選んで保存する。
  await panel.getByRole("combobox", { name: "承認するテーブル" }).click();
  await page.getByRole("option", { name: other.name }).click();
  await approveTo.click();
  await page.getByRole("option", { name: "対応済み" }).click();
  await rejectTo.click();
  await page.getByRole("option", { name: "差し戻し中" }).click();
  await assignee.click();
  await page.getByRole("option", { name: "担当者" }).click();
  await page.getByRole("button", { name: "保存", exact: true }).click();
  await expect(page.getByText(/保存しました/).first()).toBeVisible({ timeout: 15_000 });
  const saved = (await (await page.request.get(`/api/workflows/${s.workflowId}`)).json()) as {
    ir: { nodes: { id: string; params: Record<string, unknown> }[] };
  };
  const params = saved.ir.nodes.find((n) => n.id === "ok")?.params ?? {};
  expect(params.record).toMatchObject({ table: other.id });
  expect(params.approve).toEqual({ to: "対応済み" });
  expect(params.reject).toEqual({ to: "差し戻し中", comment: "required" });
  expect(params.assignees).toEqual([{ field: "owner_user" }]);
  expect(params.show_fields).toEqual(["title"]);
});

test("承認を待つブロック: テーブルに無い受信箱の列は外すボタン付きで出し、保存の誤りも欄に出す（GAP-08）", async ({ page }) => {
  test.setTimeout(120_000);
  await loginViaKeycloak(page);
  const s = await setup(page);
  // テーブルに無い列を show_fields に持つ版を開く（AI が書いた・検証が入る前に保存した）。列の削除はテーブルの
  // 設定が拒み、そういう IR の保存は検証が止めるので、エディタが読み込む IR だけを書き換える。
  let injected = false;
  await page.route(`**/api/workflows/${s.workflowId}`, async (route) => {
    if (injected || route.request().method() !== "GET") return route.continue();
    injected = true;
    const res = await route.fetch();
    const body = (await res.json()) as { ir: { nodes: { id: string; params: Record<string, unknown> }[] } };
    const node = body.ir.nodes.find((n) => n.id === "ok");
    if (node) node.params.show_fields = ["title", "ghost", "amount"];
    await route.fulfill({ response: res, json: body });
  });
  await page.goto(`/workflows/${s.workflowId}`);
  const canvas = page.locator(".react-flow");
  await expect(canvas.getByText("上長の承認").first()).toBeVisible({ timeout: 15_000 });
  expect(injected).toBe(true);
  await canvas.getByText("上長の承認").first().click();
  const panel = page.getByRole("complementary", { name: "ブロックの設定" });
  // 選べる列のボタンには出ない列を、外すボタン付きで出す。
  await expect(panel.getByRole("button", { name: "件名", pressed: true })).toBeVisible();
  await expect(panel.getByRole("button", { name: "金額", pressed: true })).toBeVisible();
  const stray = panel.getByRole("list", { name: "選べない列" });
  await expect(stray).toContainText("このテーブルに無い列: ghost");

  // ほかの欄を直して保存すると、サーバの検証（ir.unknown_field）で止まり、理由が受信箱に出す列の欄に出る。
  await panel.getByPlaceholder("例: 経費申請の承認").fill("経費申請の承認（課長）");
  await page.getByRole("button", { name: "保存", exact: true }).click();
  await expect(panel.getByText("列 ghost はこのテーブルにありません")).toBeVisible({ timeout: 15_000 });

  // 外すと消え、保存できる（ほかの列は残る）。
  await stray.getByRole("button", { name: "列 ghost を外す" }).click();
  await expect(stray).toHaveCount(0);
  await expect(panel.getByRole("button", { name: "金額", pressed: true })).toBeVisible();
  await page.getByRole("button", { name: "保存", exact: true }).click();
  await expect(page.getByText(/保存しました/).first()).toBeVisible({ timeout: 15_000 });
  const saved = (await (await page.request.get(`/api/workflows/${s.workflowId}`)).json()) as {
    ir: { nodes: { id: string; params: Record<string, unknown> }[] };
  };
  expect(saved.ir.nodes.find((n) => n.id === "ok")?.params.show_fields).toEqual(["title", "amount"]);
});

test("承認待ち: 見える承認が無いまま続きがあるページでも「承認待ちはありません」にせず、続きを読める（GAP-08）", async ({ page }) => {
  test.setTimeout(180_000);
  await loginViaKeycloak(page);
  const s = await setup(page);
  await waitForApproval(page, s);
  // サーバは見えない承認を読み飛ばすが、1 回に読む台帳の行には上限があるので、見える承認が無いまま続きの位置
  // だけを返すことがある。1 ページ目をその形にし、続きは本物の 1 ページ目を返す。件数も数え切れなかった形にする。
  const cursor = "e2e-scan-limit";
  await page.route(
    (url) => url.pathname === "/api/approvals",
    async (route) => {
      const url = new URL(route.request().url());
      if (url.searchParams.get("cursor") === cursor) {
        url.searchParams.delete("cursor");
        return route.continue({ url: url.toString() });
      }
      return route.fulfill({ json: { items: [], next_cursor: cursor } });
    },
  );
  await page.route(
    (url) => url.pathname === "/api/approvals/count",
    (route) => route.fulfill({ json: { count: 0, more: true } }),
  );
  await page.goto("/approvals");
  await expect(page.getByTestId("approval-list-pending")).toBeVisible({ timeout: 15_000 });
  await expect(page.getByText("承認待ちはありません", { exact: true })).toHaveCount(0);
  // 数え切れなかった件数は 0 として隠さない。
  await expect(
    page.getByRole("button", { name: "承認（0+ 件）" }).or(page.getByRole("link", { name: "承認（0+ 件）" })).first(),
  ).toBeVisible();

  await page.getByRole("button", { name: "さらに読み込む" }).click();
  await expect(page.getByTestId("approval-item").filter({ hasText: s.tableName })).toBeVisible();
  await expect(page.getByTestId("approval-list-pending")).toHaveCount(0);
});
