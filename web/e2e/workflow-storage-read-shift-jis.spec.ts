import { expect, test, type Page } from "@playwright/test";

import { shiftJis } from "./fixtures/shift-jis";
import { cleanupAll, loginViaKeycloak, removeWorkflowApi, uniqueName } from "./helpers";

/// 「ファイルを読む」（storage.read）が Shift_JIS の CSV を読む e2e（GAP-45）:
/// 日本語の Windows の Excel が既定で保存する Shift_JIS の CSV をドライブに上げ、ワークフローの「ファイルを読む」で
/// 読むと、文字化けせずに中身の文章が結果に入り、読んだ文字コード（encoding）が Shift_JIS になる。以前は
/// UTF-8 でない中身として not_text_file で断っていた（6 回目）。読める文字コードは「読むファイル」の「詳しく」で言う。

const SHOTS = process.env.E2E_SHOTS_DIR;

async function csrfHeaders(page: Page): Promise<Record<string, string>> {
  const csrf = (await page.context().cookies()).find((c) => c.name === "shiki_csrf")?.value ?? "";
  return { "x-csrf-token": csrf, "content-type": "application/json" };
}

/// 「経費,金額」の CSV（Shift_JIS で上げる）。
const EXPENSES = "経費,金額\n交通費,1200\n宿泊費,8000\n";

const HINT =
  "テキストのファイル（.txt、.md、.csv など）は中身を、PDF や Word は社内文書の検索に取り込まれた本文を読みます。";
/// 読める文字コードは「詳しく」で言う（注記は 2 文まで・D-111 で PDF や Word の読み方を注記に足した）。
const ENCODING_MORE = "テキストのファイルは、UTF-8 か Shift_JIS で保存したものを読みます。";

type RunDetail = { status: string; steps: { node_id: string; status: string; error?: { code?: string } | null }[] };

