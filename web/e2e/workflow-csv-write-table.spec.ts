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

/// 「CSV を集計」の結果（表）を「CSV を保存」に渡す e2e（BUG-178）:
/// - 以前は JSON の文字のまま .csv に保存していた。いまは表をそのまま CSV（1 行目が列名）にし、選んだ文字コード
///   （UTF-8（BOM 付き））で保存する。表計算ソフトで式として動きうる値（=1+1）は、先頭に ' を付けて文字にする。
/// - 実行の結果の画面（実行履歴の詳細）と、保存した CSV の画面を PC とスマホで確かめる。
/// - 「CSV の内容」の欄の下に、表や項目のまとまりのリストを CSV に直して保存することを言う。
///
/// 作ったもの（フォルダー・元の CSV・保存した CSV・ワークフロー）は finally で片付ける（互いに独立に）。

const SHOTS = process.env.E2E_SHOTS_DIR;

const SOURCE = "品目,金額\n交通費,1200\n会議費,300\n交通費,800\n=1+1,5\n";
const SAVED = "品目,合計\r\n交通費,2000\r\n会議費,300\r\n'=1+1,5\r\n";
const HINT =
  "1 行目に列名、2 行目から値を、カンマで区切って書きます。「CSV を集計」の結果（表）や、項目のまとまりのリストを入れると、CSV に直して保存します。";

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

/// 「集計を保存」の結果のファイル（ID と名前）。
async function savedFile(page: Page, id: string, runId: string): Promise<{ file_id: string; name: string }> {
  const res = await page.request.get(`/api/workflows/${id}/runs/${runId}/steps?path=save`);
  expect(res.ok(), await res.text()).toBeTruthy();
  return ((await res.json()) as { output: { file_id: string; name: string } }).output;
}

