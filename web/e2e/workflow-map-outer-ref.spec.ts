import { expect, test, type Browser, type Locator, type Page } from "@playwright/test";

import { cleanupQueue, deleteTableApi, loginViaKeycloak, removeWorkflowApi, uniqueName } from "./helpers";

/// 繰り返しの中から、繰り返しの手前のブロックの結果を使う e2e（D-123・BUG-152）と、置いたばかりの「データを追加」を
/// 保存する e2e（BUG-151）:
/// - 繰り返しの中の「データを追加」の値の候補に、繰り返しの手前の「レコードを探す」の結果が出る。選んで保存でき（以前は
///   祖先でないとして保存が止まった）、3 件の入力で実行すると、3 件とも探した承認者で書かれる。
/// - 置いたばかりの「データを追加」（テーブルも値も空）を保存でき、実行は空の欄を名指しして止まる（以前は「項目 'data'
///   がありません」で保存が止まった）。
/// - スマホ幅の一覧から開いた設定でも、同じ候補と注意が出て、横にはみ出さない。
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

/// 保存のボタンを押し、保存できたことを待つ。
async function save(page: Page) {
  const done = page.waitForResponse((r) => r.request().method() === "PUT" && /\/api\/workflows\/[^/]+$/.test(r.url()));
  await page.getByRole("button", { name: "保存", exact: true }).click();
  const res = await done;
  expect(res.ok(), await res.text()).toBeTruthy();
}

