import { expect, test, type Page } from "@playwright/test";

import { cleanupAll, createFolderApi, loginViaKeycloak, removeWorkflowApi, trashFolderApi, uniqueName } from "./helpers";

/// 「フォルダ一覧」の件数の上限の e2e（BUG-180）:
/// - 以前は 100 件で黙って打ち切っていた。いまは結果に truncated（多いと true）が加わる。
/// - 設定の欄の下で、最大 100 件と truncated を言う（PC・スマホ）。参照の候補にも truncated が出る。
/// - 実行すると、結果に truncated: false が載る（上限より少ないフォルダー）。上限を超える場合は api の結合テスト
///   （storage_list_says_when_it_cuts_the_folder）で確かめる。
///
/// 作ったもの（フォルダー・ワークフロー）は finally で片付ける（互いに独立に）。

const SHOTS = process.env.E2E_SHOTS_DIR;
const HINT =
  "有効化のとき、このフォルダの閲覧権限を渡します。名前の順で最大 100 件を一覧にし、それより多いときは結果の truncated が true になります。";

async function csrfHeaders(page: Page): Promise<Record<string, string>> {
  const csrf = (await page.context().cookies()).find((c) => c.name === "shiki_csrf")?.value ?? "";
  return { "x-csrf-token": csrf, "content-type": "application/json" };
}

test("フォルダ一覧: 最大 100 件と truncated を欄の下で言い、結果に truncated が載る（BUG-180）", async ({ browser, page }) => {
  test.setTimeout(180_000);
  await page.setViewportSize({ width: 1440, height: 900 });
  await loginViaKeycloak(page);
  let folderId = "";
  let workflowId = "";
  try {
    folderId = await createFolderApi(page, uniqueName("e2e-フォルダ一覧"));
    const sub = await createFolderApi(page, "中のフォルダー", folderId);
    expect(sub).toBeTruthy();
    const tag = uniqueName("storage-list");
    const created = await page.request.post("/api/workflows", {
      headers: await csrfHeaders(page),
      data: {
        ir: {
          ir_version: 1,
          name: tag,
          display_name: tag,
          triggers: [{ kind: "interactive" }],
          nodes: [
            { id: "ls", type: "storage.list", label: "受付フォルダを見る", params: { folder: folderId } },
            {
              id: "count",
              type: "script.run",
              label: "件数を数える",
              params: { source: { inline: "function main(input) { return input; }" }, input: { $from: "nodes.ls.output", path: "/truncated" } },
            },
          ],
          edges: [{ from: "ls", to: "count" }],
        },
      },
    });
    expect(created.ok(), await created.text()).toBeTruthy();
    workflowId = ((await created.json()) as { id: string }).id;

    await page.goto(`/workflows/${workflowId}`);
    const panel = page.getByRole("complementary", { name: "ブロックの設定" });
    await page.getByTestId("rf__node-ls").click();
    await expect(panel.getByText(HINT, { exact: true })).toBeVisible();
    if (SHOTS) await panel.screenshot({ path: `${SHOTS}/storage-list-hint-pc.png` });
    // 後ろのブロックの参照に truncated を選べる（カタログの結果の項目から作る）。
    await page.getByTestId("rf__node-count").click();
    await expect(panel.getByText("「受付フォルダを見る」の truncated").first()).toBeVisible();

    // 実行すると、結果に truncated が載る（中のフォルダー 1 件だけなので false）。
    const started = await page.request.post(`/api/workflows/${workflowId}/runs`, {
      headers: await csrfHeaders(page),
      data: { input: {} },
    });
    expect(started.ok(), await started.text()).toBeTruthy();
    const runId = ((await started.json()) as { run_id: string }).run_id;
    await expect
      .poll(
        async () =>
          ((await (await page.request.get(`/api/workflows/${workflowId}/runs/${runId}`)).json()) as { status: string }).status,
        { timeout: 60_000, intervals: [1_000] },
      )
      .toBe("succeeded");
    const step = await page.request.get(`/api/workflows/${workflowId}/runs/${runId}/steps?path=ls`);
    const out = ((await step.json()) as { output: { items: { name: string; kind: string }[]; truncated: boolean } }).output;
    expect(out.truncated).toBe(false);
    expect(out.items.map((i) => [i.name, i.kind])).toEqual([["中のフォルダー", "folder"]]);

    // スマホの一覧から開いても、同じ案内が出て横にはみ出さない。
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
      await phone.goto(`/workflows/${workflowId}`);
      const flow = phone.getByTestId("workflow-mobile-flow");
      await expect(flow).toBeVisible({ timeout: 15_000 });
      await flow.getByRole("button", { name: /受付フォルダを見る/ }).filter({ hasText: "受付フォルダを見る" }).first().click();
      const sheet = phone.getByRole("dialog");
      const hint = sheet.getByText(HINT, { exact: true });
      await expect(hint).toBeVisible();
      await hint.evaluate((el) => el.scrollIntoView({ block: "center" }));
      expect(await sheet.evaluate((el) => el.scrollWidth <= el.clientWidth + 1)).toBe(true);
      if (SHOTS) await phone.screenshot({ path: `${SHOTS}/storage-list-hint-phone.png` });
    } finally {
      await phoneCtx.close();
    }
  } finally {
    await cleanupAll(
      async () => {
        if (workflowId) await removeWorkflowApi(page, workflowId);
      },
      async () => {
        if (folderId) await trashFolderApi(page, folderId);
      },
    );
  }
});
