import { expect, test, type Browser, type Page } from "@playwright/test";

import {
  cleanupAll,
  createFolderApi,
  loginViaKeycloak,
  removeWorkflowApi,
  trashFileApi,
  trashFolderApi,
  uniqueName,
} from "./helpers";

/// 「CSV を集計」でほかの CSV をつなぐ e2e（つなぐ CSV の欄・#1037）:
/// - つなぐ CSV が無いときは「つなぐ CSV を足す」のボタンだけを出す（欄が邪魔にならない）。
/// - ボタンからドライブの CSV を選ぶと 1 件足し、名前はファイル名から付ける（英字の無い名前は t1）。名前は SQL で読む
///   名前として直せ、使えない名前（data・大文字など）はその場で言う。直すまで試しに実行できない。
/// - 試しに実行・ワークフローの実行の両方で、つないだ集計の結果が出る。保存した定義の tables は選んだとおり。
/// - 外すと、ボタンだけに戻り、定義から tables が消える。スマホでも欄が横にはみ出さない。
///
/// 作ったもの（フォルダー・CSV・ワークフロー）は finally で片付ける（互いに独立に）。

const SHOTS = process.env.E2E_SHOTS_DIR;
const ITEMS = "品目,数量\nりんご,3\nみかん,5\nりんご,2\n";
const PRICES = "品目,単価\nりんご,120\nみかん,80\n";
/// 選んだ行が見つからないと言うときの、行の下の理由と直し方（GAP-91 と同じ言い方）。
const FILE_HINT = "削除されたか、見る権限がありません。ファイルを選び直してください。";
const SQL =
  'SELECT d."品目", SUM(d."数量" * m."単価") AS 金額 FROM data d JOIN master m ON d."品目" = m."品目" GROUP BY d."品目" ORDER BY 金額 DESC';

/// ドライブの先頭に並ぶ名前（ファイルを選ぶ画面の 1 ページ目に出るよう、時刻を逆順にした固定長の鍵を頭に付ける）。
function firstInDriveName(prefix: string): string {
  const key = (Number.MAX_SAFE_INTEGER - Date.now()).toString(36).padStart(11, "0");
  return `00-${key}-${prefix}-${Math.floor(Math.random() * 1e4)}`;
}

async function csrfHeaders(page: Page): Promise<Record<string, string>> {
  const csrf = (await page.context().cookies()).find((c) => c.name === "shiki_csrf")?.value ?? "";
  return { "x-csrf-token": csrf, "content-type": "application/json" };
}

async function saveCsv(page: Page, folderId: string, name: string, csv: string): Promise<string> {
  const res = await page.request.post("/api/tabular/save", {
    headers: await csrfHeaders(page),
    data: { parent_id: folderId, name, csv },
  });
  expect(res.ok(), await res.text()).toBeTruthy();
  return ((await res.json()) as { node_id: string }).node_id;
}

async function phoneContext(browser: Browser, page: Page) {
  return browser.newContext({
    storageState: await page.context().storageState(),
    viewport: { width: 390, height: 844 },
    hasTouch: true,
    isMobile: true,
    deviceScaleFactor: 3,
    ignoreHTTPSErrors: true,
  });
}

type SavedIr = { ir: { nodes: { id: string; params: Record<string, unknown> }[] } };

