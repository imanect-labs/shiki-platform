import { expect, test, type Page } from "@playwright/test";

import { cleanupAll, loginViaKeycloak, uniqueName } from "./helpers";

/// 新しく保存する CSV の文字コードの e2e（GAP-47・D-120）:
/// - 「CSV を保存」と、CSV として保存する「ファイルを保存」は、「文字コード」で選んだ文字コード（UTF-8（BOM 付き）・
///   UTF-8・Shift_JIS）で書く。欄の無い保存済みの定義は、これまでどおり BOM の無い UTF-8 で書く。
/// - 欄の無い定義の設定の画面は「UTF-8」と出し、Excel で文字化けすることと選び直し方を言う。CSV でない種類の
///   「ファイルを保存」には欄を出さない。
/// - 新しく置いた「CSV を保存」は「UTF-8（BOM 付き）」から始まる。スマホの一覧から開いても同じ欄が出る。

const SHOTS = process.env.E2E_SHOTS_DIR;

async function csrfHeaders(page: Page): Promise<Record<string, string>> {
  const csrf = (await page.context().cookies()).find((c) => c.name === "shiki_csrf")?.value ?? "";
  return { "x-csrf-token": csrf, "content-type": "application/json" };
}

const CSV = "品目,金額\n交通費,1200\n";

