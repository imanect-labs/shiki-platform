import { expect, test, type Page } from "@playwright/test";

import { cleanupAll, grantTableEditor, loginAs, loginViaKeycloak, uniqueName } from "./helpers";

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

type Setup = { tableId: string; tableName: string; workflowId: string; recordId: string; title: string; fsmId: string };

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
  return { tableId: table.id, tableName, workflowId, recordId: record.id, title, fsmId: f.id };
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

  // サイドバーの「承認」に件数が出る（読み上げのラベルに件数を含める）。100 件以上は「99+」と出る。
  await page.goto("/approvals");
  const badge = /承認（\d+\+? 件）/;
  await expect(page.getByRole("link", { name: badge }).or(page.getByRole("button", { name: badge })).first()).toBeVisible({
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

test("承認待ち: 承認の期限が空欄でも「期限なし」と出さず、実行の期限を期限として出す（D-92）", async ({ page }) => {
  test.setTimeout(180_000);
  await page.setViewportSize({ width: 1440, height: 900 });
  await loginViaKeycloak(page);
  // 実行の期限は 14 日（承認を待つブロックには期限を書いていない）。
  const s = await setup(page, 1_209_600);
  try {
    await waitForApproval(page, s);
    const listed = (await (await page.request.get("/api/approvals")).json()) as {
      items: { record: { id: string }; deadline: string | null }[];
    };
    const mine = listed.items.find((i) => i.record.id === s.recordId);
    expect(mine?.deadline, "実行の期限を期限として返す").toBeTruthy();
    const left = new Date(mine!.deadline!).getTime() - Date.now();
    expect(left).toBeGreaterThan(13 * 86_400_000);
    expect(left).toBeLessThanOrEqual(14 * 86_400_000);

    await page.goto("/approvals");
    const item = page.getByTestId("approval-item").filter({ hasText: s.tableName });
    await expect(item).toBeVisible({ timeout: 15_000 });
    await expect(item).toContainText("あと 13 日");
    await expect(item).not.toContainText("期限なし");
    await item.click();
    const sheet = page.getByTestId("approval-sheet");
    await expect(sheet).toContainText("あと 13 日");
    await expect(sheet).not.toContainText("期限なし");
    if (process.env.E2E_SHOTS_DIR) await page.screenshot({ path: `${process.env.E2E_SHOTS_DIR}/approval-deadline-pc.png` });
  } finally {
    const headers = await csrfHeaders(page);
    await cleanupAll(
      async () => {
        const del = await page.request.delete(`/api/artifacts/${s.workflowId}`, { headers });
        expect(del.status(), await del.text()).toBeLessThan(300);
      },
      async () => {
        const del = await page.request.delete(`/api/data/tables/${s.tableId}`, { headers });
        expect(del.status(), await del.text()).toBeLessThan(300);
      },
      async () => {
        const del = await page.request.delete(`/api/artifacts/${s.fsmId}`, { headers });
        expect(del.status(), await del.text()).toBeLessThan(300);
      },
    );
  }
});

test("承認待ち: 実行履歴の一覧と詳細で、承認を待つ run を実行中と分けて出す（BUG-119）", async ({ page }) => {
  test.setTimeout(180_000);
  await page.setViewportSize({ width: 1440, height: 900 });
  await loginViaKeycloak(page);
  const s = await setup(page);
  let runId = "";
  try {
    await waitForApproval(page, s);
    runId = await runOf(page, s);
    // サーバーは run の状態から、待っているもの（承認）を返す（画面で推し量らない）。
    const listed = (await (await page.request.get(`/api/workflows/${s.workflowId}/runs`)).json()) as {
      items: { run_id: string; status: string; waiting_for: string | null }[];
    };
    expect(listed.items.find((r) => r.run_id === runId)).toMatchObject({ status: "running", waiting_for: "approval" });

    await page.goto(`/workflows/${s.workflowId}/runs`);
    const badge = page.getByTestId("run-status-waiting").first();
    await expect(badge).toHaveText("待機中（承認待ち）", { timeout: 15_000 });
    await expect(badge.locator(".animate-spin")).toHaveCount(0);
    await expect(page.locator("table").getByText("実行中", { exact: true })).toHaveCount(0);
    if (process.env.E2E_SHOTS_DIR) await page.screenshot({ path: `${process.env.E2E_SHOTS_DIR}/runs-waiting-approval-pc.png` });
    // 状態の絞り込みの「実行中」は待機中の run も出す（サーバーは run の状態で絞る）ので、選択肢の名前でそれを言う。
    await page.getByRole("button", { name: /^状態/ }).click();
    await page.getByRole("button", { name: "実行中・待機中", exact: true }).click();
    await expect(page.getByTestId("run-status-waiting").first()).toHaveText("待機中（承認待ち）", { timeout: 15_000 });
    if (process.env.E2E_SHOTS_DIR) await page.screenshot({ path: `${process.env.E2E_SHOTS_DIR}/runs-filter-running-pc.png` });
    await page.keyboard.press("Escape");

    // 詳細の見出しの状態も待機中（承認待ち）。承認を待つブロックは承認待ちと言う。
    await page.locator("table tbody tr").first().click();
    const sheet = page.locator('[role="dialog"]');
    await expect(sheet.locator("h2").getByText("待機中（承認待ち）")).toBeVisible({ timeout: 15_000 });
    await expect(sheet.getByText("承認待ち（承認者の「承認」の画面に出ています）")).toBeVisible();
    // 承認の後ろのブロックは前のブロック待ちで、やり直しの時刻は出さない（やり直さない）。
    await expect(sheet.getByText("前のブロック待ち").first()).toBeVisible();
    await expect(sheet.getByText(/に再試行|にもう一度実行します/)).toHaveCount(0);
    if (process.env.E2E_SHOTS_DIR) await page.screenshot({ path: `${process.env.E2E_SHOTS_DIR}/run-detail-waiting-approval-pc.png` });

    // スマホ幅の一覧も同じ言い方にする。
    await page.keyboard.press("Escape");
    await page.setViewportSize({ width: 390, height: 844 });
    await page.goto(`/workflows/${s.workflowId}/runs`);
    await expect(page.getByRole("list", { name: "実行の一覧" }).getByTestId("run-status-waiting").first()).toHaveText(
      "待機中（承認待ち）",
      { timeout: 15_000 },
    );
    expect(await page.evaluate(() => document.documentElement.scrollWidth <= window.innerWidth + 1)).toBe(true);
    if (process.env.E2E_SHOTS_DIR) await page.screenshot({ path: `${process.env.E2E_SHOTS_DIR}/runs-waiting-approval-phone.png` });
  } finally {
    const headers = await csrfHeaders(page);
    await cleanupAll(
      async () => {
        // 待っている run は止めてから消す（承認の依頼を受信箱に残さない）。
        if (!runId) return;
        const res = await page.request.post(`/api/workflows/${s.workflowId}/runs/${runId}/cancel`, { headers, data: {} });
        expect(res.status(), await res.text()).toBeLessThan(300);
      },
      async () => {
        const del = await page.request.delete(`/api/artifacts/${s.workflowId}`, { headers });
        expect(del.status(), await del.text()).toBeLessThan(300);
      },
      async () => {
        const del = await page.request.delete(`/api/data/tables/${s.tableId}`, { headers });
        expect(del.status(), await del.text()).toBeLessThan(300);
      },
      async () => {
        const del = await page.request.delete(`/api/artifacts/${s.fsmId}`, { headers });
        expect(del.status(), await del.text()).toBeLessThan(300);
      },
    );
  }
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

test("承認を待つブロック: テーブルに無い受信箱の列・担当者の列を出し、外せて、保存の誤りも欄に出す（GAP-08）", async ({ page }) => {
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
    if (node) {
      node.params.show_fields = ["title", "ghost", "amount"];
      node.params.assignees = [{ field: "approver" }, { field: "ghost_user" }, { field: "title" }];
    }
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
  // 担当者の列も同じ（テーブルの owner には定義の全体が見えているので「無い」と言い切れる）。見えるがユーザーの
  // 列でないものは、無いとは言わずに種類で言う。
  await expect(panel.getByText("このテーブルに無い列: ghost_user")).toBeVisible();
  await expect(panel.getByText("ユーザー・部署の列ではありません: title")).toBeVisible();

  // ほかの欄を直して保存すると、サーバの検証（ir.unknown_field）で止まる。テーブルの全体が見えている（owner）ので、
  // 理由は選べない列の行の注記（「このテーブルに無い列」）だけで言い、欄の下にサーバの文言を重ねない。
  await panel.getByRole("textbox", { name: "受信箱の件名" }).fill("経費申請の承認（課長）");
  const saving = page.waitForResponse(
    (r) => new URL(r.url()).pathname === `/api/workflows/${s.workflowId}` && r.request().method() === "PUT",
  );
  await page.getByRole("button", { name: "保存", exact: true }).click();
  const refused = await saving;
  expect(refused.status()).toBe(400);
  expect(await refused.text()).toContain("ir.unknown_field");
  await expect(page.getByText("保存できません（検証エラー）").first()).toBeVisible({ timeout: 15_000 });
  await expect(stray).toContainText("このテーブルに無い列: ghost");
  await expect(panel.getByText("列 ghost はこのテーブルに無いか、読めない列です")).toHaveCount(0);

  // 外すと消え、保存できる（ほかの列は残る）。
  await stray.getByRole("button", { name: "列 ghost を外す" }).click();
  await expect(stray).toHaveCount(0);
  await expect(panel.getByRole("button", { name: "金額", pressed: true })).toBeVisible();
  await panel.getByRole("button", { name: "相手 3 を外す" }).click();
  await panel.getByRole("button", { name: "相手 2 を外す" }).click();
  await page.getByRole("button", { name: "保存", exact: true }).click();
  await expect(page.getByText(/保存しました/).first()).toBeVisible({ timeout: 15_000 });
  const saved = (await (await page.request.get(`/api/workflows/${s.workflowId}`)).json()) as {
    ir: { nodes: { id: string; params: Record<string, unknown> }[] };
  };
  const params = saved.ir.nodes.find((n) => n.id === "ok")?.params ?? {};
  expect(params.show_fields).toEqual(["title", "amount"]);
  expect(params.assignees).toEqual([{ field: "approver" }]);
});

/// alice が作る、`masked` の列を誰にも読めない列（マスク）にした、状態の遷移の規則つきのテーブル。
async function maskedFsmTable(
  page: Page,
  prefix: string,
  fields: Record<string, unknown>[],
  masked: string[],
): Promise<string> {
  const headers = await csrfHeaders(page);
  const states = ["申請中", "承認", "却下"];
  const nobody = { has_role: { role: "a-corp/e2e-nobody", subtree: false } };
  const created = await page.request.post("/api/data/tables", {
    headers,
    data: {
      name: uniqueName(prefix),
      schema: {
        fields: [...fields, { key: "status", display_name: "状態", type: "select", options: states, indexed: true }],
        status_field: "status",
        field_policy: masked.map((field) => ({ field, readable_by: nobody })),
      },
    },
  });
  expect(created.status(), await created.text()).toBe(201);
  const table = (await created.json()) as { id: string; schema: Record<string, unknown>; schema_version: number };
  const fsm = await page.request.post("/api/data/fsms", {
    headers,
    data: {
      name: uniqueName(`${prefix}の流れ`),
      table_id: table.id,
      body: { states, transitions: [{ from: "申請中", to: "承認", actor: "public" }, { from: "申請中", to: "却下", actor: "public" }] },
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
  return table.id;
}

test("承認を待つブロック: テーブルの owner でない編集者には、読めない列を「テーブルに無い列」と言わない（GAP-08）", async ({ browser }) => {
  test.setTimeout(150_000);
  // alice: 金額と別の承認者の列を誰にも読めない列にしたテーブルと、ユーザーの列が読めない列だけのテーブルを作る。
  const aliceCtx = await browser.newContext();
  const alice = await aliceCtx.newPage();
  await loginViaKeycloak(alice);
  const tableId = await maskedFsmTable(
    alice,
    "e2e-給与の申請",
    [
      { key: "title", display_name: "件名", type: "text", indexed: true },
      { key: "amount", display_name: "金額", type: "number" },
      { key: "approver", display_name: "承認者", type: "user_ref", indexed: true },
      { key: "secret_approver", display_name: "別の承認者", type: "user_ref" },
    ],
    ["amount", "secret_approver"],
  );
  const otherId = await maskedFsmTable(
    alice,
    "e2e-人事の申請",
    [
      { key: "title", display_name: "件名", type: "text", indexed: true },
      { key: "owner_user", display_name: "担当者", type: "user_ref" },
    ],
    ["owner_user"],
  );
  await aliceCtx.close();

  // bob: テーブルの editor（owner ではない）。bob に返る定義からは読めない金額が落ちる。
  const bobCtx = await browser.newContext();
  const bob = await bobCtx.newPage();
  await loginAs(bob, "bob");
  const me = (await (await bob.request.get("/api/me")).json()) as { id: string; tenant_id: string };
  await grantTableEditor(bob, me.tenant_id, me.id, tableId);
  await grantTableEditor(bob, me.tenant_id, me.id, otherId);
  const seen = (await (await bob.request.get(`/api/data/tables/${tableId}`)).json()) as {
    schema: { fields: { key: string }[] };
  };
  expect(seen.schema.fields.map((x) => x.key)).toEqual(["title", "approver", "status"]);
  const viewer = (await (await bob.request.get(`/api/data/tables/${tableId}/viewer-context`)).json()) as {
    can_edit: boolean;
    can_manage: boolean;
  };
  expect(viewer).toMatchObject({ can_edit: true, can_manage: false });

  // bob が保存できる形で作る（保存の照合は保存者の見え方で行うので、bob に読めない列は保存で止まる）。
  const name = uniqueName("salary-approval");
  const flow = await bob.request.post("/api/workflows", {
    headers: await csrfHeaders(bob),
    data: {
      ir: {
        ir_version: 1,
        name,
        display_name: name,
        triggers: [{ kind: "interactive" }],
        nodes: [
          {
            id: "ok",
            type: "human.approval",
            label: "上長の承認",
            params: {
              title: "給与の申請の承認",
              record: { table: tableId, id: { $from: "input", path: "/record_id" } },
              assignees: [{ field: "approver" }],
              approve: { to: "承認" },
              show_fields: ["title"],
            },
          },
          {
            id: "ok2",
            type: "human.approval",
            label: "部長の承認",
            params: {
              title: "人事の申請の承認",
              record: { table: otherId, id: { $from: "input", path: "/record_id" } },
              assignees: [{ user: me.id }],
              approve: { to: "承認" },
            },
          },
        ],
        edges: [{ from: "ok", from_port: "approved", to: "ok2" }],
        input_schema: { type: "object", properties: { record_id: { type: "string" } } },
        policies: { run_timeout_sec: 1209600 },
      },
    },
  });
  expect(flow.ok(), await flow.text()).toBeTruthy();
  const workflowId = ((await flow.json()) as { id: string }).id;

  // 読めない列（金額・別の承認者・担当者）とテーブルに本当に無い列（ghost）を入れた版を開く（owner の alice が
  // 書いた・保存の照合が保存者の見え方になる前に保存した）。
  let injected = false;
  await bob.route(`**/api/workflows/${workflowId}`, async (route) => {
    if (injected || route.request().method() !== "GET") return route.continue();
    injected = true;
    const res = await route.fetch();
    const body = (await res.json()) as { ir: { nodes: { id: string; params: Record<string, unknown> }[] } };
    const node = body.ir.nodes.find((n) => n.id === "ok");
    if (node) {
      node.params.show_fields = ["title", "ghost", "amount"];
      node.params.assignees = [{ field: "approver" }, { field: "secret_approver" }, { field: "ghost_user" }];
    }
    const node2 = body.ir.nodes.find((n) => n.id === "ok2");
    if (node2) node2.params.assignees = [{ user: me.id }, { field: "owner_user" }];
    await route.fulfill({ response: res, json: body });
  });
  await bob.goto(`/workflows/${workflowId}`);
  const canvas = bob.locator(".react-flow");
  await expect(canvas.getByText("上長の承認").first()).toBeVisible({ timeout: 15_000 });
  expect(injected).toBe(true);
  await canvas.getByText("上長の承認").first().click();
  const panel = bob.getByRole("complementary", { name: "ブロックの設定" });
  await expect(panel.getByRole("button", { name: "件名", pressed: true })).toBeVisible();
  await expect(panel.getByRole("button", { name: "金額" })).toHaveCount(0);

  // bob には「テーブルに無い列」と「読めない列」の区別がつかない。どちらも断定せず、外すボタンは出す。
  const NEUTRAL = "この画面では選べない列（テーブルに無いか、あなたが読めない列）: ";
  const stray = panel.getByRole("list", { name: "選べない列" });
  await expect(stray).toContainText(`${NEUTRAL}amount`);
  await expect(stray).toContainText(`${NEUTRAL}ghost`);
  await expect(stray).not.toContainText("このテーブルに無い列");
  await expect(stray.getByRole("button", { name: "列 amount を外す" })).toBeVisible();
  // 担当者の列も同じ（読めない別の承認者の列も、無い列も断定しない）。
  await expect(panel.getByText(`${NEUTRAL}secret_approver`)).toBeVisible();
  await expect(panel.getByText(`${NEUTRAL}ghost_user`)).toBeVisible();
  await expect(panel.getByText("このテーブルに無い列")).toHaveCount(0);

  // ほかの欄を直すと検証が走る。サーバは bob の見え方で照合するので、読めない金額・別の承認者も、無い ghost と
  // 同じ誤りで返す。サーバの文言もどちらとも言わない（BUG-65）ので、欄の下にはそのまま出し、注記も断定に変えない。
  const SHOW_GHOST = "列 ghost はこのテーブルに無いか、読めない列です";
  await panel.getByRole("textbox", { name: "受信箱の件名" }).fill("給与の申請の承認（部長）");
  await expect(panel.getByText(SHOW_GHOST)).toBeVisible({ timeout: 15_000 });
  await expect(
    panel.getByText("担当者の列 secret_approver はこのテーブルに無いか、読めない列です"),
  ).toBeVisible();
  await expect(panel.getByText(/このテーブルにありません/)).toHaveCount(0);
  await expect(panel.getByText("このテーブルに無い列")).toHaveCount(0);
  await expect(stray).toContainText(`${NEUTRAL}amount`);
  await expect(stray).toContainText(`${NEUTRAL}ghost`);

  // ghost を外すと、金額がサーバの誤りの添字（/params/show_fields/1）へ詰まる。次の検証の応答が来る前でも、
  // 前の検証の添字を今の並びに当てて、金額の誤りにすり替えない。応答が来たら金額の誤りになる。
  let release: () => void = () => {};
  const held = new Promise<void>((r) => (release = r));
  await bob.route("**/api/workflows/validate", async (route) => {
    await held;
    await route.continue();
  });
  const validating = bob.waitForRequest("**/api/workflows/validate");
  await stray.getByRole("button", { name: "列 ghost を外す" }).click();
  await expect(stray).toContainText(`${NEUTRAL}amount`);
  await validating;
  await expect(panel.getByText(SHOW_GHOST)).toBeVisible();
  await expect(panel.getByText(/列 amount は/)).toHaveCount(0);
  release();
  await expect(
    panel.getByText("列 amount はこのテーブルに無いか、読めない列です"),
  ).toBeVisible({ timeout: 15_000 });
  await expect(stray).not.toContainText("このテーブルに無い列");
  await bob.unroute("**/api/workflows/validate");

  // ユーザーの列が読めない列だけのテーブルでも、「このテーブルにユーザー・部署の列がありません」と言わない。
  await canvas.getByText("部長の承認").first().click();
  await expect(panel.getByText("あなたが読める列に、ユーザー・部署の列がありません")).toBeVisible();
  await expect(panel.getByText(`${NEUTRAL}owner_user`)).toBeVisible();
  await expect(panel.getByText("このテーブルにユーザー・部署の列がありません")).toHaveCount(0);
  await panel.getByRole("button", { name: "相手 2 を外す" }).click();

  // 読めない列・無い列を外せば保存できる。
  await canvas.getByText("上長の承認").first().click();
  await stray.getByRole("button", { name: "列 amount を外す" }).click();
  await expect(stray).toHaveCount(0);
  await panel.getByRole("button", { name: "相手 3 を外す" }).click();
  await panel.getByRole("button", { name: "相手 2 を外す" }).click();
  await bob.getByRole("button", { name: "保存", exact: true }).click();
  await expect(bob.getByText(/保存しました/).first()).toBeVisible({ timeout: 15_000 });
  const saved = (await (await bob.request.get(`/api/workflows/${workflowId}`)).json()) as {
    ir: { nodes: { id: string; params: Record<string, unknown> }[] };
  };
  const params = saved.ir.nodes.find((n) => n.id === "ok")?.params ?? {};
  expect(params.show_fields).toEqual(["title"]);
  expect(params.assignees).toEqual([{ field: "approver" }]);
  expect(saved.ir.nodes.find((n) => n.id === "ok2")?.params.assignees).toEqual([{ user: me.id }]);
  await bobCtx.close();
});

test("承認を待つブロック: 状態の列を読めないテーブルでは、進める状態の欄に「状態の列を読めません」と出す（GAP-08）", async ({
  browser,
}) => {
  test.setTimeout(150_000);
  // alice: 状態の列を誰にも読めない列にした、状態の遷移の規則つきのテーブル。
  const aliceCtx = await browser.newContext();
  const alice = await aliceCtx.newPage();
  await loginViaKeycloak(alice);
  const cols = [
    { key: "title", display_name: "件名", type: "text", indexed: true },
    { key: "approver", display_name: "承認者", type: "user_ref", indexed: true },
  ];
  const maskedId = await maskedFsmTable(alice, "e2e-稟議", cols, ["status"]);
  await aliceCtx.close();

  // bob: そのテーブルの editor（owner ではない）。bob に返る定義には状態の列が無い（status_field も外れる）。
  const bobCtx = await browser.newContext();
  const bob = await bobCtx.newPage();
  await loginAs(bob, "bob");
  const me = (await (await bob.request.get("/api/me")).json()) as { id: string; tenant_id: string };
  await grantTableEditor(bob, me.tenant_id, me.id, maskedId);
  const masked = (await (await bob.request.get(`/api/data/tables/${maskedId}`)).json()) as {
    name: string;
    schema: { status_field?: string | null; fsm_ref?: unknown; fields: { key: string }[] };
  };
  expect(masked.schema.fields.map((f) => f.key)).toEqual(["title", "approver"]);
  expect(masked.schema.status_field ?? null).toBeNull();
  expect(masked.schema.fsm_ref).toBeTruthy();

  // bob が owner のテーブル（状態の列を読める）に束ねた承認を作り、エディタで承認するテーブルを変える。
  const ownId = await maskedFsmTable(bob, "e2e-bob の申請", cols, []);
  const name = uniqueName("ringi-approval");
  const flow = await bob.request.post("/api/workflows", {
    headers: await csrfHeaders(bob),
    data: {
      ir: {
        ir_version: 1,
        name,
        display_name: name,
        triggers: [{ kind: "interactive" }],
        nodes: [
          {
            id: "ok",
            type: "human.approval",
            label: "上長の承認",
            params: {
              title: "稟議の承認",
              record: { table: ownId, id: { $from: "input", path: "/record_id" } },
              assignees: [{ field: "approver" }],
              approve: { to: "承認" },
              reject: { to: "却下" },
              show_fields: ["title"],
            },
          },
        ],
        edges: [],
        input_schema: { type: "object", properties: { record_id: { type: "string" } } },
        policies: { run_timeout_sec: 1209600 },
      },
    },
  });
  expect(flow.ok(), await flow.text()).toBeTruthy();
  const workflowId = ((await flow.json()) as { id: string }).id;
  await bob.goto(`/workflows/${workflowId}`);
  const canvas = bob.locator(".react-flow");
  await expect(canvas.getByText("上長の承認").first()).toBeVisible({ timeout: 15_000 });
  await canvas.getByText("上長の承認").first().click();
  const panel = bob.getByRole("complementary", { name: "ブロックの設定" });
  await expect(panel.getByRole("combobox", { name: "承認したときに進める状態" })).toContainText("承認");

  await panel.getByRole("combobox", { name: "承認するテーブル" }).click();
  await bob.getByRole("option", { name: masked.name }).click();
  // テーブルを選んだ後なので「先にテーブルを選んでください」とは言わず、状態の名前を当て推量で書く欄も出さない。
  const UNREADABLE = "このテーブルの状態の列を読めません";
  await expect(panel.getByText(UNREADABLE, { exact: true })).toHaveCount(2);
  await expect(panel.getByRole("textbox", { name: /に進める状態/ })).toHaveCount(0);
  await expect(panel.getByRole("textbox", { name: "承認したときに進める状態" })).toHaveCount(0);
  await expect(panel.getByRole("combobox", { name: "承認したときに進める状態" })).toHaveCount(0);
  // 両方にある列（件名・承認者）は残る。
  await expect(panel.getByRole("button", { name: "件名", pressed: true })).toBeVisible();
  await expect(panel.getByRole("combobox", { name: "相手 1 の列" })).toContainText("承認者");

  // 検証はテーブルの欄で止め（状態の一覧は出さない）、選べない欄に「状態を選んでください」とは出さない。
  await expect(
    panel.getByText("承認するテーブルが見つからないか、状態の遷移（FSM）で管理されていないか、状態の列を読めません"),
  ).toBeVisible({ timeout: 15_000 });
  await expect(panel.getByText("判断したときに進める状態を選んでください")).toHaveCount(0);
  await expect(panel.getByText(UNREADABLE, { exact: true })).toHaveCount(2);

  // 保存はサーバが止め、保存済みの定義は前のテーブルのまま。
  const saving = bob.waitForResponse(
    (r) => new URL(r.url()).pathname === `/api/workflows/${workflowId}` && r.request().method() === "PUT",
  );
  await bob.getByRole("button", { name: "保存", exact: true }).click();
  expect((await saving).status()).toBe(400);
  const saved = (await (await bob.request.get(`/api/workflows/${workflowId}`)).json()) as {
    ir: { nodes: { id: string; params: { record: { table: string } } }[] };
  };
  expect(saved.ir.nodes.find((n) => n.id === "ok")?.params.record.table).toBe(ownId);
  await bobCtx.close();
});

test("承認の件数のバッジ: 数え切れずに数えた分が 0 件なら「…」、1 件以上なら「N+」（PC・レール・スマホ・GAP-08）", async ({
  browser,
  page,
}) => {
  test.setTimeout(120_000);
  await loginViaKeycloak(page);
  // 見えない承認が多くてサーバが数えるのを打ち切った形（数えられた分は 0 件）。読み直しは画面を開き直して起こす。
  let count = { count: 0, more: true };
  const fake = (p: Page) =>
    p.route(
      (url) => url.pathname === "/api/approvals/count",
      (route) => route.fulfill({ json: count }),
    );
  await fake(page);
  const MAYBE = "承認（承認待ちがあるかもしれません）";
  await page.goto("/drive");
  const nav = page.getByRole("navigation", { name: "メインナビゲーション" });
  // PC の広げた表示: 数の代わりに「…」。「0+」は出さない。
  const wide = nav.getByRole("button", { name: MAYBE, exact: true });
  await expect(wide).toBeVisible({ timeout: 15_000 });
  await expect(wide).toHaveText(/^承認…$/);
  await expect(nav).not.toContainText("0+");
  // レール（折りたたみ）: 点を出し、読み上げ・ツールチップにも同じ語。
  await page.getByRole("button", { name: "サイドバーを折りたたむ" }).click();
  const rail = nav.getByRole("button", { name: MAYBE, exact: true });
  await expect(rail).toBeVisible();
  await expect(rail.locator("span.rounded-full")).toHaveCount(1);
  await rail.hover();
  await expect(page.getByRole("tooltip")).toHaveText(MAYBE);

  // 1 件以上を数えたうえで打ち切ったら、今までどおり「N+」。
  count = { count: 3, more: true };
  await page.reload();
  await expect(nav.getByRole("button", { name: "承認（3+ 件）", exact: true })).toBeVisible({ timeout: 15_000 });
  await page.getByRole("button", { name: "サイドバーを開く" }).click();
  await expect(nav.getByRole("button", { name: "承認（3+ 件）", exact: true })).toHaveText(/^承認3\+$/);

  // スマホのドロワのナビも同じ。
  count = { count: 0, more: true };
  const ctx = await browser.newContext({
    storageState: await page.context().storageState(),
    viewport: { width: 390, height: 844 },
    hasTouch: true,
    isMobile: true,
  });
  const phone = await ctx.newPage();
  await fake(phone);
  await phone.goto("/drive");
  await phone.getByRole("button", { name: "メニューを開く", exact: true }).tap();
  const drawer = phone.getByRole("dialog", { name: "ナビゲーション" }).getByRole("navigation", { name: "メインナビゲーション" });
  const tile = drawer.getByRole("link", { name: MAYBE, exact: true });
  await expect(tile).toBeVisible({ timeout: 15_000 });
  await expect(tile).toHaveText(/^…承認$/);
  await expect(drawer).not.toContainText("0+");
  count = { count: 3, more: true };
  await phone.reload();
  await phone.getByRole("button", { name: "メニューを開く", exact: true }).tap();
  await expect(drawer.getByRole("link", { name: "承認（3+ 件）", exact: true })).toHaveText(/^3\+承認$/, {
    timeout: 15_000,
  });
  await ctx.close();
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
  // 数え切れなかった件数は 0 として隠さない（数えられた分が 0 件なら数は出さない）。
  await expect(
    page
      .getByRole("button", { name: "承認（承認待ちがあるかもしれません）" })
      .or(page.getByRole("link", { name: "承認（承認待ちがあるかもしれません）" }))
      .first(),
  ).toBeVisible();

  await page.getByRole("button", { name: "さらに読み込む" }).click();
  await expect(page.getByTestId("approval-item").filter({ hasText: s.tableName })).toBeVisible();
  await expect(page.getByTestId("approval-list-pending")).toHaveCount(0);
});

test("承認待ち: 判断の前に行が更新されたら、取り直せたときだけ「読み直した」と出し、新しい rev で判断できる（GAP-08）", async ({ page }) => {
  test.setTimeout(180_000);
  await loginViaKeycloak(page);
  const s = await setup(page);
  await waitForApproval(page, s);
  await page.goto("/approvals");
  const item = page.getByTestId("approval-item").filter({ hasText: s.tableName });
  await item.click();
  const sheet = page.getByTestId("approval-sheet");
  await expect(sheet.getByText("12,000")).toBeVisible({ timeout: 15_000 });

  // 開いている間に、ほかの人が行（金額）を更新する。
  const headers = await csrfHeaders(page);
  const current = (await (await page.request.get(`/api/data/tables/${s.tableId}/records/${s.recordId}`)).json()) as {
    rev: number;
  };
  const updated = await page.request.patch(`/api/data/tables/${s.tableId}/records/${s.recordId}`, {
    headers,
    data: { patch: { amount: 15000 }, expected_rev: current.rev },
  });
  expect(updated.status(), await updated.text()).toBe(200);

  // 1 回目: 判断は rev 不一致で断られ、取り直しも通信の失敗で読めない。「読み直した」とは言わない。
  const one = new RegExp(`/api/approvals/[0-9a-f-]{36}$`);
  let failRefetch = true;
  await page.route(one, (route) =>
    failRefetch && route.request().method() === "GET" ? route.abort("failed") : route.continue(),
  );
  await sheet.getByRole("radio", { name: /承認する/ }).click();
  const first = page.waitForResponse((r) => r.url().includes("/decision") && r.request().method() === "POST");
  await sheet.getByTestId("approval-send").click();
  expect((await first).status()).toBe(409);
  const alert = sheet.getByText(/ほかの人が先に行を更新しましたが、最新の内容を読み込めませんでした/);
  await expect(alert).toBeVisible();
  await expect(sheet.getByText(/最新の内容を読み直したので/)).toHaveCount(0);
  await expect(sheet.getByText("12,000")).toBeVisible();

  // 2 回目: 取り直せたら、新しい内容を見せて「読み直した」と出し、もう一度押すと判断が通る。
  failRefetch = false;
  const second = page.waitForResponse((r) => r.url().includes("/decision") && r.request().method() === "POST");
  await sheet.getByTestId("approval-send").click();
  expect((await second).status()).toBe(409);
  await expect(sheet.getByText(/最新の内容を読み直したので、確かめてから判断してください/)).toBeVisible();
  await expect(sheet.getByText("15,000")).toBeVisible();
  const third = page.waitForResponse((r) => r.url().includes("/decision") && r.request().method() === "POST");
  await sheet.getByTestId("approval-send").click();
  expect((await third).status()).toBe(202);
  await expect(sheet).toBeHidden();
});