test("CSV を集計: つなぐ CSV を足し、名前を付けて SQL でつなぎ、外せる（#1037）", async ({ browser, page }) => {
  test.setTimeout(300_000);
  await page.setViewportSize({ width: 1440, height: 900 });
  await loginViaKeycloak(page);
  let folderId = "";
  const fileIds: string[] = [];
  let workflowId = "";
  try {
    const folderName = firstInDriveName("e2e-CSV をつなぐ");
    folderId = await createFolderApi(page, folderName);
    const itemsName = `${uniqueName("明細")}.csv`;
    // 英字を含まない名前（SQL での名前は t1 になる）。
    const pricesName = `単価-${Date.now()}${Math.floor(Math.random() * 1e4)}.csv`;
    const itemsId = await saveCsv(page, folderId, itemsName, ITEMS);
    fileIds.push(itemsId);
    const pricesId = await saveCsv(page, folderId, pricesName, PRICES);
    fileIds.push(pricesId);

    const tag = uniqueName("csv-join");
    const created = await page.request.post("/api/workflows", {
      headers: await csrfHeaders(page),
      data: {
        ir: {
          ir_version: 1,
          name: tag,
          display_name: tag,
          triggers: [{ kind: "interactive" }],
          nodes: [{ id: "q", type: "csv.query", label: "金額を集計", params: { file: itemsId, sql: SQL } }],
          edges: [],
        },
      },
    });
    expect(created.ok(), await created.text()).toBeTruthy();
    workflowId = ((await created.json()) as { id: string }).id;

    // つなぐ CSV が無いときは、ボタンだけ。
    await page.goto(`/workflows/${workflowId}`);
    const panel = page.getByRole("complementary", { name: "ブロックの設定" });
    await page.getByTestId("rf__node-q").click();
    await expect(panel.getByText(itemsName)).toBeVisible({ timeout: 15_000 });
    const tables = panel.getByTestId("csv-tables");
    const add = tables.getByRole("button", { name: "つなぐ CSV を足す" });
    await expect(add).toBeVisible();
    await expect(tables.getByRole("group", { name: "つなぐ CSV" })).toHaveCount(0);
    await expect(tables.getByTestId("csv-table")).toHaveCount(0);
    if (SHOTS) await panel.screenshot({ path: `${SHOTS}/csv-query-join-empty-pc.png` });

    // ボタンからドライブの CSV を選ぶと 1 件足す（日本語の名前は t1）。
    await add.click();
    const picker = page.getByRole("dialog", { name: "つなぐ CSV を選ぶ" });
    await picker.getByRole("button", { name: folderName }).click({ timeout: 15_000 });
    await picker.getByRole("button", { name: pricesName }).click({ timeout: 15_000 });
    await expect(picker).toHaveCount(0);
    const row = tables.getByTestId("csv-table");
    await expect(row).toHaveCount(1);
    await expect(row).toContainText(pricesName);
    const name = row.getByRole("textbox", { name: "1 つ目の CSV の SQL での名前" });
    await expect(name).toHaveValue("t1");

    // 使えない名前はその場で言い、直すまで試しに実行できない。
    await name.fill("data");
    await expect(row).toContainText("data は集計するファイルの名前です。ほかの名前にしてください。");
    await expect(panel.getByRole("button", { name: "試しに実行" })).toBeDisabled();
    await expect(panel.getByText("つなぐ CSV の名前とファイルを直すと試せます")).toBeVisible();
    await name.fill("Master");
    await expect(row).toContainText("英小文字・数字・_ で書き、先頭は数字以外にしてください。");
    await name.fill("master");
    await expect(row).not.toContainText("先頭は数字以外");
    await expect(tables).toContainText("例: SELECT * FROM data JOIN master ON …");
    await expect(panel.getByTestId("csv-columns")).toContainText("テーブル名は data です（つなぐ CSV は master）。");

    // 試しに実行すると、つないだ集計の結果が出る。
    await panel.getByRole("button", { name: "試しに実行" }).click();
    const preview = panel.getByTestId("csv-preview");
    await expect(preview).toContainText("金額", { timeout: 20_000 });
    await expect(preview).toContainText("りんご");
    await expect(preview).toContainText("600");
    await expect(preview).toContainText("400");
    await tables.scrollIntoViewIfNeeded();
    if (SHOTS) await panel.screenshot({ path: `${SHOTS}/csv-query-join-panel-pc.png` });

    // 保存した定義の tables は選んだとおり。
    await page.getByRole("button", { name: "保存", exact: true }).click();
    await expect(page.getByText(/保存しました/).first()).toBeVisible({ timeout: 15_000 });
    const saved = (await (await page.request.get(`/api/workflows/${workflowId}`)).json()) as SavedIr;
    expect(saved.ir.nodes.find((n) => n.id === "q")?.params.tables).toEqual([{ name: "master", file: pricesId }]);

    // ワークフローの実行でも、つないだ集計の結果になる。
    await page.getByRole("button", { name: "実行", exact: true }).click();
    const dialog = page.getByRole("dialog", { name: "いま実行する" });
    await dialog.getByRole("button", { name: "実行する" }).click();
    await page.waitForURL(/\/runs\?run=/, { timeout: 30_000 });
    const runId = new URL(page.url()).searchParams.get("run") ?? "";
    await expect
      .poll(
        async () =>
          ((await (await page.request.get(`/api/workflows/${workflowId}/runs/${runId}`)).json()) as { status: string })
            .status,
        { timeout: 60_000, intervals: [1_000] },
      )
      .toBe("succeeded");
    const step = (await (
      await page.request.get(`/api/workflows/${workflowId}/runs/${runId}/steps?path=q`)
    ).json()) as { output: { items: unknown[] } };
    expect(step.output.items).toEqual([
      { 品目: "りんご", 金額: 600 },
      { 品目: "みかん", 金額: 400 },
    ]);

    // スマホ: つなぐ CSV の欄が横にはみ出さない。
    const phoneCtx = await phoneContext(browser, page);
    try {
      const phone = await phoneCtx.newPage();
      await phone.goto(`/workflows/${workflowId}`);
      const flow = phone.getByTestId("workflow-mobile-flow");
      await expect(flow).toBeVisible({ timeout: 15_000 });
      await flow.getByRole("button", { name: /金額を集計/ }).filter({ hasText: "金額を集計" }).first().click();
      const settings = phone.getByRole("dialog");
      const phoneRow = settings.getByTestId("csv-table");
      await expect(phoneRow).toContainText(pricesName, { timeout: 15_000 });
      await phoneRow.evaluate((el) => el.scrollIntoView({ block: "center" }));
      expect(await settings.evaluate((el) => el.scrollWidth <= el.clientWidth + 1)).toBe(true);
      expect(await phone.evaluate(() => document.documentElement.scrollWidth)).toBeLessThanOrEqual(390);
      if (SHOTS) await phone.screenshot({ path: `${SHOTS}/csv-query-join-panel-phone.png` });
      await settings.getByRole("button", { name: "閉じる" }).first().click();
    } finally {
      await phoneCtx.close();
    }

    // 外すと、ボタンだけに戻り、保存した定義から tables が消える。
    await page.goto(`/workflows/${workflowId}`);
    await page.getByTestId("rf__node-q").click();
    await panel.getByRole("button", { name: "1 つ目の CSV を外す" }).click();
    await expect(tables.getByTestId("csv-table")).toHaveCount(0);
    await expect(tables.getByRole("button", { name: "つなぐ CSV を足す" })).toBeVisible();
    await page.getByRole("button", { name: "保存", exact: true }).click();
    await expect
      .poll(
        async () => {
          const ir = (await (await page.request.get(`/api/workflows/${workflowId}`)).json()) as SavedIr;
          return "tables" in (ir.ir.nodes.find((n) => n.id === "q")?.params ?? {});
        },
        { timeout: 15_000 },
      )
      .toBe(false);
  } finally {
    await cleanupAll(
      async () => {
        if (workflowId) await removeWorkflowApi(page, workflowId);
      },
      ...fileIds.map((id) => () => trashFileApi(page, id)),
      async () => {
        if (folderId) await trashFolderApi(page, folderId);
      },
    );
  }
});

