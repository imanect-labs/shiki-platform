import { expect, test, type Page } from "@playwright/test";

import { cleanupAll, cleanupQueue, loginViaKeycloak, removeWorkflowApi, uniqueName } from "./helpers";

/// 入力項目の設定をきっかけと別に開く（GAP-35 の残り）の E2E。
/// - 「手動で実行」のきっかけが無いワークフロー（別のワークフローから呼ばれるもの）でも、画面の上の「入力項目」から
///   入力項目の表を開いて項目を足し、保存できる。
/// - 入力項目の形の誤り（起動で確かめられない oneOf）は「直すところ」の一覧の「入力項目」から開け、編集の下に理由が出る。
/// - スマホ幅でも、⋯ の「入力項目」と「直すところ」の一覧から下のシートで開け、横にはみ出さない。
/// 作ったワークフローは最後に消す。`E2E_SHOTS_DIR` を渡すとスクリーンショットを撮る。

const SHOTS = process.env.E2E_SHOTS_DIR;

const cleanup = cleanupQueue();
test.afterEach(() => cleanup.run());

async function csrfHeaders(page: Page): Promise<Record<string, string>> {
  const csrf = (await page.context().cookies()).find((c) => c.name === "shiki_csrf")?.value ?? "";
  return { "x-csrf-token": csrf, "content-type": "application/json" };
}

/// 起動で確かめられない形の入力項目（oneOf）。保存の確かめが入力項目の中の位置つきで止める。
const ONE_OF = JSON.stringify({ type: "object", properties: { memo: { oneOf: [{ type: "string" }] } } }, null, 2);
const ONE_OF_ERROR = /入力項目の形に「oneOf」は書けません/;

/// きっかけの無い（別のワークフローから呼ばれる）ワークフローを作る。
async function createChildWorkflow(page: Page): Promise<string> {
  const name = uniqueName("child-inputs");
  const res = await page.request.post("/api/workflows", {
    headers: await csrfHeaders(page),
    data: {
      ir: {
        ir_version: 1,
        name,
        display_name: name,
        declared_scopes: [],
        triggers: [],
        nodes: [
          {
            id: "take",
            type: "script.run",
            label: "受け取る",
            params: { source: { inline: "function main(input) { return input; }" } },
          },
        ],
        edges: [],
      },
    },
  });
  expect(res.ok(), await res.text()).toBeTruthy();
  const id = ((await res.json()) as { id: string }).id;
  cleanup.add(() => removeWorkflowApi(page, id));
  return id;
}

test("手動で実行のきっかけが無いワークフローでも、画面の上の「入力項目」から入力項目を直して保存できる（GAP-35）", async ({
  page,
}) => {
  test.setTimeout(180_000);
  await page.setViewportSize({ width: 1440, height: 900 });
  await loginViaKeycloak(page);
  const id = await createChildWorkflow(page);

  await page.goto(`/workflows/${id}`);
  const canvas = page.locator(".react-flow");
  await expect(canvas.getByText("受け取る").first()).toBeVisible({ timeout: 20_000 });
  // きっかけは無いので、入力項目はきっかけの設定からは開けない。画面の上の「入力項目」から開く。
  await expect(page.getByTestId("trigger-node")).toHaveCount(0);
  const open = page.getByTestId("workflow-inputs");
  await open.click();
  await expect(open).toHaveAttribute("aria-pressed", "true");
  const panel = page.getByRole("complementary", { name: "入力項目の設定" });
  await expect(panel.getByTestId("input-schema-editor")).toBeVisible();

  // 項目を足す（表で書ける形）。
  await panel.getByRole("button", { name: "項目を追加" }).click();
  await panel.getByLabel("表示名").fill("申請番号");
  await panel.getByLabel("キー").fill("request_no");
  if (SHOTS) await page.screenshot({ path: `${SHOTS}/workflow-inputs-panel-pc.png` });

  // 起動で確かめられない形にすると、ライブの確かめが「直すところ」に入力項目を挙げ、編集の下に理由が出る。
  await panel.getByRole("button", { name: "JSON で編集" }).click();
  await panel.getByLabel("入力項目（JSON Schema）").fill(ONE_OF);
  const summary = page.getByTestId("workflow-errors");
  await expect(summary).toHaveText("直すところ 1 件", { timeout: 15_000 });
  await expect(panel.getByTestId("input-schema-errors")).toContainText(ONE_OF_ERROR);
  await expect(panel.getByTestId("input-schema-errors")).not.toContainText("/input_schema");
  await expect(open).toContainText("直すところ 1 件");
  if (SHOTS) await page.screenshot({ path: `${SHOTS}/workflow-inputs-panel-error-pc.png` });

  // 閉じてから「直すところ」の一覧の「入力項目」を押すと、入力項目の設定が開く（前は手動で実行のきっかけが無いと開かなかった）。
  await panel.getByRole("button", { name: "パネルを閉じる" }).click();
  await expect(panel).toHaveCount(0);
  await expect(open).toHaveAttribute("aria-pressed", "false");
  await summary.click();
  const list = page.getByTestId("workflow-errors-list");
  const item = list.getByRole("button", { name: /入力項目/ });
  await expect(item).toBeEnabled();
  await expect(item).toContainText(ONE_OF_ERROR);
  if (SHOTS) {
    await page.waitForTimeout(400); // 開くときの動きが終わってから撮る。
    await page.screenshot({ path: `${SHOTS}/workflow-inputs-summary-pc.png` });
  }
  await item.click();
  await expect(panel.getByTestId("input-schema-errors")).toContainText(ONE_OF_ERROR);

  // 直すと誤りが消え、保存できる。保存した定義に入力項目が入る。
  await panel
    .getByLabel("入力項目（JSON Schema）")
    .fill(JSON.stringify({ type: "object", properties: { request_no: { type: "string", title: "申請番号" } } }, null, 2));
  await expect(summary).toHaveCount(0, { timeout: 15_000 });
  await expect(panel.getByTestId("input-schema-errors")).toHaveCount(0);
  const saved = page.waitForResponse((r) => r.request().method() === "PUT" && r.url().endsWith(`/workflows/${id}`));
  await page.getByRole("button", { name: "保存", exact: true }).click();
  expect((await saved).status()).toBe(200);
  const wf = (await (await page.request.get(`/api/workflows/${id}`)).json()) as {
    ir: { input_schema?: { properties?: Record<string, { title?: string }> }; triggers?: unknown[] };
  };
  expect(wf.ir.input_schema?.properties?.request_no?.title).toBe("申請番号");
  expect(wf.ir.triggers ?? []).toEqual([]);
});

