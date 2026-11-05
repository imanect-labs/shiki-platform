import { readFileSync } from "node:fs";
import path from "node:path";

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

/// 「CSV を集計」で Excel（.xlsx）を読み、結果を「CSV を保存」に渡す e2e（D-17・GAP-40）:
/// - Excel は左端のシートを表として読み、いちばん上の行を列の名前にする（2 つ目のシートは読まない）。
/// - 列の案内に、集計で読むときの列の型（日付・テキスト・整数・はい／いいえ）が出る。
/// - 結果の行の値（items）は数値を種類のまま渡し、rows はテキストのまま。結果の全体を「CSV を保存」に渡すと CSV になる。
/// - 設定の画面（PC とスマホ）と、試しに実行した結果を目で確かめる。
///
/// 材料は crates/tabular/tests/fixtures/expenses.xlsx（make_expenses_xlsx.py）。作ったもの（フォルダー・Excel・保存した
/// CSV・ワークフロー）は finally で片付ける（互いに独立に）。

const SHOTS = process.env.E2E_SHOTS_DIR;
const FIXTURE = path.join(__dirname, "..", "..", "crates", "tabular", "tests", "fixtures", "expenses.xlsx");
const XLSX_TYPE = "application/vnd.openxmlformats-officedocument.spreadsheetml.sheet";
const SQL = "SELECT 品目, SUM(金額) AS 合計, COUNT(*) AS 件数 FROM data GROUP BY 品目 ORDER BY 合計 DESC";
const SAVED = "品目,合計,件数\r\n宿泊費,9800,1\r\n会議費,3000,1\r\n交通費,2000,2\r\n";
const VALUES_HINT =
  "結果の行（rows）は、値をすべてテキストで渡します。数値やはい／いいえを種類のまま使うときは、行の値（items）を選び、列の名前で引きます。";

/// ドライブの先頭に並ぶ名前（ファイルを選ぶ画面の 1 ページ目に出るよう、時刻を逆順にした固定長の鍵を頭に付ける）。
function firstInDriveName(prefix: string): string {
  const key = (Number.MAX_SAFE_INTEGER - Date.now()).toString(36).padStart(11, "0");
  return `00-${key}-${prefix}-${Math.floor(Math.random() * 1e4)}`;
}

