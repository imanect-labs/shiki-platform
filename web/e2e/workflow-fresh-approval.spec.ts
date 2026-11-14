import { expect, test, type Browser, type Locator, type Page } from "@playwright/test";

import { cleanupQueue, loginViaKeycloak, removeWorkflowApi, trackCreatedResources, uniqueName } from "./helpers";

/// 置いたばかりの「承認を待つ」と、キーの順の e2e（BUG-167・D-129・BUG-156・D-128）:
/// - 置いたばかりの承認を待つ（件名・テーブル・承認者・行き先が空）は保存でき、実行は空の欄を名指しして止まる。
///   欄の下では、件名は「入力してください」、テーブルは「選んでください」、承認者は「承認者を 1 人以上追加してください。」と
///   促し、テーブルを選ぶまでは状態の欄に促しを出さない。
/// - 別のワークフローを実行で渡す入力は、相手の入力項目の順（x-order）で並ぶ。結果を画面に返すの表の行は、表の列の
///   順で並ぶ。保存した値のキーは名前の順に並び直る（jsonb）ので、並べるのはエディターだけで、保存する値は変えない。
/// 前提: shiki-server が SHIKI__WORKFLOW__ENABLED=true・LLM は stub（AI は呼ばない）。`E2E_SHOTS_DIR` でスクリーンショット。

const SHOTS = process.env.E2E_SHOTS_DIR;

const cleanup = cleanupQueue();
test.afterEach(() => cleanup.run());

async function csrfHeaders(page: Page): Promise<Record<string, string>> {
  const csrf = (await page.context().cookies()).find((c) => c.name === "shiki_csrf")?.value ?? "";
  return { "x-csrf-token": csrf, "content-type": "application/json" };
}

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

/// 「項目を並べる」の行のキーの並び（行の名前は、キーか列の名前）。
async function rowKeys(scope: Locator): Promise<string[]> {
  return scope.getByTestId("compose-row").evaluateAll((rows) => rows.map((r) => r.getAttribute("aria-label") ?? ""));
}

const BLANK_MESSAGES = [
  "「受信箱の件名」が空です。入れるまで、実行も自動実行のオンもできません。",
  "「承認するテーブル」が空です。選ぶまで、実行も自動実行のオンもできません。",
  "「承認者」がいません。追加するまで、実行も自動実行のオンもできません。",
  "「承認したときに進める状態」が空です。選ぶまで、実行も自動実行のオンもできません。",
];

