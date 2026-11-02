import { expect, test, type Browser, type Locator, type Page } from "@playwright/test";

import { cleanupQueue, deleteTableApi, loginViaKeycloak, removeWorkflowApi, uniqueName } from "./helpers";

/// 同じレコードを探す列と足す数（GAP-16）の e2e:
/// - 「レコードを追加か更新」は、同じ品番で 2 回実行しても 1 件のまま在庫に足す。同じレコードを探す列には重複を許さない
///   列だけが出て、重複を許す列を書いた定義は保存で止まる。置いたばかりのブロックは空のまま保存でき、空の欄を言う。
/// - 「レコードを更新」の「足す数」だけで在庫を減らせ（変更する値は空でよい）、列の最小値を外れる出庫は書かずに失敗する。
/// - 「レコードをまとめて登録」の「同じレコードを探す列」を選ぶと、取り込み直しても件数が増えない。
/// - スマホ幅の一覧から開いた設定でも同じ欄が出て、横にはみ出さない。
/// 前提: shiki-server が SHIKI__WORKFLOW__ENABLED=true（AI のブロックは使わない）。作ったテーブル・ワークフローは最後に消す。

const SHOTS = process.env.E2E_SHOTS_DIR;

const cleanup = cleanupQueue();
test.afterEach(() => cleanup.run());

async function csrfHeaders(page: Page): Promise<Record<string, string>> {
  const csrf = (await page.context().cookies()).find((c) => c.name === "shiki_csrf")?.value ?? "";
  return { "x-csrf-token": csrf, "content-type": "application/json" };
}

type IrNode = { id: string; type: string; params: Record<string, unknown> };

async function savedParams(page: Page, id: string, node: string): Promise<Record<string, unknown>> {
  const res = await page.request.get(`/api/workflows/${id}`);
  expect(res.ok(), await res.text()).toBeTruthy();
  const ir = ((await res.json()) as { ir: { nodes: IrNode[] } }).ir;
  return ir.nodes.find((n) => n.id === node)?.params ?? {};
}

async function save(page: Page) {
  const done = page.waitForResponse((r) => r.request().method() === "PUT" && /\/api\/workflows\/[^/]+$/.test(r.url()));
  await page.getByRole("button", { name: "保存", exact: true }).click();
  const res = await done;
  expect(res.ok(), await res.text()).toBeTruthy();
}

/// 品目の表: 品番（重複を許さない）・名前・メモ（重複を許す）・在庫（0 以上の整数）。
async function itemsTable(page: Page): Promise<{ id: string; name: string }> {
  const name = uniqueName("e2e-品目");
  const res = await page.request.post("/api/data/tables", {
    headers: await csrfHeaders(page),
    data: {
      name,
      schema: {
        fields: [
          { key: "sku", display_name: "品番", type: "text", unique: true },
          { key: "name", display_name: "名前", type: "text" },
          { key: "memo", display_name: "メモ", type: "text", indexed: true },
          { key: "stock", display_name: "在庫", type: "number", numeric_spec: { scale: 0, min: 0 } },
        ],
      },
    },
  });
  expect(res.status(), await res.text()).toBe(201);
  const id = ((await res.json()) as { id: string }).id;
  cleanup.add(() => deleteTableApi(page, id));
  return { id, name };
}

async function createWorkflow(page: Page, nodes: unknown[], inputSchema?: unknown): Promise<string> {
  const name = uniqueName("wf-upsert");
  const res = await page.request.post("/api/workflows", {
    headers: await csrfHeaders(page),
    data: {
      ir: {
        ir_version: 1,
        name,
        display_name: name,
        ...(inputSchema ? { input_schema: inputSchema } : {}),
        triggers: [{ kind: "interactive" }],
        nodes,
        edges: [],
      },
    },
  });
  expect(res.ok(), await res.text()).toBeTruthy();
  const id = ((await res.json()) as { id: string }).id;
  cleanup.add(() => removeWorkflowApi(page, id));
  return id;
}

/// 実行して終わるまで待ち、終わった状態を返す。
async function runToEnd(page: Page, id: string, input: unknown): Promise<string> {
  const started = await page.request.post(`/api/workflows/${id}/runs`, { headers: await csrfHeaders(page), data: { input } });
  expect(started.status(), await started.text()).toBe(202);
  const runId = ((await started.json()) as { run_id: string }).run_id;
  let status = "";
  await expect
    .poll(
      async () => {
        status = ((await (await page.request.get(`/api/workflows/${id}/runs/${runId}`)).json()) as { status: string }).status;
        return ["succeeded", "failed"].includes(status);
      },
      { timeout: 90_000 },
    )
    .toBe(true);
  return status;
}