test("スマホ幅でも、⋯ の「入力項目」と「直すところ」の一覧から入力項目のシートを開ける（GAP-35）", async ({ page, browser }) => {
  test.setTimeout(180_000);
  await loginViaKeycloak(page);
  const id = await createChildWorkflow(page);

  const ctx = await browser.newContext({
    storageState: await page.context().storageState(),
    viewport: { width: 390, height: 844 },
    hasTouch: true,
    isMobile: true,
    deviceScaleFactor: 2,
    ignoreHTTPSErrors: true,
  });
  try {
    const phone = await ctx.newPage();
    await phone.goto(`/workflows/${id}`);
    const flow = phone.getByTestId("workflow-mobile-flow");
    await expect(flow).toBeVisible({ timeout: 20_000 });
    await expect(flow.getByText("受け取る").first()).toBeVisible();

    // ⋯ の「入力項目」から下のシートで開く。
    await phone.getByTestId("chrome-menu").click();
    await phone.getByTestId("workflow-inputs-menu").click();
    // 下のシート（見出しは「入力項目」）。
    const sheet = phone.getByRole("dialog").filter({ has: phone.getByTestId("input-schema-editor") });
    await expect(sheet.getByTestId("input-schema-editor")).toBeVisible({ timeout: 10_000 });
    await expect(sheet.getByRole("heading", { name: "入力項目" })).toBeVisible();
    const overflow = await phone.evaluate(() => document.documentElement.scrollWidth - window.innerWidth);
    expect(overflow).toBeLessThanOrEqual(0);
    if (SHOTS) await phone.screenshot({ path: `${SHOTS}/workflow-inputs-sheet-phone.png` });

    // 起動で確かめられない形にすると、シートの中の編集の下に理由が出る。閉じると「直すところ」に入力項目が出る。
    await sheet.getByRole("button", { name: "JSON で編集" }).click();
    await sheet.getByLabel("入力項目（JSON Schema）").fill(ONE_OF);
    await expect(sheet.getByTestId("input-schema-errors")).toContainText(ONE_OF_ERROR, { timeout: 15_000 });
    if (SHOTS) await phone.screenshot({ path: `${SHOTS}/workflow-inputs-sheet-error-phone.png` });
    await sheet.getByRole("button", { name: "閉じる" }).click();
    await expect(sheet).toHaveCount(0);
    const summary = phone.getByTestId("workflow-errors");
    await expect(summary).toHaveText("直すところ 1 件");
    await summary.click();
    const item = phone.getByTestId("workflow-errors-list").getByRole("button", { name: /入力項目/ });
    await expect(item).toBeEnabled();
    const listOverflow = await phone.evaluate(() => document.documentElement.scrollWidth - window.innerWidth);
    expect(listOverflow).toBeLessThanOrEqual(0);
    if (SHOTS) {
      await phone.waitForTimeout(400);
      await phone.screenshot({ path: `${SHOTS}/workflow-inputs-summary-phone.png` });
    }
    await item.click();
    await expect(sheet.getByTestId("input-schema-errors")).toContainText(ONE_OF_ERROR);
    // ⋯ の項目にも直すところの数が出る。
    await sheet.getByRole("button", { name: "閉じる" }).click();
    await phone.getByTestId("chrome-menu").click();
    await expect(phone.getByTestId("workflow-inputs-menu")).toHaveText("入力項目（直すところ 1 件）");
    if (SHOTS) {
      await phone.waitForTimeout(400); // 開くときの動きが終わってから撮る。
      await phone.screenshot({ path: `${SHOTS}/workflow-inputs-menu-phone.png` });
    }
    await phone.keyboard.press("Escape");
  } finally {
    await cleanupAll(() => ctx.close());
  }
});