async function createTable(page: Page, prefix: string, fields: { key: string; display_name: string }[]): Promise<{ id: string; name: string }> {
  const name = uniqueName(prefix);
  const res = await page.request.post("/api/data/tables", {
    headers: await csrfHeaders(page),
    data: { name, schema: { fields: fields.map((f) => ({ ...f, type: "text" })) } },
  });
  expect(res.status(), await res.text()).toBe(201);
  const id = ((await res.json()) as { id: string }).id;
  cleanup.add(() => deleteTableApi(page, id));
  return { id, name };
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

/// スマホ幅の一覧から、ラベルのブロックの設定を開く（下からのシート）。
async function openOnPhone(phone: Page, id: string, label: string): Promise<Locator> {
  await phone.goto(`/workflows/${id}`);
  const list = phone.getByTestId("workflow-mobile-flow");
  await expect(list).toBeVisible({ timeout: 15_000 });
  // ブロックの行のボタンの名前は「{カテゴリ} {ブロック名}」（「…の後ろにブロックを追加」と取り違えない）。
  await list.getByRole("button", { name: new RegExp(` ${label}$`) }).first().click();
  const sheet = phone.getByRole("dialog").filter({ hasText: "このブロックを削除" });
  await expect(sheet).toBeVisible();
  return sheet;
}

test("繰り返しの中の「データを追加」で、繰り返しの手前の「レコードを探す」の結果を選んで保存し、3 件とも探した承認者で書く（D-123）", async ({
  page,
  browser,
}) => {
  test.setTimeout(180_000);
  await page.setViewportSize({ width: 1440, height: 900 });
  await loginViaKeycloak(page);

  const approvers = await createTable(page, "e2e-承認者の表", [{ key: "name", display_name: "名前" }]);
  const added = await page.request.post(`/api/data/tables/${approvers.id}/records`, {
    headers: await csrfHeaders(page),
    data: { data: { name: "佐藤" } },
  });
  expect(added.status(), await added.text()).toBe(201);
  const tasks = await createTable(page, "e2e-担当の台帳", [
    { key: "title", display_name: "件名" },
    { key: "approver", display_name: "承認者" },
  ]);

  const name = uniqueName("wf-map-outer-ref");
  const created = await page.request.post("/api/workflows", {
    headers: await csrfHeaders(page),
    data: {
      ir: {
        ir_version: 1,
        name,
        display_name: name,
        input_schema: { type: "object", properties: { titles: { type: "array", items: { type: "string" } } } },
        triggers: [{ kind: "interactive" }],
        nodes: [
          { id: "find", type: "data.query", label: "承認者を探す", params: { table: approvers.id } },
          { id: "each", type: "control.map", label: "申請ごとに", params: { items: { $from: "input", path: "/titles" } } },
          {
            id: "write",
            type: "data.record.create",
            label: "担当を記録",
            parent: "each",
            params: { table: tasks.id, data: { title: { $from: "each", path: "/item" } } },
          },
        ],
        edges: [{ from: "find", to: "each" }],
      },
    },
  });
  expect(created.ok(), await created.text()).toBeTruthy();
  const id = ((await created.json()) as { id: string }).id;
  cleanup.add(() => removeWorkflowApi(page, id));

  await page.goto(`/workflows/${id}`);
  const panel = page.getByRole("complementary", { name: "ブロックの設定" });
  await page.getByTestId("rf__node-write").click({ position: { x: 60, y: 24 } });
  await expect(panel.getByRole("combobox", { name: "対象のテーブル" })).toContainText(tasks.name, { timeout: 30_000 });
  const field = panel.locator('[data-value-expr="レコードの値"]');
  await expect(field.getByRole("combobox", { name: "値の入れ方" }).first()).toHaveText("項目を並べる");

  // 承認者の列を足し、入力や結果から選ぶ。候補に、繰り返しの手前の「承認者を探す」の結果が出る。
  await field.getByRole("button", { name: "列を追加" }).click();
  const row = field.getByRole("group").last();
  await row.getByRole("combobox", { name: "列" }).click();
  await page.getByRole("option", { name: "承認者（approver）", exact: true }).click();
  const approver = field.getByRole("group", { name: "承認者（approver）", exact: true });
  await approver.getByRole("combobox", { name: "値の入れ方" }).first().click();
  await page.getByRole("option", { name: "入力や結果から選ぶ", exact: true }).click();
  await approver.getByRole("combobox", { name: "参照先" }).click();
  await expect(page.getByRole("option", { name: "「承認者を探す」の items", exact: true })).toBeVisible();
  await expect(page.getByRole("option", { name: "「承認者を探す」の結果（全体）", exact: true })).toBeVisible();
  // 繰り返しのブロックそのもの（中が終わるまで結果が無い）は候補に出ない。
  await expect(page.getByRole("option", { name: /「申請ごとに」の (items|errors)/ })).toHaveCount(0);
  if (SHOTS) await page.screenshot({ path: `${SHOTS}/map-outer-ref-candidates-pc.png` });
  // 最初のレコードの名前を、場所を指定して選ぶ。
  await page.getByRole("option", { name: "場所を指定する…", exact: true }).click();
  await approver.getByRole("combobox", { name: "参照元" }).click();
  await page.getByRole("option", { name: "「承認者を探す」の結果", exact: true }).click();
  await approver.getByLabel("取り出す場所", { exact: true }).fill("/items/0/data/name");

  await save(page);
  expect((await savedParams(page, id, "write")).data).toEqual({
    title: { $from: "each", path: "/item" },
    approver: { $from: "nodes.find.output", path: "/items/0/data/name" },
  });
  await expect(panel.getByTestId("node-warnings")).toHaveCount(0);
  await approver.scrollIntoViewIfNeeded();
  if (SHOTS) await page.screenshot({ path: `${SHOTS}/map-outer-ref-saved-pc.png` });

  // 3 件の入力で実行すると、3 件とも探した承認者で書かれる。
  const started = await page.request.post(`/api/workflows/${id}/runs`, {
    headers: await csrfHeaders(page),
    data: { input: { titles: ["交通費", "宿泊費", "会議費"] } },
  });
  expect(started.status(), await started.text()).toBe(202);
  const runId = ((await started.json()) as { run_id: string }).run_id;
  await expect
    .poll(
      async () => ((await (await page.request.get(`/api/workflows/${id}/runs/${runId}`)).json()) as { status: string }).status,
      { timeout: 90_000 },
    )
    .toBe("succeeded");
  const records = await page.request.get(`/api/data/tables/${tasks.id}/records`);
  expect(records.ok(), await records.text()).toBeTruthy();
  const rows = ((await records.json()) as { items: { data: { title?: string; approver?: string } }[] }).items;
  expect(rows.map((r) => `${r.data.title}:${r.data.approver}`).sort()).toEqual(
    ["交通費:佐藤", "宿泊費:佐藤", "会議費:佐藤"].sort(),
  );

  // スマホ幅: 一覧から開いた設定でも、選んだ参照が出て、候補に手前のブロックの結果が出る。
  const phone = await phonePage(browser, page);
  const sheet = await openOnPhone(phone, id, "担当を記録");
  const phoneApprover = sheet.locator('[data-value-expr="レコードの値"]').getByRole("group", { name: "承認者（approver）", exact: true });
  await expect(phoneApprover.getByLabel("取り出す場所", { exact: true })).toHaveValue("/items/0/data/name", { timeout: 30_000 });
  await phoneApprover.getByRole("combobox", { name: "参照先" }).click();
  await expect(phone.getByRole("option", { name: "「承認者を探す」の items", exact: true })).toBeVisible();
  if (SHOTS) await phone.screenshot({ path: `${SHOTS}/map-outer-ref-candidates-phone.png` });
  await phone.keyboard.press("Escape");
  expect(await sheet.evaluate((el) => el.scrollWidth <= el.clientWidth + 1)).toBe(true);
  await phoneApprover.scrollIntoViewIfNeeded();
  if (SHOTS) await phone.screenshot({ path: `${SHOTS}/map-outer-ref-phone.png` });
});

test("置いたばかりの「データを追加」は、テーブルも値も空のまま保存でき、実行は空の欄を名指しして止まる（BUG-151）", async ({
  page,
  browser,
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
  await page.getByRole("button", { name: /^データを追加/ }).click();
  await expect(panel.getByRole("combobox", { name: "対象のテーブル" })).toContainText("テーブルを選ぶ");
  const field = panel.locator('[data-value-expr="レコードの値"]');
  await expect(field.getByRole("combobox", { name: "値の入れ方" }).first()).toHaveText("項目を並べる");

  // 保存はできる（注意つき）。保存した定義は空のまま。
  await save(page);
  expect(await savedParams(page, id, "create_1")).toEqual({ table: "", data: "" });
  // 空の欄は、その欄の下で入力を促す（保存の誤りにはしない）。
  await expect(panel.getByText(/項目 'data' がありません|項目 'table' がありません/)).toHaveCount(0);
  await expect(panel.getByText("入力してください")).toHaveCount(2);
  if (SHOTS) await page.screenshot({ path: `${SHOTS}/blank-record-create-pc.png` });

  // 実行は、空の欄を名指しして止まる（run は作らない）。
  await page.getByRole("button", { name: "実行", exact: true }).click();
  await page.getByRole("button", { name: "実行する" }).click();
  await expect(page.getByTestId("run-now-failure")).toContainText("実行できませんでした", { timeout: 15_000 });
  await expect(page.getByText("「対象のテーブル」が空です。選ぶまで、実行も自動実行のオンもできません。").first()).toBeVisible();
  await expect(page.getByText("「レコードの値」が空です。入れるまで、実行も自動実行のオンもできません。").first()).toBeVisible();
  if (SHOTS) await page.screenshot({ path: `${SHOTS}/blank-record-create-run-pc.png` });
  const runs = (await (await page.request.get(`/api/workflows/${id}/runs`)).json()) as { items: unknown[] };
  expect(runs.items).toHaveLength(0);

  // スマホ幅: 一覧から開いた設定でも、空のテーブルと値の欄が出て、横にはみ出さない。
  const phone = await phonePage(browser, page);
  const sheet = await openOnPhone(phone, id, "データを追加");
  await expect(sheet.getByRole("combobox", { name: "対象のテーブル" })).toContainText("テーブルを選ぶ");
  await expect(sheet.locator('[data-value-expr="レコードの値"]').getByRole("combobox", { name: "値の入れ方" }).first()).toHaveText(
    "項目を並べる",
  );
  expect(await sheet.evaluate((el) => el.scrollWidth <= el.clientWidth + 1)).toBe(true);
  if (SHOTS) await phone.screenshot({ path: `${SHOTS}/blank-record-create-phone.png` });
});

test("「レコードの値」に列が無い「データを追加」は、注意だけ出して実行できる（中身の無いまとまりは止めない）", async ({ page }) => {
  test.setTimeout(180_000);
  await page.setViewportSize({ width: 1440, height: 900 });
  await loginViaKeycloak(page);
  const ledger = await createTable(page, "e2e-値の無い追加", [{ key: "title", display_name: "件名" }]);
  const name = uniqueName("wf-empty-values");
  const created = await page.request.post("/api/workflows", {
    headers: await csrfHeaders(page),
    data: {
      ir: {
        ir_version: 1,
        name,
        display_name: name,
        triggers: [{ kind: "interactive" }],
        nodes: [{ id: "add", type: "data.record.create", label: "起票する", params: { table: ledger.id, data: {} } }],
        edges: [],
      },
    },
  });
  expect(created.ok(), await created.text()).toBeTruthy();
  const id = ((await created.json()) as { id: string }).id;
  cleanup.add(() => removeWorkflowApi(page, id));
  // 保存の注意（ir.empty_values）は返るが、実行を止める理由（required_text_empty）にはしない。
  const saved = (await created.json()) as { warnings?: { code: string; path?: string }[] };
  expect((saved.warnings ?? []).map((w) => `${w.code}${w.path ?? ""}`)).toContain("ir.empty_values/params/data");

  await page.goto(`/workflows/${id}`);
  const panel = page.getByRole("complementary", { name: "ブロックの設定" });
  await page.getByTestId("rf__node-add").click({ position: { x: 60, y: 24 } });
  await expect(panel.getByTestId("node-warnings")).toContainText(
    "「レコードの値」に列がありません。このままでは、どの列にも値を入れずにレコードを追加します。",
    { timeout: 30_000 },
  );
  await expect(panel.getByText("入力してください", { exact: true })).toHaveCount(0);
  if (SHOTS) await page.screenshot({ path: `${SHOTS}/empty-values-pc.png` });

  // 実行すると、値の無いレコードが 1 件できる（以前の直しでは「レコードの値」が空として止まった）。
  const started = await page.request.post(`/api/workflows/${id}/runs`, { headers: await csrfHeaders(page), data: { input: {} } });
  expect(started.status(), await started.text()).toBe(202);
  const runId = ((await started.json()) as { run_id: string }).run_id;
  await expect
    .poll(
      async () => ((await (await page.request.get(`/api/workflows/${id}/runs/${runId}`)).json()) as { status: string }).status,
      { timeout: 90_000 },
    )
    .toBe("succeeded");
  const records = await page.request.get(`/api/data/tables/${ledger.id}/records`);
  expect(((await records.json()) as { items: unknown[] }).items).toHaveLength(1);
});