async function records(page: Page, table: string): Promise<{ id: string; data: Record<string, unknown> }[]> {
  const res = await page.request.get(`/api/data/tables/${table}/records`);
  expect(res.ok(), await res.text()).toBeTruthy();
  return ((await res.json()) as { items: { id: string; data: Record<string, unknown> }[] }).items;
}

async function phonePage(browser: Browser, page: Page): Promise<Page> {
  const ctx = await browser.newContext({
    storageState: await page.context().storageState(),
    viewport: { width: 390, height: 844 },
    hasTouch: true,
    isMobile: true,
    deviceScaleFactor: 2,
    ignoreHTTPSErrors: true,
  });
  cleanup.add(() => ctx.close());
  return ctx.newPage();
}

async function openOnPhone(phone: Page, id: string, label: string): Promise<Locator> {
  await phone.goto(`/workflows/${id}`);
  const list = phone.getByTestId("workflow-mobile-flow");
  await expect(list).toBeVisible({ timeout: 15_000 });
  await list.getByRole("button", { name: new RegExp(` ${label}$`) }).first().click();
  const sheet = phone.getByRole("dialog").filter({ hasText: "このブロックを削除" });
  await expect(sheet).toBeVisible();
  return sheet;
}

test("「レコードを追加か更新」は同じ品番で 2 回実行しても 1 件のまま在庫に足し、探す列には重複を許さない列だけが出る（GAP-16）", async ({
  page,
  browser,
}) => {
  test.setTimeout(180_000);
  await page.setViewportSize({ width: 1440, height: 900 });
  await loginViaKeycloak(page);
  const items = await itemsTable(page);
  const id = await createWorkflow(
    page,
    [
      {
        id: "restock",
        type: "data.record.upsert",
        label: "入庫を記録",
        params: {
          table: items.id,
          match_on: "sku",
          data: { sku: { $from: "input", path: "/sku" }, name: "六角ボルト" },
          add: { stock: { $from: "input", path: "/qty" } },
        },
      },
    ],
    { type: "object", properties: { sku: { type: "string" }, qty: { type: "number" } } },
  );

  await page.goto(`/workflows/${id}`);
  const panel = page.getByRole("complementary", { name: "ブロックの設定" });
  await page.getByTestId("rf__node-restock").click({ position: { x: 60, y: 24 } });
  await expect(panel.getByRole("combobox", { name: "対象のテーブル" })).toContainText(items.name, { timeout: 30_000 });
  const matchOn = panel.getByRole("combobox", { name: "同じレコードを探す列", exact: true });
  await expect(matchOn).toHaveText("品番（sku）");
  // 選べるのは重複を許さない列だけ（名前・メモ・在庫は出ない）。
  await matchOn.click();
  await expect(page.getByRole("option")).toHaveText(["品番（sku）"]);
  await page.keyboard.press("Escape");
  const add = panel.locator('[data-value-expr="足す数（省略可）"]');
  await expect(add.getByRole("group", { name: "在庫（stock）", exact: true })).toBeVisible();
  await expect(panel.getByText("入力してください", { exact: true })).toHaveCount(0);
  await add.scrollIntoViewIfNeeded();
  if (SHOTS) await page.screenshot({ path: `${SHOTS}/upsert-panel-pc.png` });

  // 同じ品番で 2 回実行しても 1 件のまま、在庫に足す。別の品番は追加する。
  expect(await runToEnd(page, id, { sku: "A-100", qty: 5 })).toBe("succeeded");
  expect(await runToEnd(page, id, { sku: "A-100", qty: 3 })).toBe("succeeded");
  expect(await runToEnd(page, id, { sku: "B-200", qty: 1 })).toBe("succeeded");
  const rows = await records(page, items.id);
  expect(rows.map((r) => `${r.data.sku}:${r.data.stock}`).sort()).toEqual(["A-100:8", "B-200:1"]);

  // 重複を許す列（メモ）で探す定義は、保存で止まる（同時に動くと 2 件追加されうるため）。
  const refused = await page.request.put(`/api/workflows/${id}`, {
    headers: await csrfHeaders(page),
    data: {
      ir: {
        ...((await (await page.request.get(`/api/workflows/${id}`)).json()) as { ir: Record<string, unknown> }).ir,
        nodes: [
          {
            id: "restock",
            type: "data.record.upsert",
            params: { table: items.id, match_on: "memo", data: { memo: "x" } },
          },
        ],
      },
    },
  });
  expect(refused.status()).toBe(400);
  expect(await refused.text()).toContain("ir.match_not_unique");

  // スマホ幅: 一覧から開いた設定でも、探す列と足す数が出て、横にはみ出さない。
  const phone = await phonePage(browser, page);
  const sheet = await openOnPhone(phone, id, "入庫を記録");
  await expect(sheet.getByRole("combobox", { name: "同じレコードを探す列", exact: true })).toHaveText("品番（sku）", {
    timeout: 30_000,
  });
  await expect(sheet.locator('[data-value-expr="足す数（省略可）"]').getByRole("group", { name: "在庫（stock）" })).toBeVisible();
  expect(await sheet.evaluate((el) => el.scrollWidth <= el.clientWidth + 1)).toBe(true);
  if (SHOTS) await phone.screenshot({ path: `${SHOTS}/upsert-panel-phone.png` });
});

