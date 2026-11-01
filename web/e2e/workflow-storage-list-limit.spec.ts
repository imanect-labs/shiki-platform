import { expect, test, type Page } from "@playwright/test";

import { cleanupAll, createFolderApi, loginViaKeycloak, removeWorkflowApi, trashFolderApi, uniqueName } from "./helpers";

/// 「フォルダ一覧」の件数の上限と続きの位置の e2e（BUG-180・D-131）:
/// - 以前は 100 件で黙って打ち切っていた。いまは結果に truncated（続きがあると true）と next_cursor（続きの位置）が
///   加わる。
/// - 設定の欄の下で、最大 100 件を言い、「続きの位置（省略可）」の欄で前の結果の続きの位置を選べる（PC・スマホ）。
///   参照の候補にも truncated と next_cursor が出る。
/// - 実行すると、結果に truncated: false・next_cursor: null が載る（上限より少ないフォルダー）。上限を超えるフォルダー
///   は、1 つ目の結果の続きの位置を 2 つ目の「フォルダ一覧」に渡すと、残りを読む。読めない位置などは api の結合テスト
///   （storage_list_reads_on_from_the_cursor）で確かめる。
///
/// 作ったもの（フォルダー・ワークフロー）は finally で片付ける（互いに独立に）。

const SHOTS = process.env.E2E_SHOTS_DIR;
const HINT = "有効化のとき、このフォルダの閲覧権限を渡します。名前の順に最大 100 件を一覧にします。";
const CURSOR_HINT =
  "前の「フォルダ一覧」の結果の続きの位置（next_cursor）を選ぶと、その続きを一覧にします。空欄なら先頭から一覧にします。";

async function csrfHeaders(page: Page): Promise<Record<string, string>> {
  const csrf = (await page.context().cookies()).find((c) => c.name === "shiki_csrf")?.value ?? "";
  return { "x-csrf-token": csrf, "content-type": "application/json" };
}

test("フォルダ一覧: 最大 100 件と続きの位置の欄を出し、結果に truncated と next_cursor が載る（BUG-180・D-131）", async ({ browser, page }) => {
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
    const cursorField = panel.getByTestId("storage-list-cursor");
    await expect(cursorField.getByText("続きの位置（省略可）").first()).toBeVisible();
    await expect(cursorField.getByText(CURSOR_HINT, { exact: true })).toBeVisible();
    await cursorField.getByTestId("storage-list-cursor-more").getByRole("button", { name: "詳しく" }).click();
    await expect(cursorField.getByText("続きの位置は、同じフォルダーを一覧にした結果のものだけを使えます。", { exact: false })).toBeVisible();
    if (SHOTS) await panel.screenshot({ path: `${SHOTS}/storage-list-hint-pc.png` });
    // 後ろのブロックの参照に truncated と next_cursor を選べる（カタログの結果の項目から作る）。
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
    const out = ((await step.json()) as {
      output: { items: { name: string; kind: string }[]; truncated: boolean; next_cursor: string | null };
    }).output;
    expect(out.truncated).toBe(false);
    expect(out.next_cursor).toBeNull();
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
      const cursorHint = sheet.getByText(CURSOR_HINT, { exact: true });
      await expect(cursorHint).toBeVisible();
      await cursorHint.evaluate((el) => el.scrollIntoView({ block: "center" }));
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

test("フォルダ一覧: 1 つ目の結果の続きの位置を 2 つ目に渡すと、上限より後ろを読む（D-131）", async ({ browser, page }) => {
  test.setTimeout(240_000);
  await page.setViewportSize({ width: 1440, height: 900 });
  await loginViaKeycloak(page);
  let folderId = "";
  let workflowId = "";
  try {
    folderId = await createFolderApi(page, uniqueName("e2e-続きの位置"));
    // 上限（100 件）より 1 件多いフォルダー（中身はフォルダー・名前の順に並ぶ）。
    for (let i = 0; i <= 100; i += 1) await createFolderApi(page, `受付-${String(i).padStart(3, "0")}`, folderId);
    const tag = uniqueName("storage-list-cursor");
    const created = await page.request.post("/api/workflows", {
      headers: await csrfHeaders(page),
      data: {
        ir: {
          ir_version: 1,
          name: tag,
          display_name: tag,
          triggers: [{ kind: "interactive" }],
          nodes: [
            { id: "first", type: "storage.list", label: "最初の 100 件", params: { folder: folderId } },
            {
              id: "rest",
              type: "storage.list",
              label: "残り",
              params: { folder: folderId, cursor: { $from: "nodes.first.output", path: "/next_cursor" } },
            },
          ],
          edges: [{ from: "first", to: "rest" }],
        },
      },
    });
    expect(created.ok(), await created.text()).toBeTruthy();
    workflowId = ((await created.json()) as { id: string }).id;

    // エディターでは、2 つ目の「続きの位置」に 1 つ目の結果の続きの位置が入っている。
    await page.goto(`/workflows/${workflowId}`);
    const panel = page.getByRole("complementary", { name: "ブロックの設定" });
    await page.getByTestId("rf__node-rest").click();
    const cursorField = panel.getByTestId("storage-list-cursor");
    await expect(cursorField.getByText("「最初の 100 件」の next_cursor").first()).toBeVisible();
    if (SHOTS) await panel.screenshot({ path: `${SHOTS}/storage-list-cursor-pc.png` });

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
        { timeout: 90_000, intervals: [1_000] },
      )
      .toBe("succeeded");
    type Out = { items: { name: string }[]; truncated: boolean; next_cursor: string | null };
    const output = async (path: string) =>
      ((await (await page.request.get(`/api/workflows/${workflowId}/runs/${runId}/steps?path=${path}`)).json()) as { output: Out })
        .output;
    const first = await output("first");
    expect(first.items).toHaveLength(100);
    expect(first.truncated).toBe(true);
    expect(first.next_cursor).toBeTruthy();
    expect(first.next_cursor).not.toContain(folderId);
    const rest = await output("rest");
    expect(rest.items.map((i) => i.name)).toEqual(["受付-100"]);
    expect(rest.truncated).toBe(false);
    expect(rest.next_cursor).toBeNull();

    // スマホの一覧から開いても、続きの位置の欄が出て横にはみ出さない。
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
      await flow.getByRole("button", { name: /残り/ }).filter({ hasText: "残り" }).first().click();
      const sheet = phone.getByRole("dialog");
      const ref = sheet.getByText("「最初の 100 件」の next_cursor").first();
      await expect(ref).toBeVisible();
      await ref.evaluate((el) => el.scrollIntoView({ block: "center" }));
      expect(await sheet.evaluate((el) => el.scrollWidth <= el.clientWidth + 1)).toBe(true);
      if (SHOTS) await phone.screenshot({ path: `${SHOTS}/storage-list-cursor-phone.png` });
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