/// つないだ CSV を消した定義（GAP-91 と同じ形）: その件の行が「見つからないファイル」と言い、理由と直し方はその件の下に
/// 1 回だけ出る（保存の誤りを欄の全体に重ねない）。保存は止まる。
test("CSV を集計: つないだ CSV が消えたら、その件の行で言い、保存を止める（#1037・GAP-91）", async ({ page }) => {
  test.setTimeout(180_000);
  await page.setViewportSize({ width: 1440, height: 900 });
  await loginViaKeycloak(page);
  let folderId = "";
  const fileIds: string[] = [];
  let workflowId = "";
  try {
    folderId = await createFolderApi(page, uniqueName("e2e-消えた CSV"));
    const itemsName = `${uniqueName("明細")}.csv`;
    const itemsId = await saveCsv(page, folderId, itemsName, ITEMS);
    fileIds.push(itemsId);
    const pricesId = await saveCsv(page, folderId, `${uniqueName("単価")}.csv`, PRICES);
    fileIds.push(pricesId);
    const tag = uniqueName("csv-join-gone");
    const created = await page.request.post("/api/workflows", {
      headers: await csrfHeaders(page),
      data: {
        ir: {
          ir_version: 1,
          name: tag,
          display_name: tag,
          triggers: [{ kind: "interactive" }],
          nodes: [
            {
              id: "q",
              type: "csv.query",
              label: "金額を集計",
              params: { file: itemsId, sql: SQL, tables: [{ name: "master", file: pricesId }] },
            },
          ],
          edges: [],
        },
      },
    });
    expect(created.ok(), await created.text()).toBeTruthy();
    workflowId = ((await created.json()) as { id: string }).id;
    // 保存した後に、つないだ CSV を消す。
    await trashFileApi(page, pricesId);
    fileIds.pop();

    await page.goto(`/workflows/${workflowId}`);
    const panel = page.getByRole("complementary", { name: "ブロックの設定" });
    await page.getByTestId("rf__node-q").click();
    await expect(panel.getByText(itemsName)).toBeVisible({ timeout: 15_000 });
    const row = panel.getByTestId("csv-table");
    await expect(row.getByText("見つからないファイル", { exact: true })).toBeVisible({ timeout: 15_000 });
    await expect(row.getByText(FILE_HINT, { exact: true })).toBeVisible();
    await expect(panel.getByText("つなぐ CSV の名前とファイルを直すと試せます")).toHaveCount(0);

    // 変更を入れると、ライブの確かめが同じ誤りを返す。理由は 1 回だけ（欄の全体に重ねない）。
    await panel.getByLabel("ブロック名").fill("金額を集計（月次）");
    await expect(page.locator(".react-flow").locator('[aria-label="検証エラー 1 件"]').first()).toBeVisible({
      timeout: 15_000,
    });
    await expect(panel.getByText(/削除されたか、/)).toHaveCount(1);
    await expect(panel).not.toContainText("選んだファイルが見つかりません");
    await expect(panel).not.toContainText("/params/tables");
    await row.scrollIntoViewIfNeeded();
    if (SHOTS) await panel.screenshot({ path: `${SHOTS}/csv-query-join-gone-pc.png` });

    // 保存は止まる。
    const saved = page.waitForResponse(
      (r) => r.request().method() === "PUT" && r.url().endsWith(`/workflows/${workflowId}`),
    );
    await page.getByRole("button", { name: "保存", exact: true }).click();
    expect((await saved).status()).toBe(400);
  } finally {
    await cleanupAll(
      async () => {
        if (workflowId) await removeWorkflowApi(page, workflowId);
      },
      ...fileIds.map((id) => () => trashFileApi(page, id)),
      async () => {
        if (folderId) await trashFolderApi(page, folderId);
      },
    );
  }
});
