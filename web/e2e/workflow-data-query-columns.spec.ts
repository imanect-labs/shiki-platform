import { expect, test, type Locator, type Page } from "@playwright/test";

import { cleanupAll, deleteTableApi, loginViaKeycloak, uniqueName } from "./helpers";

/// 「データを検索」（data.query）の探す条件と並べ替えで選べる列の e2e（BUG-244）:
/// - ワークフローの経路では索引を作らないので、選べるのは索引のある列・重複を許さない列・システムの列だけ。隠した列が
///   あるときは欄の下で理由と直し方を言い、利用者の列が 1 つも使えないテーブルでは使える列の名前を挙げる。
/// - 保存した後に列の索引を外した定義は、開いたときに保存時の誤りを欄の下と、その列の条件・並べ替えの行の下に出し、
///   行は選んだ列の名前のまま（空欄にしない）。使える列に選び直すと誤りが消えて保存できる。

const SHOTS = process.env.E2E_SHOTS_DIR;

async function csrfHeaders(page: Page): Promise<Record<string, string>> {
  const csrf = (await page.context().cookies()).find((c) => c.name === "shiki_csrf")?.value ?? "";
  return { "x-csrf-token": csrf, "content-type": "application/json" };
}

/// 列を選ぶ欄を開き、並んだ候補の名前を返して閉じる。
async function columnOptions(page: Page, combobox: Locator): Promise<string[]> {
  await combobox.click();
  const list = page.getByRole("listbox");
  await expect(list).toBeVisible();
  const names = (await list.getByRole("option").allTextContents()).map((s) => s.trim());
  await page.keyboard.press("Escape");
  await expect(list).toHaveCount(0);
  return names;
}

type Cmp = { cmp: { field: string; op: string; value?: unknown } };
type SavedIr = { nodes: { id: string; params: { query?: { filter?: Cmp; sort?: { field: string }[] } } }[] };