test("置いたばかりの承認を待つ: 空のまま保存でき、実行は空の欄を名指しして止まる（BUG-167・D-129）", async ({ browser, page }) => {
  test.setTimeout(180_000);
  await page.setViewportSize({ width: 1440, height: 900 });
  await loginViaKeycloak(page);
  // 一覧の「新しいワークフロー」で作ったものは、応答から片付けに積む。
  trackCreatedResources(page, cleanup);
  page.on("dialog", (d) => void d.accept());
  let phone: Page | null = null;
  cleanup.add(async () => {
    await phone?.context().close();
  });

  await page.goto("/workflows");
  await expect(page.getByText("読み込み中…")).toHaveCount(0);
  await page.getByRole("button", { name: "新しいワークフロー", exact: true }).click();
  await page.waitForURL(/\/workflows\/[0-9a-f-]+$/i, { timeout: 20_000 });
  const id = new URL(page.url()).pathname.split("/").pop()!;
  const panel = page.getByRole("complementary", { name: "ブロックの設定" });

  // 置く。保存の前の検証は、空の欄を誤りにせず、欄の下で促すだけ。
  const validated = page.waitForResponse(
    (r) => r.url().endsWith("/api/workflows/validate") && r.request().method() === "POST",
  );
  await page.getByRole("button", { name: "最初のブロック" }).click();
  await page.getByRole("button", { name: /^承認を待つ/ }).click();
  const check = (await (await validated).json()) as { errors: unknown[]; warnings: { code: string; path?: string }[] };
  expect(check.errors).toEqual([]);
  await expect(panel.getByRole("textbox", { name: "受信箱の件名" })).toHaveAccessibleDescription("入力してください");
  await expect(panel.getByText("選んでください", { exact: true })).toHaveCount(1, { timeout: 15_000 });
  await expect(panel.getByTestId("approval-assignees-error")).toHaveText("承認者を 1 人以上追加してください。");
  // テーブルを選ぶまでは状態を選べないので、状態の欄には促しを出さない（欄が「先にテーブルを選んでください」と言う）。
  await expect(panel.getByRole("textbox", { name: "承認したときに進める状態" })).toHaveAttribute(
    "placeholder",
    "先にテーブルを選んでください",
  );
  await expect(panel.getByText("件名は 1〜120 字で書いてください")).toHaveCount(0);
  if (SHOTS) await page.screenshot({ path: `${SHOTS}/fresh-approval-pc.png` });

  // 保存できる。保存した定義は空のまま。
  await page.getByRole("button", { name: "保存", exact: true }).click();
  await expect(page.getByText(/保存しました/).first()).toBeVisible({ timeout: 15_000 });
  const saved = (await (await page.request.get(`/api/workflows/${id}`)).json()) as {
    ir: { nodes: { type: string; params: Record<string, unknown> }[] };
  };
  const approval = saved.ir.nodes.find((n) => n.type === "human.approval");
  expect(approval?.params).toMatchObject({ title: "", record: { table: "" }, assignees: [], approve: { to: "" } });

  // 実行は、空の欄を名指しして止まる（run は作らない）。
  await page.getByRole("button", { name: "実行", exact: true }).click();
  await page.getByRole("button", { name: "実行する" }).click();
  const failure = page.getByTestId("run-now-failure");
  await expect(failure).toContainText("実行できませんでした", { timeout: 15_000 });
  for (const m of BLANK_MESSAGES) await expect(failure.getByText(m)).toBeVisible();
  if (SHOTS) await page.screenshot({ path: `${SHOTS}/fresh-approval-run-pc.png` });
  const runs = (await (await page.request.get(`/api/workflows/${id}/runs`)).json()) as { items: unknown[] };
  expect(runs.items).toHaveLength(0);
  await page.getByRole("dialog").getByRole("button", { name: "閉じる", exact: true }).click();

  // スマホ幅: 一覧から開いたシートでも同じ促しが出て、横にはみ出さない。実行も同じ理由で止まる。
  phone = await phoneOf(browser, page);
  await phone.goto(`/workflows/${id}`);
  const list = phone.getByTestId("workflow-mobile-flow");
  await expect(list).toBeVisible({ timeout: 15_000 });
  await list.getByRole("button", { name: /承認を待つ/ }).filter({ hasText: "承認を待つ" }).first().click();
  const sheet = phone.getByRole("dialog").filter({ has: phone.getByRole("textbox", { name: "受信箱の件名" }) });
  await expect(sheet.getByRole("textbox", { name: "受信箱の件名" })).toHaveAccessibleDescription("入力してください", {
    timeout: 15_000,
  });
  await expect(sheet.getByTestId("approval-assignees-error")).toHaveText("承認者を 1 人以上追加してください。");
  expect(await sheet.evaluate((el) => el.scrollWidth <= el.clientWidth + 1)).toBe(true);
  if (SHOTS) await phone.screenshot({ path: `${SHOTS}/fresh-approval-phone.png` });
  await sheet.getByRole("button", { name: "閉じる", exact: true }).click();
  await expect(sheet).toHaveCount(0);
  await phone.getByRole("button", { name: "実行", exact: true }).click();
  await phone.getByRole("button", { name: "実行する" }).click();
  const phoneFailure = phone.getByTestId("run-now-failure");
  await expect(phoneFailure).toContainText("実行できませんでした", { timeout: 15_000 });
  for (const m of BLANK_MESSAGES) await expect(phoneFailure.getByText(m)).toBeVisible();
  expect(await phone.evaluate(() => document.documentElement.scrollWidth <= window.innerWidth + 1)).toBe(true);
  if (SHOTS) await phone.screenshot({ path: `${SHOTS}/fresh-approval-run-phone.png` });
});