test("ファイルを読む: Shift_JIS の CSV を文字化けせずに読む（GAP-45）", async ({ browser, page }) => {
  test.setTimeout(180_000);
  await page.setViewportSize({ width: 1440, height: 900 });
  await loginViaKeycloak(page);
  let folderId = "";
  let id = "";
  let fileId = "";
  try {
    const folder = await page.request.post("/api/folders", {
      headers: await csrfHeaders(page),
      data: { name: uniqueName("e2e-文字コード"), parent_id: null },
    });
    expect(folder.ok(), await folder.text()).toBeTruthy();
    folderId = ((await folder.json()) as { id: string }).id;

    // ドライブの画面から Shift_JIS の CSV を上げる（Excel を入れた Windows のブラウザーは ms-excel の MIME を付ける）。
    await page.goto(`/drive?folder=${folderId}`);
    const csvName = `${uniqueName("経費")}.csv`;
    const bytes = shiftJis(EXPENSES);
    // UTF-8 として読むと壊れる中身であること（以前は not_text_file で断っていた）。
    expect(() => new TextDecoder("utf-8", { fatal: true }).decode(bytes)).toThrow();
    await page.locator('input[type="file"][multiple]').setInputFiles({
      name: csvName,
      mimeType: "application/vnd.ms-excel",
      buffer: bytes,
    });
    await expect(page.getByText(csvName, { exact: true })).toBeVisible({ timeout: 20_000 });
    await expect
      .poll(
        async () => {
          const res = await page.request.get(`/api/nodes?parent_id=${folderId}`);
          const children = (await res.json()) as { items?: { id: string; name: string }[] };
          fileId = children.items?.find((c) => c.name === csvName)?.id ?? "";
          return fileId;
        },
        { timeout: 30_000 },
      )
      .not.toBe("");

    const name = uniqueName("read-sjis");
    const created = await page.request.post("/api/workflows", {
      headers: await csrfHeaders(page),
      data: {
        ir: {
          ir_version: 1,
          name,
          display_name: name,
          triggers: [{ kind: "interactive" }],
          nodes: [{ id: "rd", type: "storage.read", label: "経費を読む", params: { file: fileId } }],
          edges: [],
        },
      },
    });
    expect(created.ok(), await created.text()).toBeTruthy();
    id = ((await created.json()) as { id: string }).id;

    // 「読むファイル」の注記は、読める文字コードを言う。
    await page.goto(`/workflows/${id}`);
    await page.getByTestId("rf__node-rd").click();
    const panel = page.getByRole("complementary", { name: "ブロックの設定" });
    await expect(panel.getByText(HINT)).toBeVisible();
    await panel.getByTestId("storage-read-more").getByRole("button", { name: "詳しく" }).click();
    await expect(panel.getByText(ENCODING_MORE, { exact: true })).toBeVisible();
    await expect(panel.getByText(csvName)).toBeVisible({ timeout: 15_000 });
    if (SHOTS) await page.screenshot({ path: `${SHOTS}/storage-read-sjis-hint-pc.png` });

    const started = await page.request.post(`/api/workflows/${id}/runs`, {
      headers: await csrfHeaders(page),
      data: { input: {} },
    });
    expect(started.ok(), await started.text()).toBeTruthy();
    const runId = ((await started.json()) as { run_id: string }).run_id;
    const detail = async () =>
      (await (await page.request.get(`/api/workflows/${id}/runs/${runId}`)).json()) as RunDetail;
    await expect
      .poll(async () => (await detail()).status, { timeout: 60_000, intervals: [1_000] })
      .toMatch(/^(succeeded|failed)$/);
    const finished = await detail();
    expect(finished.status, JSON.stringify(finished.steps)).toBe("succeeded");
    const step = (await (await page.request.get(`/api/workflows/${id}/runs/${runId}/steps?path=rd`)).json()) as {
      output?: { text?: string; encoding?: string; name?: string };
    };
    expect(step.output?.text).toBe(EXPENSES);
    expect(step.output?.encoding).toBe("Shift_JIS");
    expect(step.output?.name).toBe(csvName);

    // 実行履歴の詳細で、ブロックの結果に読んだ中身と文字コードが出る（文字化けしない）。
    await page.goto(`/workflows/${id}/runs?run=${runId}`);
    const sheet = page.getByRole("dialog");
    await sheet.getByRole("button", { name: /経費を読む/ }).click();
    await expect(sheet.getByText(/"encoding": "Shift_JIS"/)).toBeVisible({ timeout: 20_000 });
    await expect(sheet.getByText(/交通費,1200/)).toBeVisible();
    await expect(sheet.getByText(/�/)).toHaveCount(0);
    if (SHOTS) await page.screenshot({ path: `${SHOTS}/storage-read-sjis-result-pc.png` });

    const phoneCtx = await browser.newContext({
      storageState: await page.context().storageState(),
      viewport: { width: 390, height: 844 },
      hasTouch: true,
      isMobile: true,
      deviceScaleFactor: 3,
      ignoreHTTPSErrors: true,
    });
    try {
      const phone = await phoneCtx.newPage();
      // スマホの一覧から「経費を読む」を開いても、同じ注記が横にはみ出さずに出る。
      await phone.goto(`/workflows/${id}`);
      const flow = phone.getByTestId("workflow-mobile-flow");
      await expect(flow).toBeVisible({ timeout: 15_000 });
      await flow.getByRole("button", { name: /経費を読む/ }).filter({ hasText: "経費を読む" }).first().click();
      const phoneSheet = phone.getByRole("dialog");
      await expect(phoneSheet.getByText(HINT)).toBeVisible();
      await expect(phoneSheet.getByText(csvName)).toBeVisible({ timeout: 15_000 });
      expect(await phoneSheet.evaluate((el) => el.scrollWidth <= el.clientWidth + 1)).toBe(true);
      if (SHOTS) await phone.screenshot({ path: `${SHOTS}/storage-read-sjis-hint-phone.png` });

      await phone.goto(`/workflows/${id}/runs?run=${runId}`);
      const runSheet = phone.getByRole("dialog");
      await runSheet.getByRole("button", { name: /経費を読む/ }).click();
      await expect(runSheet.getByText(/"encoding": "Shift_JIS"/)).toBeVisible({ timeout: 20_000 });
      expect(await phone.evaluate(() => document.documentElement.scrollWidth)).toBeLessThanOrEqual(390);
      if (SHOTS) await phone.screenshot({ path: `${SHOTS}/storage-read-sjis-result-phone.png` });
    } finally {
      await phoneCtx.close();
    }
  } finally {
    await cleanupAll(
      async () => {
        if (id) await removeWorkflowApi(page, id);
      },
      async () => {
        if (!fileId) return;
        const del = await page.request.delete(`/api/files/${fileId}`, { headers: await csrfHeaders(page) });
        expect(del.status(), await del.text()).toBeLessThan(300);
      },
      async () => {
        if (!folderId) return;
        const del = await page.request.delete(`/api/folders/${folderId}`, { headers: await csrfHeaders(page) });
        expect(del.status(), await del.text()).toBeLessThan(300);
      },
    );
  }
});