test("置いたばかりの「レコードを追加か更新」は空のまま保存でき、テーブルと探す列は「選んでください」、入れる値は「入力してください」と言う", async ({
  page,
}) => {
  test.setTimeout(150_000);
  await page.setViewportSize({ width: 1440, height: 900 });
  await loginViaKeycloak(page);
  page.on("dialog", (d) => void d.accept());
  await page.goto("/workflows");
  await expect(page.getByText("読み込み中…")).toHaveCount(0);
  await page.getByRole("button", { name: "新しいワークフロー", exact: true }).click();
  await page.waitForURL(/\/workflows\/[0-9a-f-]+$/i, { timeout: 20_000 });
  const id = new URL(page.url()).pathname.split("/").pop()!;
  cleanup.add(() => removeWorkflowApi(page, id));
  const panel = page.getByRole("complementary", { name: "ブロックの設定" });

  await page.getByRole("button", { name: "最初のブロック" }).click();
  await page.getByRole("button", { name: /^レコードを追加か更新/ }).click();
  await expect(panel.getByRole("combobox", { name: "対象のテーブル" })).toContainText("テーブルを選ぶ");
  await save(page);
  const nodeId = (((await (await page.request.get(`/api/workflows/${id}`)).json()) as { ir: { nodes: IrNode[] } }).ir.nodes[0]).id;
  expect(await savedParams(page, id, nodeId)).toEqual({ table: "", match_on: "", data: "" });
  await expect(panel.getByText("選んでください", { exact: true })).toHaveCount(2);
  await expect(panel.getByText("入力してください", { exact: true })).toHaveCount(1);
  if (SHOTS) await page.screenshot({ path: `${SHOTS}/upsert-blank-pc.png` });

  // 実行は、空の欄を名指しして止まる（run は作らない）。
  await page.getByRole("button", { name: "実行", exact: true }).click();
  await page.getByRole("button", { name: "実行する" }).click();
  await expect(page.getByTestId("run-now-failure")).toContainText("実行できませんでした", { timeout: 15_000 });
  await expect(
    page.getByText("「同じレコードを探す列」が空です。選ぶまで、実行も自動実行のオンもできません。").first(),
  ).toBeVisible();
  const runs = (await (await page.request.get(`/api/workflows/${id}/runs`)).json()) as { items: unknown[] };
  expect(runs.items).toHaveLength(0);
});