test("渡す入力は相手の入力項目の順、表の行は表の列の順で並ぶ（保存する値は変えない・BUG-156・D-128）", async ({
  browser,
  page,
}) => {
  test.setTimeout(150_000);
  await page.setViewportSize({ width: 1440, height: 900 });
  await loginViaKeycloak(page);
  let phone: Page | null = null;
  cleanup.add(async () => {
    await phone?.context().close();
  });
  const headers = await csrfHeaders(page);
  // 相手のワークフロー: 入力項目は 件名・金額・申請者 の順（名前の順は amount・applicant・title）。
  const childName = uniqueName("order-child");
  const child = await page.request.post("/api/workflows", {
    headers,
    data: {
      ir: {
        ir_version: 1,
        name: childName,
        display_name: `並びの相手 ${childName.slice(-6)}`,
        triggers: [{ kind: "interactive" }],
        input_schema: {
          type: "object",
          "x-order": ["title", "amount", "applicant"],
          properties: {
            title: { type: "string", title: "件名" },
            amount: { type: "number", title: "金額" },
            applicant: { type: "string", title: "申請者" },
          },
        },
        nodes: [{ id: "done", type: "workflow.reply", params: { format: "text", value: "受け付けました" } }],
        edges: [],
      },
    },
  });
  expect(child.status(), await child.text()).toBe(201);
  const childId = ((await child.json()) as { id: string }).id;
  cleanup.add(() => removeWorkflowApi(page, childId));
  const parentName = uniqueName("order-parent");
  const parent = await page.request.post("/api/workflows", {
    headers,
    data: {
      ir: {
        ir_version: 1,
        name: parentName,
        display_name: parentName,
        triggers: [{ kind: "interactive" }],
        nodes: [
          {
            id: "start",
            type: "workflow.start",
            label: "申請を回す",
            params: { name: childName, input: { applicant: "山田", title: "出張旅費", amount: 12000 } },
          },
          {
            id: "list",
            type: "workflow.reply",
            label: "受付の一覧",
            params: {
              format: "table",
              value: [{ qty: 2, name: "付箋", code: "A-1" }],
              columns: [
                { label: "品名", path: "/name" },
                { label: "品番", path: "/code" },
                { label: "数量", path: "/qty" },
              ],
            },
          },
        ],
        edges: [{ from: "start", to: "list" }],
      },
    },
  });
  expect(parent.status(), await parent.text()).toBe(201);
  const parentId = ((await parent.json()) as { id: string }).id;
  cleanup.add(() => removeWorkflowApi(page, parentId));
  const savedBefore = await (await page.request.get(`/api/workflows/${parentId}`)).json();

  await page.goto(`/workflows/${parentId}`);
  const canvas = page.locator(".react-flow");
  const panel = page.getByRole("complementary", { name: "ブロックの設定" });
  await expect(canvas.getByText("申請を回す").first()).toBeVisible({ timeout: 15_000 });
  await canvas.getByText("申請を回す").first().click();
  const input = panel.locator('[data-value-expr="渡す入力項目（省略可）"]');
  await expect.poll(() => rowKeys(input), { timeout: 15_000 }).toEqual(["title", "amount", "applicant"]);
  if (SHOTS) await page.screenshot({ path: `${SHOTS}/key-order-start-pc.png` });
  await canvas.getByText("受付の一覧").first().click();
  const rows = panel.locator('[data-value-expr="表にするリスト"]');
  await expect.poll(() => rowKeys(rows), { timeout: 15_000 }).toEqual(["name", "code", "qty"]);
  if (SHOTS) await page.screenshot({ path: `${SHOTS}/key-order-reply-pc.png` });
  // 並べたのは画面だけで、定義は変わっていない（保存の前の状態のまま）。
  await expect(page.getByText(/v1・保存済み/).first()).toBeVisible();
  await expect(page.getByText(/・未保存/)).toHaveCount(0);
  expect(await (await page.request.get(`/api/workflows/${parentId}`)).json()).toEqual(savedBefore);

  // スマホ幅のシートでも同じ並び。
  phone = await phoneOf(browser, page);
  await phone.goto(`/workflows/${parentId}`);
  const list = phone.getByTestId("workflow-mobile-flow");
  await expect(list).toBeVisible({ timeout: 15_000 });
  await list.getByRole("button", { name: /申請を回す/ }).filter({ hasText: "申請を回す" }).first().click();
  const sheet = phone.getByRole("dialog").filter({ has: phone.locator('[data-value-expr="渡す入力項目（省略可）"]') });
  await expect
    .poll(() => rowKeys(sheet.locator('[data-value-expr="渡す入力項目（省略可）"]')), { timeout: 15_000 })
    .toEqual(["title", "amount", "applicant"]);
  expect(await sheet.evaluate((el) => el.scrollWidth <= el.clientWidth + 1)).toBe(true);
  if (SHOTS) await phone.screenshot({ path: `${SHOTS}/key-order-start-phone.png` });
  await sheet.getByRole("button", { name: "閉じる", exact: true }).click();
  await expect(sheet).toHaveCount(0);
  await list.getByRole("button", { name: /受付の一覧/ }).filter({ hasText: "受付の一覧" }).first().click();
  const replySheet = phone.getByRole("dialog").filter({ has: phone.locator('[data-value-expr="表にするリスト"]') });
  await expect
    .poll(() => rowKeys(replySheet.locator('[data-value-expr="表にするリスト"]')), { timeout: 15_000 })
    .toEqual(["name", "code", "qty"]);
  expect(await replySheet.evaluate((el) => el.scrollWidth <= el.clientWidth + 1)).toBe(true);
  if (SHOTS) await phone.screenshot({ path: `${SHOTS}/key-order-reply-phone.png` });
});
