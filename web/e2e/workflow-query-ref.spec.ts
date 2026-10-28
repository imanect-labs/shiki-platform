import { expect, test, type Page } from "@playwright/test";

import { cleanupAll, loginViaKeycloak, uniqueName } from "./helpers";

/// 「データを調べる」の探す条件に、入力や結果・実行情報の値（`$from`）を使う e2e（BUG-98・D-101）:
/// - 参照を含む保存済みの条件を開いても「[object Object]」と出さず、「入力や結果から選ぶ」の行として選んだ値の名前で
///   出す。ほかの条件を直して「この条件で探す」を押しても、参照は黙って消えずにそのまま保存される。
/// - 新しい条件の値を「入力や結果から選ぶ」で、実行情報の「実行した人」から選べる。
/// - 手動で実行すると、実行した人（ログインしている本人）の行が見つかる（申請者の上長を引く組み方・GAP-17）。
/// 前提: shiki-server が SHIKI__WORKFLOW__ENABLED=true・LLM は stub（AI は呼ばない）。

const SHOTS = process.env.E2E_SHOTS_DIR;

async function csrfHeaders(page: Page): Promise<Record<string, string>> {
  const csrf = (await page.context().cookies()).find((c) => c.name === "shiki_csrf")?.value ?? "";
  return { "x-csrf-token": csrf, "content-type": "application/json" };
}

type Cmp = { cmp: { field: string; op: string; value?: unknown } };
type SavedIr = { nodes: { id: string; params: { query?: { filter?: { all?: Cmp[] } & Partial<Cmp> } } }[] };