async function csrfHeaders(page: Page): Promise<Record<string, string>> {
  const csrf = (await page.context().cookies()).find((c) => c.name === "shiki_csrf")?.value ?? "";
  return { "x-csrf-token": csrf, "content-type": "application/json" };
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

/// ドライブの画面からファイルを上げ、ID を返す。
async function upload(page: Page, folderId: string, name: string, buffer: Buffer): Promise<string> {
  await page.goto(`/drive?folder=${folderId}`);
  await page.locator('input[type="file"][multiple]').setInputFiles({ name, mimeType: XLSX_TYPE, buffer });
  await expect(page.getByText(name, { exact: true })).toBeVisible({ timeout: 20_000 });
  let id = "";
  await expect
    .poll(
      async () => {
        const res = await page.request.get(`/api/nodes?parent_id=${folderId}`);
        const items = ((await res.json()) as { items?: { id: string; name: string }[] }).items ?? [];
        id = items.find((c) => c.name === name)?.id ?? "";
        return id;
      },
      { timeout: 30_000 },
    )
    .not.toBe("");
  return id;
}

/// 終わるまで待った実行の詳細。
async function finishedRun(page: Page, id: string, runId: string) {
  await expect
    .poll(
      async () => ((await (await page.request.get(`/api/workflows/${id}/runs/${runId}`)).json()) as { status: string }).status,
      { timeout: 60_000, intervals: [1_000] },
    )
    .toMatch(/^(succeeded|failed)$/);
  return (await (await page.request.get(`/api/workflows/${id}/runs/${runId}`)).json()) as { status: string };
}

/// ブロックの結果。
async function stepOutput<T>(page: Page, id: string, runId: string, node: string): Promise<T> {
  const res = await page.request.get(`/api/workflows/${id}/runs/${runId}/steps?path=${node}`);
  expect(res.ok(), await res.text()).toBeTruthy();
  return ((await res.json()) as { output: T }).output;
}

test("CSV を集計: Excel の左端のシートを読み、結果を CSV を保存に渡せる（D-17・GAP-40）", async ({ browser, page }) => {
  test.setTimeout(300_000);
  await page.setViewportSize({ width: 1440, height: 900 });
  await loginViaKeycloak(page);
  let folderId = "";
  let sourceId = "";
  let workflowId = "";
  const savedIds = new Set<string>();
  try {
    const folderName = firstInDriveName("e2e-Excel を集計");
    folderId = await createFolderApi(page, folderName);
    const xlsxName = `${uniqueName("経費")}.xlsx`;
    sourceId = await upload(page, folderId, xlsxName, readFileSync(FIXTURE));

    const tag = uniqueName("csv-xlsx");
    const created = await page.request.post("/api/workflows", {
      headers: await csrfHeaders(page),
      data: {
        ir: {
          ir_version: 1,
          name: tag,
          display_name: tag,
          triggers: [{ kind: "interactive" }],
          nodes: [
            { id: "q", type: "csv.query", label: "経費を集計", params: { file: sourceId, sql: SQL } },
            {
              id: "save",
              type: "csv.write",
              label: "集計を保存",
              params: {
                folder: folderId,
                name: `集計-${tag}`,
                content: { $from: "nodes.q.output" },
                encoding: "utf8_bom",
                on_name_taken: "add_number",
              },
            },
          ],
          edges: [{ from: "q", to: "save" }],
        },
      },
    });
    expect(created.ok(), await created.text()).toBeTruthy();
    workflowId = ((await created.json()) as { id: string }).id;

    // 設定の画面: Excel のファイル名、列の名前と型（左端のシート）、結果の値の渡り方が出る。
    await page.goto(`/workflows/${workflowId}`);
    const panel = page.getByRole("complementary", { name: "ブロックの設定" });
    await page.getByTestId("rf__node-q").click();
    await expect(panel.getByText(xlsxName)).toBeVisible({ timeout: 15_000 });
    await expect(panel.getByText("Excel のファイルは、表示しているシートのうち左端のシートを読み、値のある最初の行を列の名前にします。", { exact: false })).toBeVisible();
    const columns = panel.getByTestId("csv-columns");
    await expect(columns).toContainText("日付（日付）", { timeout: 20_000 });
    await expect(columns).toContainText("品目（テキスト）");
    await expect(columns).toContainText("金額（整数）");
    await expect(columns).toContainText("精算済み（はい／いいえ）");
    await expect(columns).toContainText("社員番号（テキスト）");
    await expect(columns).toContainText("・4 行");
    await expect(panel.getByTestId("csv-result-values-hint")).toHaveText(VALUES_HINT);
    // ブロックの説明も Excel を読むと言う。
    await expect(panel.getByText("CSV か Excel のファイルを SQL で集計したり", { exact: false }).first()).toBeVisible();

    // ファイルを選ぶ画面にも Excel が出る（CSV だけに絞らない・D-17）。
    await panel.getByRole("button", { name: "集計する CSV か Excel のファイルを変更" }).click();
    const picker = page.getByRole("dialog", { name: "CSV か Excel のファイルを選ぶ" });
    await picker.getByRole("button", { name: folderName }).click({ timeout: 15_000 });
    await expect(picker.getByRole("button", { name: xlsxName })).toBeVisible({ timeout: 15_000 });
    await picker.getByRole("button", { name: xlsxName }).click();
    await expect(picker).toHaveCount(0);

    // 試しに実行すると、本人の権限で集計した上位の行が見える。
    await panel.getByRole("button", { name: "試しに実行" }).click();
    const preview = panel.getByTestId("csv-preview");
    await expect(preview).toContainText("合計", { timeout: 20_000 });
    await expect(preview).toContainText("宿泊費");
    await expect(preview).toContainText("9800");
    await columns.scrollIntoViewIfNeeded();
    if (SHOTS) await panel.screenshot({ path: `${SHOTS}/csv-query-xlsx-panel-pc.png` });

    // 画面から実行すると、実行履歴の詳細に移る。
    await page.getByRole("button", { name: "実行", exact: true }).click();
    const dialog = page.getByRole("dialog", { name: "いま実行する" });
    await dialog.getByRole("button", { name: "実行する" }).click();
    await page.waitForURL(/\/runs\?run=/, { timeout: 30_000 });
    const runId = new URL(page.url()).searchParams.get("run") ?? "";
    expect((await finishedRun(page, workflowId, runId)).status).toBe("succeeded");

    // 集計の結果: items は数値を種類のまま、rows はテキストのまま。
    const out = await stepOutput<{ items: unknown[]; rows: unknown[][]; column_types: string[] }>(
      page,
      workflowId,
      runId,
      "q",
    );
    expect(out.items).toEqual([
      { 品目: "宿泊費", 合計: 9800, 件数: 1 },
      { 品目: "会議費", 合計: 3000, 件数: 1 },
      { 品目: "交通費", 合計: 2000, 件数: 2 },
    ]);
    expect(out.rows[0]).toEqual(["宿泊費", "9800", "1"]);
    expect(out.column_types).toEqual(["VARCHAR", "HUGEINT", "BIGINT"]);

    // 保存した CSV は表のとおり（BOM 付きの UTF-8）。
    const file = await stepOutput<{ file_id: string; name: string }>(page, workflowId, runId, "save");
    savedIds.add(file.file_id);
    expect(file.name).toBe(`集計-${tag}.csv`);
    const text = (await (await page.request.get(`/api/files/${file.file_id}/text`)).json()) as {
      text: string;
      encoding: string;
      bom: boolean;
    };
    expect(text).toMatchObject({ text: SAVED, encoding: "UTF-8", bom: true });

    // 結果の画面: 「経費を集計」の結果に、集計した値が出る。
    const sheet = page.getByRole("dialog");
    await sheet.getByRole("button", { name: /経費を集計/ }).click();
    await expect(sheet.getByText("このブロックの結果")).toBeVisible({ timeout: 15_000 });
    await expect(sheet.getByText("宿泊費").first()).toBeVisible();
    if (SHOTS) await page.screenshot({ path: `${SHOTS}/csv-query-xlsx-run-pc.png` });

    // スマホ: 設定の欄（ファイル・列の案内・値の渡り方）が横にはみ出さない。
    const phoneCtx = await phoneContext(browser, page);
    try {
      const phone = await phoneCtx.newPage();
      await phone.goto(`/workflows/${workflowId}`);
      const flow = phone.getByTestId("workflow-mobile-flow");
      await expect(flow).toBeVisible({ timeout: 15_000 });
      await flow.getByRole("button", { name: /経費を集計/ }).filter({ hasText: "経費を集計" }).first().click();
      const settings = phone.getByRole("dialog");
      await expect(settings.getByText(xlsxName)).toBeVisible({ timeout: 15_000 });
      const phoneColumns = settings.getByTestId("csv-columns");
      await expect(phoneColumns).toContainText("金額（整数）", { timeout: 20_000 });
      await phoneColumns.evaluate((el) => el.scrollIntoView({ block: "center" }));
      expect(await settings.evaluate((el) => el.scrollWidth <= el.clientWidth + 1)).toBe(true);
      expect(await phone.evaluate(() => document.documentElement.scrollWidth)).toBeLessThanOrEqual(390);
      if (SHOTS) await phone.screenshot({ path: `${SHOTS}/csv-query-xlsx-panel-phone.png` });
      await settings.getByRole("button", { name: "閉じる" }).first().click();
    } finally {
      await phoneCtx.close();
    }
  } finally {
    await cleanupAll(
      async () => {
        if (workflowId) await removeWorkflowApi(page, workflowId);
      },
      ...[...savedIds].map((fileId) => () => trashFileApi(page, fileId)),
      async () => {
        if (sourceId) await trashFileApi(page, sourceId);
      },
      async () => {
        if (folderId) await trashFolderApi(page, folderId);
      },
    );
  }
});

/// 読めない Excel を選ぶと、列の案内と試しに実行の結果に、サーバーの理由と直し方が出る（D-17）。以前の画面は
/// 「CSV として読めないか、閲覧の権限がありません」「SQL が拒否されました」と言い、理由が分からなかった。
test("CSV を集計: 読めない Excel は、列の案内と試しに実行で理由を言う（D-17）", async ({ page }) => {
  test.setTimeout(180_000);
  await page.setViewportSize({ width: 1440, height: 900 });
  await loginViaKeycloak(page);
  let folderId = "";
  let sourceId = "";
  let workflowId = "";
  const reason = "Excel のファイル（.xlsx）として読めませんでした。ファイルが壊れていないか確かめてください。";
  try {
    folderId = await createFolderApi(page, uniqueName("e2e-壊れた Excel"));
    const name = `${uniqueName("壊れた")}.xlsx`;
    // zip の印だけがある、中身の壊れたファイル（実行ごとに中身を変える）。
    sourceId = await upload(page, folderId, name, Buffer.concat([Buffer.from("PK\x03\x04"), Buffer.from(name)]));
    const tag = uniqueName("csv-xlsx-broken");
    const created = await page.request.post("/api/workflows", {
      headers: await csrfHeaders(page),
      data: {
        ir: {
          ir_version: 1,
          name: tag,
          display_name: tag,
          triggers: [{ kind: "interactive" }],
          nodes: [{ id: "q", type: "csv.query", label: "経費を集計", params: { file: sourceId, sql: "SELECT * FROM data" } }],
          edges: [],
        },
      },
    });
    expect(created.ok(), await created.text()).toBeTruthy();
    workflowId = ((await created.json()) as { id: string }).id;

    await page.goto(`/workflows/${workflowId}`);
    const panel = page.getByRole("complementary", { name: "ブロックの設定" });
    await page.getByTestId("rf__node-q").click();
    await expect(panel.getByTestId("csv-columns")).toContainText(reason, { timeout: 20_000 });
    await panel.getByRole("button", { name: "試しに実行" }).click();
    await expect(panel.getByRole("alert").filter({ hasText: reason })).toBeVisible({ timeout: 20_000 });
    if (SHOTS) await panel.screenshot({ path: `${SHOTS}/csv-query-xlsx-broken-pc.png` });
  } finally {
    await cleanupAll(
      async () => {
        if (workflowId) await removeWorkflowApi(page, workflowId);
      },
      async () => {
        if (sourceId) await trashFileApi(page, sourceId);
      },
      async () => {
        if (folderId) await trashFolderApi(page, folderId);
      },
    );
  }
});