test("データを検索: 索引のある列と重複を許さない列だけを選べ、索引を外した列は保存時の誤りを欄と行の下に出す（BUG-244）", async ({
  page,
  browser,
}) => {
  test.setTimeout(240_000);
  await page.setViewportSize({ width: 1440, height: 900 });
  await loginViaKeycloak(page);
  const headers = await csrfHeaders(page);
  let tableId = "";
  let bareId = "";
  let id = "";
  try {
    // 件名は索引あり、コードは重複を許さない、金額とメモは索引なし。
    const made = await page.request.post("/api/data/tables", {
      headers,
      data: {
        name: uniqueName("e2e-経費の索引"),
        schema: {
          fields: [
            { key: "title", display_name: "件名", type: "text", indexed: true },
            { key: "code", display_name: "コード", type: "text", unique: true },
            { key: "amount", display_name: "金額", type: "number" },
            { key: "memo", display_name: "メモ", type: "text" },
          ],
        },
      },
    });
    expect(made.status(), await made.text()).toBe(201);
    tableId = ((await made.json()) as { id: string }).id;
    // 索引のある列も重複を許さない列も無いテーブル。
    const bare = await page.request.post("/api/data/tables", {
      headers,
      data: {
        name: uniqueName("e2e-索引の無い表"),
        schema: { fields: [{ key: "memo", display_name: "メモ", type: "text" }] },
      },
    });
    expect(bare.status(), await bare.text()).toBe(201);
    bareId = ((await bare.json()) as { id: string }).id;

    const name = uniqueName("query-columns");
    const flow = await page.request.post("/api/workflows", {
      headers,
      data: {
        ir: {
          ir_version: 1,
          name,
          display_name: name,
          triggers: [{ kind: "interactive" }],
          nodes: [
            {
              id: "q",
              type: "data.query",
              label: "申請を探す",
              params: {
                table: tableId,
                query: { filter: { cmp: { field: "title", op: "eq", value: "交通費" } }, sort: [{ field: "title" }] },
              },
            },
            { id: "q2", type: "data.query", label: "メモを探す", params: { table: bareId } },
          ],
          edges: [{ from: "q", to: "q2" }],
        },
      },
    });
    expect(flow.ok(), await flow.text()).toBeTruthy();
    id = ((await flow.json()) as { id: string }).id;

    await page.goto(`/workflows/${id}`);
    await page.getByTestId("rf__node-q").click();
    const panel = page.getByRole("complementary", { name: "ブロックの設定" });
    await expect(panel.getByText("探す条件と並べ替え（省略可）")).toBeVisible({ timeout: 20_000 });
    // 隠した列（金額・メモ）があるので、理由と直し方を欄の下で言う。
    const note = panel.getByTestId("query-columns-note");
    await expect(note).toHaveText(
      "絞り込み・並べ替えに使えるのは、索引のある列と重複を許さない列だけです。ほかの列を使うには、テーブルの「列の編集」でその列の索引をオンにしてください。",
    );
    await expect(panel.getByText(/索引の無い列なので/)).toHaveCount(0);

    // 探す条件: 足した条件の列の候補は、索引のある列・重複を許さない列・システムの列だけ。
    await panel.getByTestId("data-filter-button").click();
    const filterPop = page.getByRole("dialog").filter({ hasText: "すべての条件に合うレコード" });
    await expect(filterPop).toBeVisible();
    await expect(filterPop.getByRole("combobox", { name: "条件 1 の列" })).toHaveText("件名");
    await filterPop.getByRole("button", { name: "条件を追加" }).click();
    expect(await columnOptions(page, filterPop.getByRole("combobox", { name: "条件 2 の列" }))).toEqual([
      "件名",
      "コード",
      "更新日時",
      "作成日時",
      "作成者",
    ]);
    if (SHOTS) await page.screenshot({ path: `${SHOTS}/query-columns-filter-pc.png` });
    await page.keyboard.press("Escape");
    await expect(filterPop).toBeHidden();

    // 並べ替えも同じ。
    await panel.getByRole("button", { name: /^並べ替え/ }).click();
    const sortPop = page.getByRole("dialog").filter({ hasText: "並べ替えを追加" });
    await expect(sortPop).toBeVisible();
    expect(await columnOptions(page, sortPop.getByRole("combobox", { name: "並べ替え 1 の列" }))).toEqual([
      "件名",
      "コード",
      "更新日時",
      "作成日時",
      "作成者",
    ]);
    await page.keyboard.press("Escape");
    await expect(sortPop).toBeHidden();

    // 利用者の列が 1 つも使えないテーブルでは、使える列の名前を挙げる。
    await page.getByRole("button", { name: "パネルを閉じる" }).click();
    await page.getByTestId("rf__node-q2").click();
    await expect(panel.getByTestId("query-columns-note")).toHaveText(
      "このテーブルの列には索引が無いので、絞り込み・並べ替えに使えるのは「更新日時」「作成日時」「作成者」だけです。ほかの列を使うには、テーブルの「列の編集」でその列の索引をオンにしてください。",
      { timeout: 20_000 },
    );
    await panel.getByTestId("data-filter-button").click();
    await filterPop.getByRole("button", { name: "条件を追加" }).click();
    expect(await columnOptions(page, filterPop.getByRole("combobox", { name: "条件 1 の列" }))).toEqual([
      "更新日時",
      "作成日時",
      "作成者",
    ]);
    if (SHOTS) await page.screenshot({ path: `${SHOTS}/query-columns-bare-pc.png` });
    await page.keyboard.press("Escape");
    await expect(filterPop).toBeHidden();

    // 保存した後に件名の索引を外す（保存済みの定義はそのまま残る）。
    const table = (await (await page.request.get(`/api/data/tables/${tableId}`)).json()) as {
      schema: { fields: { key: string; indexed?: boolean }[] };
      schema_version: number;
    };
    const fields = table.schema.fields.map((f) => (f.key === "title" ? { ...f, indexed: false } : f));
    const put = await page.request.put(`/api/data/tables/${tableId}/schema`, {
      headers,
      data: { schema: { ...table.schema, fields }, expected_schema_version: table.schema_version },
    });
    expect(put.ok(), await put.text()).toBeTruthy();

    // 開いたときに、保存時の誤りが欄の下に出る（実行する前に分かる）。
    await page.goto(`/workflows/${id}`);
    await page.getByTestId("rf__node-q").click();
    const filterError = "「title」は索引の無い列なので、絞り込みの条件に使えません。";
    const sortError = "「title」は索引の無い列なので、並べ替えに使えません。";
    await expect(panel.getByText(filterError, { exact: false })).toBeVisible({ timeout: 20_000 });
    await expect(panel.getByText(sortError, { exact: false })).toBeVisible();
    if (SHOTS) await page.screenshot({ path: `${SHOTS}/query-columns-unindexed-pc.png` });

    // 探す条件の行は、選べない列でも名前のまま出し、その行の下に誤りを出す。使える列に選び直す。
    await panel.getByTestId("data-filter-button").click();
    await expect(filterPop.getByRole("combobox", { name: "条件 1 の列" })).toHaveText("件名");
    await expect(filterPop.getByTestId("filter-column-error")).toContainText(filterError);
    // 開くときのフェードが終わってから撮る。
    await expect.poll(() => filterPop.evaluate((el) => getComputedStyle(el).opacity)).toBe("1");
    if (SHOTS) await page.screenshot({ path: `${SHOTS}/query-columns-filter-row-error-pc.png` });
    await filterPop.getByRole("combobox", { name: "条件 1 の列" }).click();
    await page.getByRole("option", { name: "コード", exact: true }).click();
    await expect(filterPop.getByTestId("filter-column-error")).toHaveCount(0);
    await filterPop.getByRole("textbox", { name: "条件 1 の値" }).fill("A-1");
    await filterPop.getByRole("button", { name: "この条件で探す" }).click();
    await expect(filterPop).toBeHidden();

    // 並べ替えの行も同じ。
    await panel.getByRole("button", { name: /^並べ替え/ }).click();
    await expect(sortPop.getByRole("combobox", { name: "並べ替え 1 の列" })).toHaveText("件名");
    await expect(sortPop.getByTestId("sort-column-error")).toContainText(sortError);
    await expect.poll(() => sortPop.evaluate((el) => getComputedStyle(el).opacity)).toBe("1");
    if (SHOTS) await page.screenshot({ path: `${SHOTS}/query-columns-sort-row-error-pc.png` });
    await sortPop.getByRole("combobox", { name: "並べ替え 1 の列" }).click();
    await page.getByRole("option", { name: "コード", exact: true }).click();
    await expect(sortPop.getByRole("combobox", { name: "並べ替え 1 の列" })).toHaveText("コード");
    await page.keyboard.press("Escape");
    await expect(sortPop).toBeHidden();

    // 選び直すと、ライブの確かめで誤りが消え、保存できる。
    await expect(panel.getByText(/索引の無い列なので/)).toHaveCount(0, { timeout: 15_000 });
    await page.getByRole("button", { name: "保存", exact: true }).click();
    await expect(page.getByText(/保存しました/).first()).toBeVisible({ timeout: 15_000 });
    const saved = (await (await page.request.get(`/api/workflows/${id}`)).json()) as { ir: SavedIr };
    const query = saved.ir.nodes.find((n) => n.id === "q")?.params.query;
    expect(query?.filter).toEqual({ cmp: { field: "code", op: "eq", value: "A-1" } });
    expect(query?.sort?.map((k) => k.field)).toEqual(["code"]);

    // スマホ幅: ブロックのシートでも説明が出て、横にはみ出さない。
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
      const list = phone.getByTestId("workflow-mobile-flow");
      await expect(list).toBeVisible({ timeout: 20_000 });
      await list.getByRole("button", { name: /申請を探す/ }).filter({ hasText: "申請を探す" }).first().click();
      const sheet = phone.getByRole("dialog").filter({ hasText: "探す条件と並べ替え（省略可）" });
      // 手元の next dev は、初めて開くシートの部品をその場で作るので待つ。
      await expect(sheet).toBeVisible({ timeout: 20_000 });
      await expect(sheet.getByTestId("query-columns-note")).toBeVisible();
      await sheet.getByTestId("query-columns-note").scrollIntoViewIfNeeded();
      expect(await phone.evaluate(() => document.documentElement.scrollWidth)).toBeLessThanOrEqual(390);
      if (SHOTS) await phone.screenshot({ path: `${SHOTS}/query-columns-phone.png` });
      await sheet.getByTestId("data-filter-button").click();
      const phonePop = phone.getByRole("dialog").filter({ hasText: "すべての条件に合うレコード" });
      await expect(phonePop.getByRole("combobox", { name: "条件 1 の列" })).toHaveText("コード");
      expect(await phone.evaluate(() => document.documentElement.scrollWidth)).toBeLessThanOrEqual(390);
      if (SHOTS) await phone.screenshot({ path: `${SHOTS}/query-columns-filter-phone.png` });
    } finally {
      await phoneCtx.close();
    }
  } finally {
    await cleanupAll(
      async () => {
        if (!id) return;
        const del = await page.request.delete(`/api/artifacts/${id}`, { headers });
        expect(del.status(), await del.text()).toBeLessThan(300);
      },
      async () => {
        if (tableId) await deleteTableApi(page, tableId);
      },
      async () => {
        if (bareId) await deleteTableApi(page, bareId);
      },
    );
  }
});