test("データを調べる: 実行した人を使う探す条件を壊さずに出して直せ、手動の実行で本人の行が見つかる（BUG-98・D-101）", async ({
  browser,
  page,
}) => {
  test.setTimeout(240_000);
  await page.setViewportSize({ width: 1440, height: 900 });
  await loginViaKeycloak(page);
  page.on("dialog", (d) => void d.accept());
  const headers = await csrfHeaders(page);
  const me = (await (await page.request.get("/api/me")).json()) as { id: string };
  let tableId = "";
  let id = "";
  try {
    const created = await page.request.post("/api/data/tables", {
      headers,
      data: {
        name: uniqueName("e2e-承認者表"),
        schema: {
          fields: [
            { key: "member", display_name: "メンバー", type: "user_ref", indexed: true },
            { key: "manager", display_name: "上長", type: "user_ref", indexed: true },
            { key: "note", display_name: "メモ", type: "text", indexed: true },
          ],
        },
      },
    });
    expect(created.status(), await created.text()).toBe(201);
    tableId = ((await created.json()) as { id: string }).id;
    const row = await page.request.post(`/api/data/tables/${tableId}/records`, {
      headers,
      data: { data: { member: me.id, manager: me.id, note: "本人" } },
    });
    expect(row.status(), await row.text()).toBe(201);

    // 探す条件: メンバー = 実行した人（参照）かつ メモ = 「ほか」（直す前は当たらない）。
    const byPerson = { $from: "run", path: "/started_by" };
    const name = uniqueName("query-ref");
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
              label: "上長を探す",
              params: {
                table: tableId,
                query: {
                  filter: {
                    all: [
                      { cmp: { field: "member", op: "eq", value: byPerson } },
                      { cmp: { field: "note", op: "eq", value: "ほか" } },
                    ],
                  },
                },
              },
            },
          ],
          edges: [],
        },
      },
    });
    expect(flow.ok(), await flow.text()).toBeTruthy();
    id = ((await flow.json()) as { id: string }).id;

    await page.goto(`/workflows/${id}`);
    await page.getByTestId("rf__node-q").click();
    const panel = page.getByRole("complementary", { name: "ブロックの設定" });
    await expect(panel.getByText("探す条件と並べ替え（省略可）")).toBeVisible({ timeout: 15_000 });
    await panel.getByTestId("data-filter-button").click();
    const pop = page.getByRole("dialog").filter({ hasText: "すべての条件に合うレコード" });
    await expect(pop).toBeVisible();
    // 参照の値は文字に直さず、選んだ値の名前で出す。
    await expect(pop).not.toContainText("[object Object]");
    const mode1 = pop.getByRole("radiogroup", { name: "条件 1 の値の入れ方" });
    await expect(mode1.getByRole("radio", { name: "入力や結果から選ぶ" })).toHaveAttribute("aria-checked", "true");
    await expect(pop.getByRole("combobox", { name: "条件 1 の値" })).toHaveText("実行した人（started_by）");
    await expect(pop.getByRole("textbox", { name: "条件 2 の値" })).toHaveValue("ほか");

    // ほかの条件を直し、新しい条件の値を実行情報の「実行した人」から選ぶ。
    await pop.getByRole("textbox", { name: "条件 2 の値" }).fill("本人");
    await pop.getByRole("button", { name: "条件を追加" }).click();
    await pop.getByRole("combobox", { name: "条件 3 の列" }).click();
    await page.getByRole("option", { name: "上長", exact: true }).click();
    const mode3 = pop.getByRole("radiogroup", { name: "条件 3 の値の入れ方" });
    await mode3.getByRole("radio", { name: "入力や結果から選ぶ" }).click();
    // 値を選ぶまでは適用できない（黙って捨てない）。
    await expect(pop.getByText("値を選んでください")).toBeVisible();
    await expect(pop.getByRole("button", { name: "この条件で探す" })).toBeDisabled();
    await pop.getByRole("combobox", { name: "条件 3 の値" }).click();
    await expect(page.getByText("実行情報（日時は日本時間）")).toBeVisible();
    await page.getByRole("option", { name: "実行した人（started_by）" }).click();
    await expect(page.getByRole("listbox")).toHaveCount(0);
    if (SHOTS) await page.screenshot({ path: `${SHOTS}/query-ref-filter-pc.png` });
    await pop.getByRole("button", { name: "この条件で探す" }).click();
    await expect(pop).toBeHidden();

    await page.getByRole("button", { name: "保存", exact: true }).click();
    await expect(page.getByText(/保存しました/).first()).toBeVisible({ timeout: 15_000 });
    const saved = (await (await page.request.get(`/api/workflows/${id}`)).json()) as { ir: SavedIr };
    const all = saved.ir.nodes.find((n) => n.id === "q")?.params.query?.filter?.all ?? [];
    expect(all).toEqual([
      { cmp: { field: "member", op: "eq", value: byPerson } },
      { cmp: { field: "note", op: "eq", value: "本人" } },
      { cmp: { field: "manager", op: "eq", value: byPerson } },
    ]);

    // 手動で実行すると、実行した人（本人）の行が見つかる。入力に別の人を書いても名乗れない。
    const start = await page.request.post(`/api/workflows/${id}/runs`, {
      headers,
      data: { input: { started_by: "someone-else" } },
    });
    expect(start.ok(), await start.text()).toBeTruthy();
    const runId = ((await start.json()) as { run_id: string }).run_id;
    await expect
      .poll(
        async () => {
          const run = (await (await page.request.get(`/api/workflows/${id}/runs/${runId}`)).json()) as {
            status: string;
          };
          return run.status;
        },
        { timeout: 60_000, intervals: [1_000] },
      )
      .toBe("succeeded");
    const step = (await (await page.request.get(`/api/workflows/${id}/runs/${runId}/steps?path=q`)).json()) as {
      output: { items: { data: { member: string; manager: string } }[] };
    };
    expect(step.output.items).toHaveLength(1);
    expect(step.output.items[0].data).toMatchObject({ member: me.id, manager: me.id });

    // スマホ幅: ブロックのシートから開いた探す条件も、参照の行を名前で出し、横にはみ出さない。
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
      await expect(list).toBeVisible({ timeout: 15_000 });
      await list.getByRole("button", { name: /上長を探す/ }).filter({ hasText: "上長を探す" }).first().click();
      const sheet = phone.getByRole("dialog").filter({ hasText: "探す条件と並べ替え（省略可）" });
      await expect(sheet).toBeVisible();
      await sheet.getByTestId("data-filter-button").click();
      const phonePop = phone.getByRole("dialog").filter({ hasText: "すべての条件に合うレコード" });
      await expect(phonePop.getByRole("combobox", { name: "条件 1 の値" })).toHaveText("実行した人（started_by）");
      await expect(phonePop).not.toContainText("[object Object]");
      expect(await phone.evaluate(() => document.documentElement.scrollWidth)).toBeLessThanOrEqual(390);
      // 条件が多くても、適用のボタンまで送って押せる（画面の外に出ない）。
      await phonePop.getByRole("button", { name: "この条件で探す" }).scrollIntoViewIfNeeded();
      await expect(phonePop.getByRole("button", { name: "この条件で探す" })).toBeInViewport();
      if (SHOTS) await phone.screenshot({ path: `${SHOTS}/query-ref-filter-phone.png` });
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
        if (!tableId) return;
        const del = await page.request.delete(`/api/data/tables/${tableId}`, { headers });
        expect(del.status(), await del.text()).toBeLessThan(300);
      },
    );
  }
});
