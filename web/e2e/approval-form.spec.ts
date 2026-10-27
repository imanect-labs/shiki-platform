import { expect, test, type Browser, type Page } from "@playwright/test";

import { loginViaKeycloak, uniqueName } from "./helpers";

/// 承認を待つブロック（human.approval）の設定の欄の E2E（BUG-82）。
/// - 受信箱に出す列に状態の列があると、選べない列の行の注記（外すボタン付き）と、保存時の照合の誤り
///   （`ir.bad_show_field`）が同じことを 2 回言っていた。行の注記にまとめ、欄の下には重ねない（BUG-67 の E2E を
///   approvals.spec.ts から移して書き直した）。
/// テーブル・状態の遷移の規則・ワークフローは有効化せずに作り（run を起こさない）、最後に消す。
/// `E2E_SHOTS_DIR` を渡すとスクリーンショットを撮る。

const SHOTS = process.env.E2E_SHOTS_DIR;

async function csrfHeaders(page: Page): Promise<Record<string, string>> {
  const csrf = (await page.context().cookies()).find((c) => c.name === "shiki_csrf")?.value ?? "";
  return { "x-csrf-token": csrf, "content-type": "application/json" };
}

type Fixture = { tableId: string; fsmId: string; workflowId: string };

/// 申請の表（状態の遷移の規則つき）と、その行の承認を待つワークフロー（有効化しない）を作る。
async function createFixture(page: Page): Promise<Fixture> {
  const headers = await csrfHeaders(page);
  const states = ["下書き", "申請中", "受付", "承認", "却下", "差し戻し"];
  const created = await page.request.post("/api/data/tables", {
    headers,
    data: {
      name: uniqueName("e2e-備品の申請"),
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
  const fsm = await page.request.post("/api/data/fsms", {
    headers,
    data: {
      name: uniqueName("e2e-備品の承認の流れ"),
      table_id: table.id,
      body: {
        states,
        transitions: [
          { from: "下書き", to: "申請中", actor: "public" },
          { from: "申請中", to: "受付", actor: "public" },
          { from: "受付", to: "承認", actor: "public" },
          { from: "受付", to: "却下", actor: "public" },
          { from: "受付", to: "差し戻し", actor: "public" },
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
  const name = uniqueName("lend-approval");
  const flow = await page.request.post("/api/workflows", {
    headers,
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
              title: "備品の申請の承認",
              record: { table: table.id, id: { $from: "input", path: "/record_id" } },
              assignees: [{ field: "approver" }],
              approve: { to: "承認" },
              reject: { to: "却下", comment: "required" },
              show_fields: ["title", "amount"],
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
  return { tableId: table.id, fsmId: f.id, workflowId: ((await flow.json()) as { id: string }).id };
}

async function deleteFixture(page: Page, f: Fixture): Promise<void> {
  const headers = await csrfHeaders(page);
  for (const id of [f.workflowId, f.fsmId]) {
    const res = await page.request.delete(`/api/artifacts/${id}`, { headers });
    expect([204, 404], `DELETE /artifacts/${id} → ${res.status()}`).toContain(res.status());
  }
  const res = await page.request.delete(`/api/data/tables/${f.tableId}`, { headers });
  expect([200, 204, 404], `DELETE /data/tables/${f.tableId} → ${res.status()}`).toContain(res.status());
}

/// エディタが読み込む IR の承認ノードの params だけを書き換える（AI が書いた・照合が入る前に保存した版を開く）。
/// ページごとに最初の 1 回だけ書き換える。
async function injectParams(page: Page, workflowId: string, patch: Record<string, unknown>): Promise<() => boolean> {
  let injected = false;
  await page.route(`**/api/workflows/${workflowId}`, async (route) => {
    if (injected || route.request().method() !== "GET") return route.continue();
    injected = true;
    const res = await route.fetch();
    const body = (await res.json()) as { ir: { nodes: { id: string; params: Record<string, unknown> }[] } };
    const node = body.ir.nodes.find((n) => n.id === "ok");
    if (node) Object.assign(node.params, patch);
    await route.fulfill({ response: res, json: body });
  });
  return () => injected;
}

test("受信箱に出す列の状態の列は、選べない列の行の注記だけで言い、保存時の照合の誤りを重ねない（BUG-82）", async ({
  page,
  browser,
}) => {
  test.setTimeout(180_000);
  await loginViaKeycloak(page);
  const f = await createFixture(page);
  let phone: Page | null = null;
  try {
    // 状態の列（status）とテーブルに無い列（ghost）を show_fields に持つ版を開く。
    const injected = await injectParams(page, f.workflowId, { show_fields: ["title", "status", "ghost", "amount"] });
    await page.setViewportSize({ width: 1440, height: 900 });
    await page.goto(`/workflows/${f.workflowId}`);
    const canvas = page.locator(".react-flow");
    await expect(canvas.getByText("上長の承認").first()).toBeVisible({ timeout: 15_000 });
    expect(injected()).toBe(true);
    await canvas.getByText("上長の承認").first().click();
    const panel = page.getByRole("complementary", { name: "ブロックの設定" });
    await expect(panel.getByRole("button", { name: "状態", exact: true })).toHaveCount(0);
    const stray = panel.getByRole("list", { name: "選べない列" });
    await expect(stray).toContainText("状態の列は選べません（状態は受信箱に別に出ます）: status");
    await expect(stray).toContainText("このテーブルに無い列: ghost");

    // ほかの欄を直すと検証が走り、サーバも状態の列（ir.bad_show_field）と無い列を止める。
    const validated = page.waitForResponse(
      (r) => r.url().endsWith("/api/workflows/validate") && r.request().method() === "POST",
    );
    await panel.getByRole("textbox", { name: "受信箱の件名" }).fill("備品の申請の承認（課長）");
    const body = JSON.stringify(await (await validated).json());
    expect(body).toContain("ir.bad_show_field");
    expect(body).toContain("ir.unknown_field");
    // 無い列の誤りは欄の下に出る。状態の列の誤りは行の注記と同じことなので、欄の下には出さない（1 回だけ言う）。
    await expect(panel.getByText("列 ghost はこのテーブルに無いか、読めない列です")).toBeVisible({ timeout: 15_000 });
    await expect(panel.getByText(/列 status は状態の列なので/)).toHaveCount(0);
    await expect(panel.getByText(/状態の列/)).toHaveCount(1);
    await stray.scrollIntoViewIfNeeded();
    if (SHOTS) await panel.screenshot({ path: `${SHOTS}/bug82-show-fields-pc.png` });

    // スマホのシートでも同じ（同じ版を開き、検証の後も状態の列は 1 回だけ言う）。
    phone = await phoneOf(browser, page);
    const phoneInjected = await injectParams(phone, f.workflowId, { show_fields: ["title", "status", "ghost", "amount"] });
    await phone.goto(`/workflows/${f.workflowId}`);
    const flow = phone.getByTestId("workflow-mobile-flow");
    await expect(flow).toBeVisible({ timeout: 15_000 });
    expect(phoneInjected()).toBe(true);
    await flow.getByRole("button", { name: /上長の承認/ }).filter({ hasText: "上長の承認" }).click();
    const sheet = phone.getByRole("dialog");
    const phoneStray = sheet.getByRole("list", { name: "選べない列" });
    await expect(phoneStray).toContainText("状態の列は選べません（状態は受信箱に別に出ます）: status", { timeout: 15_000 });
    const phoneValidated = phone.waitForResponse(
      (r) => r.url().endsWith("/api/workflows/validate") && r.request().method() === "POST",
    );
    await sheet.getByRole("textbox", { name: "受信箱の件名" }).fill("備品の申請の承認（係長）");
    expect(JSON.stringify(await (await phoneValidated).json())).toContain("ir.bad_show_field");
    await expect(sheet.getByText("列 ghost はこのテーブルに無いか、読めない列です")).toBeVisible({ timeout: 15_000 });
    await expect(sheet.getByText(/状態の列/)).toHaveCount(1);
    await phoneStray.scrollIntoViewIfNeeded();
    expect(await sheet.evaluate((el) => el.scrollWidth <= el.clientWidth + 1)).toBe(true);
    if (SHOTS) await phone.screenshot({ path: `${SHOTS}/bug82-show-fields-phone.png` });

    // 無い列を外すと、欄の下の誤りは消える（状態の列の注記は残る）。
    const revalidated = page.waitForResponse(
      (r) => r.url().endsWith("/api/workflows/validate") && r.request().method() === "POST",
    );
    await stray.getByRole("button", { name: "列 ghost を外す" }).click();
    expect(JSON.stringify(await (await revalidated).json())).toContain("ir.bad_show_field");
    await expect(panel.getByText("列 ghost はこのテーブルに無いか、読めない列です")).toHaveCount(0);
    await expect(panel.getByText(/列 status は状態の列なので/)).toHaveCount(0);
    await expect(panel.getByText(/状態の列/)).toHaveCount(1);

    // 状態の列も外すと保存できる（ほかの列は残る）。
    await stray.getByRole("button", { name: "列 status を外す" }).click();
    await expect(stray).toHaveCount(0);
    await page.getByRole("button", { name: "保存", exact: true }).click();
    await expect(page.getByText(/保存しました/).first()).toBeVisible({ timeout: 15_000 });
    const saved = (await (await page.request.get(`/api/workflows/${f.workflowId}`)).json()) as {
      ir: { nodes: { id: string; params: Record<string, unknown> }[] };
    };
    expect(saved.ir.nodes.find((n) => n.id === "ok")?.params.show_fields).toEqual(["title", "amount"]);
  } finally {
    await page.unrouteAll({ behavior: "ignoreErrors" });
    await phone?.context().close().catch(() => undefined);
    await deleteFixture(page, f);
  }
});

/// ログインは PC の画面で済ませ、そのセッションをタッチ端末の文脈へ持ち込む。
async function phoneOf(browser: Browser, page: Page): Promise<Page> {
  const ctx = await browser.newContext({
    storageState: await page.context().storageState(),
    viewport: { width: 390, height: 844 },
    hasTouch: true,
    isMobile: true,
    deviceScaleFactor: 2,
    ignoreHTTPSErrors: true,
  });
  return ctx.newPage();
}