test("CSV を保存: 「CSV を集計」の結果の表を、そのまま CSV にして保存する（BUG-178）", async ({ browser, page }) => {
  test.setTimeout(300_000);
  await page.setViewportSize({ width: 1440, height: 900 });
  await loginViaKeycloak(page);
  let folderId = "";
  let sourceId = "";
  let workflowId = "";
  const savedIds = new Set<string>();
  try {
    folderId = await createFolderApi(page, uniqueName("e2e-CSV の表を保存"));
    const src = await page.request.post("/api/tabular/save", {
      headers: await csrfHeaders(page),
      data: { parent_id: folderId, name: uniqueName("経費"), csv: SOURCE },
    });
    expect(src.ok(), await src.text()).toBeTruthy();
    sourceId = ((await src.json()) as { node_id: string }).node_id;

    const tag = uniqueName("csv-table");
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
              label: "経費を集計",
              params: {
                file: sourceId,
                sql: "SELECT 品目, SUM(金額) AS 合計 FROM data GROUP BY 品目 ORDER BY 合計 DESC",
              },
            },
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

    // 設定の画面: 「CSV の内容」の欄の下で、表やリストを CSV に直して保存することを言う。
    await page.goto(`/workflows/${workflowId}`);
    const panel = page.getByRole("complementary", { name: "ブロックの設定" });
    await page.getByTestId("rf__node-save").click();
    await expect(panel.getByText(HINT, { exact: true })).toBeVisible();
    if (SHOTS) await panel.screenshot({ path: `${SHOTS}/csv-write-table-hint-pc.png` });

    // 画面から実行すると、実行履歴の詳細に移る。
    await page.getByRole("button", { name: "実行", exact: true }).click();
    const dialog = page.getByRole("dialog", { name: "いま実行する" });
    await dialog.getByRole("button", { name: "実行する" }).click();
    await page.waitForURL(/\/runs\?run=/, { timeout: 30_000 });
    const runId = new URL(page.url()).searchParams.get("run") ?? "";
    expect((await finishedRun(page, workflowId, runId)).status).toBe("succeeded");
    const file = await savedFile(page, workflowId, runId);
    savedIds.add(file.file_id);
    expect(file.name).toBe(`集計-${tag}.csv`);

    // 保存した CSV は表のとおり（JSON の文字ではない）。文字コードはサーバーの判定で確かめる。
    const text = (await (await page.request.get(`/api/files/${file.file_id}/text`)).json()) as {
      text: string;
      encoding: string;
      bom: boolean;
    };
    expect(text).toMatchObject({ text: SAVED, encoding: "UTF-8", bom: true });

    // 結果の画面: 「集計を保存」の結果に、保存した名前が出る。
    const sheet = page.getByRole("dialog");
    await sheet.getByRole("button", { name: /集計を保存/ }).click();
    await expect(sheet.getByText("このブロックの結果")).toBeVisible({ timeout: 15_000 });
    await expect(sheet.getByText(`集計-${tag}.csv`).first()).toBeVisible();
    if (SHOTS) await page.screenshot({ path: `${SHOTS}/csv-write-table-run-pc.png` });

    // 保存した CSV を開くと、1 行目を列名にした 3 行 × 2 列の表になる（グリッドは canvas に描くので、中身は上の
    // テキストの API で確かめ、ここでは行と列の数とスクリーンショットで見る）。
    const pcRows = page.waitForResponse((r) => r.url().includes(`/api/files/${file.file_id}/tabular/rows`) && r.ok(), {
      timeout: 30_000,
    });
    await page.goto(`/csv/${file.file_id}`);
    await expect(page.getByTestId("csv-grid")).toBeVisible({ timeout: 20_000 });
    await expect(page.getByText(/3 行 × 2 列/)).toBeVisible({ timeout: 15_000 });
    await pcRows;
    await page.waitForTimeout(500);
    if (SHOTS) await page.screenshot({ path: `${SHOTS}/csv-write-table-file-pc.png` });

    // スマホ: 結果の画面・保存した CSV・設定の欄の下の案内。横にはみ出さない。
    const phoneCtx = await phoneContext(browser, page);
    try {
      const phone = await phoneCtx.newPage();
      await phone.goto(`/workflows/${workflowId}/runs?run=${runId}`);
      const phoneSheet = phone.getByRole("dialog");
      await phoneSheet.getByRole("button", { name: /集計を保存/ }).click({ timeout: 30_000 });
      await expect(phoneSheet.getByText("このブロックの結果")).toBeVisible({ timeout: 15_000 });
      await phoneSheet.getByText("このブロックの結果").scrollIntoViewIfNeeded();
      expect(await phone.evaluate(() => document.documentElement.scrollWidth)).toBeLessThanOrEqual(390);
      if (SHOTS) await phone.screenshot({ path: `${SHOTS}/csv-write-table-run-phone.png` });

      // スマホの見出しは行と列の数を出さないので、行を読み終えるのを待ってから見る。
      const rows = phone.waitForResponse((r) => r.url().includes(`/api/files/${file.file_id}/tabular/rows`) && r.ok(), {
        timeout: 30_000,
      });
      await phone.goto(`/csv/${file.file_id}`);
      await expect(phone.getByTestId("csv-grid")).toBeVisible({ timeout: 20_000 });
      const body = (await (await rows).json()) as { columns?: string[]; rows?: unknown[] };
      expect(body.columns).toEqual(["品目", "合計"]);
      expect(body.rows?.length).toBe(3);
      await phone.waitForTimeout(500);
      expect(await phone.evaluate(() => document.documentElement.scrollWidth)).toBeLessThanOrEqual(390);
      if (SHOTS) await phone.screenshot({ path: `${SHOTS}/csv-write-table-file-phone.png` });

      await phone.goto(`/workflows/${workflowId}`);
      const flow = phone.getByTestId("workflow-mobile-flow");
      await expect(flow).toBeVisible({ timeout: 15_000 });
      await flow.getByRole("button", { name: /集計を保存/ }).filter({ hasText: "集計を保存" }).first().click();
      const settings = phone.getByRole("dialog");
      const hint = settings.getByText(HINT, { exact: true });
      await expect(hint).toBeVisible();
      await hint.evaluate((el) => el.scrollIntoView({ block: "center" }));
      expect(await settings.evaluate((el) => el.scrollWidth <= el.clientWidth + 1)).toBe(true);
      if (SHOTS) await phone.screenshot({ path: `${SHOTS}/csv-write-table-hint-phone.png` });
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