test("「レコードを更新」の「足す数」だけで在庫を減らし、0 未満になる出庫は書かずに失敗する（GAP-16）", async ({ page, browser }) => {
  test.setTimeout(180_000);
  await page.setViewportSize({ width: 1440, height: 900 });
  await loginViaKeycloak(page);
  const items = await itemsTable(page);
  const added = await page.request.post(`/api/data/tables/${items.id}/records`, {
    headers: await csrfHeaders(page),
    data: { data: { sku: "C-300", stock: 3 } },
  });
  expect(added.status(), await added.text()).toBe(201);
  const recordId = ((await added.json()) as { id: string }).id;
  const id = await createWorkflow(
    page,
    [
      {
        id: "ship",
        type: "data.record.update",
        label: "出庫を記録",
        params: { table: items.id, record: recordId, add: { stock: { $from: "input", path: "/delta" } } },
      },
    ],
    { type: "object", properties: { delta: { type: "number" } } },
  );

  await page.goto(`/workflows/${id}`);
  const panel = page.getByRole("complementary", { name: "ブロックの設定" });
  await page.getByTestId("rf__node-ship").click({ position: { x: 60, y: 24 } });
  await expect(panel.getByRole("combobox", { name: "対象のテーブル" })).toContainText(items.name, { timeout: 30_000 });
  const add = panel.locator('[data-value-expr="足す数（省略可）"]');
  await expect(add.getByRole("group", { name: "在庫（stock）", exact: true })).toBeVisible();
  // 足す数だけで書き換えるので、変更する値は空でよい（「入力してください」を出さない）。
  await expect(panel.getByText("入力してください", { exact: true })).toHaveCount(0);
  await add.scrollIntoViewIfNeeded();
  if (SHOTS) await page.screenshot({ path: `${SHOTS}/update-add-pc.png` });

  expect(await runToEnd(page, id, { delta: -2 })).toBe("succeeded");
  // 残り 1 から 2 は出せない（列の最小値 0 を外れる）。書かずに失敗し、在庫は 1 のまま。
  expect(await runToEnd(page, id, { delta: -2 })).toBe("failed");
  const rows = await records(page, items.id);
  expect(rows.find((r) => r.id === recordId)?.data.stock).toBe(1);

  const phone = await phonePage(browser, page);
  const sheet = await openOnPhone(phone, id, "出庫を記録");
  await expect(sheet.locator('[data-value-expr="足す数（省略可）"]').getByRole("group", { name: "在庫（stock）" })).toBeVisible({
    timeout: 30_000,
  });
  expect(await sheet.evaluate((el) => el.scrollWidth <= el.clientWidth + 1)).toBe(true);
  await sheet.locator('[data-value-expr="足す数（省略可）"]').scrollIntoViewIfNeeded();
  if (SHOTS) await phone.screenshot({ path: `${SHOTS}/update-add-phone.png` });
});

test("「レコードをまとめて登録」で同じレコードを探す列を選ぶと、取り込み直しても件数が増えない（GAP-16）", async ({ page }) => {
  test.setTimeout(180_000);
  await page.setViewportSize({ width: 1440, height: 900 });
  await loginViaKeycloak(page);
  const items = await itemsTable(page);
  const id = await createWorkflow(
    page,
    [
      {
        id: "load",
        type: "data.bulk_upsert",
        label: "品目を取り込む",
        params: { table: items.id, rows: { $from: "input", path: "/rows" } },
      },
    ],
    { type: "object", properties: { rows: { type: "array", items: { type: "object" } } } },
  );

  await page.goto(`/workflows/${id}`);
  const panel = page.getByRole("complementary", { name: "ブロックの設定" });
  await page.getByTestId("rf__node-load").click({ position: { x: 60, y: 24 } });
  await expect(panel.getByRole("combobox", { name: "対象のテーブル" })).toContainText(items.name, { timeout: 30_000 });
  const matchOn = panel.getByRole("combobox", { name: "同じレコードを探す列（省略可）", exact: true });
  await expect(matchOn).toHaveText("選ばない（ID の無いレコードはすべて追加）");
  await matchOn.click();
  await page.getByRole("option", { name: "品番（sku）", exact: true }).click();
  await save(page);
  expect((await savedParams(page, id, "load")).match_on).toBe("sku");
  if (SHOTS) await page.screenshot({ path: `${SHOTS}/bulk-match-on-pc.png` });

  const rows = [
    { data: { sku: "D-1", stock: 1 } },
    { data: { sku: "D-2", stock: 2 } },
  ];
  expect(await runToEnd(page, id, { rows })).toBe("succeeded");
  expect(await runToEnd(page, id, { rows: [{ data: { sku: "D-1", stock: 10 } }, ...rows.slice(1)] })).toBe("succeeded");
  const got = await records(page, items.id);
  expect(got.map((r) => `${r.data.sku}:${r.data.stock}`).sort()).toEqual(["D-1:10", "D-2:2"]);

  // 選ばないに戻すと、項目を外して保存する（従来どおり、ID の無いレコードはすべて追加）。
  await matchOn.click();
  await page.getByRole("option", { name: "選ばない（ID の無いレコードはすべて追加）", exact: true }).click();
  await save(page);
  expect(await savedParams(page, id, "load")).not.toHaveProperty("match_on");
});