test("CSV を保存: 選んだ文字コードで書き、欄の無い定義は BOM の無い UTF-8 のまま（GAP-47）", async ({ browser, page }) => {
  test.setTimeout(240_000);
  await page.setViewportSize({ width: 1440, height: 900 });
  await loginViaKeycloak(page);
  page.on("dialog", (d) => void d.accept());
  let folderId = "";
  const workflowIds: string[] = [];
  const fileIds = new Set<string>();
  try {
    const folder = await page.request.post("/api/folders", {
      headers: await csrfHeaders(page),
      data: { name: uniqueName("e2e-CSV の文字コード"), parent_id: null },
    });
    expect(folder.ok(), await folder.text()).toBeTruthy();
    folderId = ((await folder.json()) as { id: string }).id;

    const tag = uniqueName("enc");
    const node = (id: string, type: string, label: string, params: Record<string, unknown>) => ({
      id,
      type,
      label,
      params: { folder: folderId, content: CSV, ...params },
    });
    const created = await page.request.post("/api/workflows", {
      headers: await csrfHeaders(page),
      data: {
        ir: {
          ir_version: 1,
          name: tag,
          display_name: tag,
          triggers: [{ kind: "interactive" }],
          nodes: [
            node("legacy", "csv.write", "以前の発注書", { name: `以前-${tag}` }),
            node("bom", "csv.write", "Excel 用の発注書", { name: `bom-${tag}`, encoding: "utf8_bom" }),
            node("sjis", "csv.write", "古い仕組みへの発注書", { name: `sjis-${tag}`, encoding: "shift_jis" }),
            node("file_csv", "storage.write", "一覧を保存", { name: `一覧-${tag}.csv`, encoding: "utf8_bom" }),
            node("file_md", "storage.write", "メモを保存", { name: `メモ-${tag}.md`, encoding: "shift_jis" }),
          ],
          edges: [],
        },
      },
    });
    expect(created.ok(), await created.text()).toBeTruthy();
    const id = ((await created.json()) as { id: string }).id;
    workflowIds.push(id);

    const started = await page.request.post(`/api/workflows/${id}/runs`, {
      headers: await csrfHeaders(page),
      data: { input: {} },
    });
    expect(started.ok(), await started.text()).toBeTruthy();
    const runId = ((await started.json()) as { run_id: string }).run_id;
    await expect
      .poll(async () => ((await (await page.request.get(`/api/workflows/${id}/runs/${runId}`)).json()) as { status: string }).status, {
        timeout: 60_000,
        intervals: [1_000],
      })
      .toMatch(/^(succeeded|failed)$/);
    const saved = async (step: string) => {
      const res = await page.request.get(`/api/workflows/${id}/runs/${runId}/steps?path=${step}`);
      const out = ((await res.json()) as { output: { file_id: string } }).output;
      fileIds.add(out.file_id);
      // 文字コードはサーバーの判定（ドライブのテキストの API）で確かめる。
      return (await (await page.request.get(`/api/files/${out.file_id}/text`)).json()) as {
        text: string;
        encoding: string;
        bom: boolean;
      };
    };
    expect(await saved("legacy")).toMatchObject({ text: CSV, encoding: "UTF-8", bom: false });
    expect(await saved("bom")).toMatchObject({ text: CSV, encoding: "UTF-8", bom: true });
    expect(await saved("sjis")).toMatchObject({ text: CSV, encoding: "Shift_JIS", bom: false });
    expect(await saved("file_csv")).toMatchObject({ text: CSV, encoding: "UTF-8", bom: true });
    // CSV でない種類には使わない（UTF-8 のまま）。
    expect(await saved("file_md")).toMatchObject({ text: CSV, encoding: "UTF-8", bom: false });

    // 設定の画面: 欄の無い定義は「UTF-8」と出し、Excel で文字化けすることを言う。
    await page.goto(`/workflows/${id}`);
    const panel = page.getByRole("complementary", { name: "ブロックの設定" });
    await page.getByTestId("rf__node-legacy").click();
    await expect(panel.getByTestId("csv-encoding-select")).toHaveText("UTF-8");
    await expect(
      panel.getByText(
        "日本語の Windows の Excel で開くと文字化けします。Excel で開くなら「UTF-8（BOM 付き）」を選んでください。",
        { exact: true },
      ),
    ).toBeVisible();
    if (SHOTS) await panel.screenshot({ path: `${SHOTS}/csv-encoding-legacy-pc.png` });
    // CSV として保存する「ファイルを保存」には欄が出て、CSV でない種類には出さない。
    await page.getByTestId("rf__node-file_csv").click();
    await expect(panel.getByTestId("csv-encoding-select")).toHaveText("UTF-8（BOM 付き）");
    await page.getByTestId("rf__node-file_md").click();
    await expect(panel.getByRole("combobox", { name: "ファイルの種類" })).toBeVisible();
    await expect(panel.getByTestId("csv-encoding-select")).toHaveCount(0);

    // 新しく置いた「CSV を保存」は UTF-8（BOM 付き）から始まり、選び直すと注記が変わる。
    await page.goto("/workflows");
    await expect(page.getByText("読み込み中…")).toHaveCount(0);
    await page.getByRole("button", { name: "新しいワークフロー", exact: true }).click();
    await page.waitForURL(/\/workflows\/[0-9a-f-]+$/i, { timeout: 20_000 });
    workflowIds.push(page.url().split("/").pop() ?? "");
    await page.getByRole("button", { name: "最初のブロック" }).click();
    await page.getByRole("button", { name: /^CSV を保存/ }).click();
    await page.locator(".react-flow").getByText("CSV を保存").first().click();
    const select = panel.getByTestId("csv-encoding-select");
    await expect(select).toHaveText("UTF-8（BOM 付き）");
    await expect(panel.getByText("日本語の Windows の Excel でも、文字化けせずに開けます。", { exact: true })).toBeVisible();
    await select.scrollIntoViewIfNeeded();
    if (SHOTS) await page.screenshot({ path: `${SHOTS}/csv-encoding-new-pc.png` });
    await select.click();
    await page.getByRole("option", { name: "Shift_JIS" }).click();
    await expect(select).toHaveText("Shift_JIS");
    await expect(
      panel.getByText("Shift_JIS にない文字（絵文字など）が内容にあると、保存できずに失敗します。", { exact: true }),
    ).toBeVisible();
    if (SHOTS) await panel.screenshot({ path: `${SHOTS}/csv-encoding-sjis-pc.png` });

    // スマホの一覧から開いても同じ欄が出て、横にはみ出さない。
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
      await phone.goto(`/workflows/${id}`);
      const flow = phone.getByTestId("workflow-mobile-flow");
      await expect(flow).toBeVisible({ timeout: 15_000 });
      await flow.getByRole("button", { name: /Excel 用の発注書/ }).filter({ hasText: "Excel 用の発注書" }).first().click();
      const sheet = phone.getByRole("dialog");
      await expect(sheet.getByTestId("csv-encoding-select")).toHaveText("UTF-8（BOM 付き）");
      // 画面の下の端のボタンに重ならないよう、欄を真ん中まで送ってから確かめる。
      await sheet.getByTestId("csv-encoding-select").evaluate((el) => el.scrollIntoView({ block: "center" }));
      expect(await sheet.evaluate((el) => el.scrollWidth <= el.clientWidth + 1)).toBe(true);
      expect(await phone.evaluate(() => document.documentElement.scrollWidth)).toBeLessThanOrEqual(390);
      if (SHOTS) await phone.screenshot({ path: `${SHOTS}/csv-encoding-phone.png` });
    } finally {
      await phoneCtx.close();
    }
  } finally {
    await cleanupAll(
      ...workflowIds.map((wf) => async () => {
        if (!wf) return;
        const del = await page.request.delete(`/api/artifacts/${wf}`, { headers: await csrfHeaders(page) });
        expect(del.status(), await del.text()).toBeLessThan(300);
      }),
      ...[...fileIds].map((fileId) => async () => {
        const del = await page.request.delete(`/api/files/${fileId}`, { headers: await csrfHeaders(page) });
        expect(del.status(), await del.text()).toBeLessThan(300);
      }),
      async () => {
        if (!folderId) return;
        const del = await page.request.delete(`/api/folders/${folderId}`, { headers: await csrfHeaders(page) });
        expect(del.status(), await del.text()).toBeLessThan(300);
      },
    );
  }
});
