import { expect, test, type Page } from "@playwright/test";

import { cleanupAll, loginViaKeycloak } from "./helpers";

/// 置いたばかりで必須の欄が空のブロックの e2e（D-83・付録 D の N7 Q3）:
/// - 条件で分ける の調べる値・値で振り分ける の値は、入力全体を黙って入れず、選んでいない空で置き、「入力や結果から選ぶ」
///   の候補を開いた状態で出す。外部 API 呼び出しの URL は空で置き、置いた直後に URL の誤りを出さない。
/// - 空のままでも保存はでき（注意つき）、実行はその理由（「{見出し}」が空です。…）を言って止まる。
/// 前提: shiki-server が SHIKI__WORKFLOW__ENABLED=true・LLM は stub（AI は呼ばない）。

const SHOTS = process.env.E2E_SHOTS_DIR;

async function csrfHeaders(page: Page): Promise<Record<string, string>> {
  const csrf = (await page.context().cookies()).find((c) => c.name === "shiki_csrf")?.value ?? "";
  return { "x-csrf-token": csrf, "content-type": "application/json" };
}

test("置いたばかりの必須の欄: 選んでいない空で置き、保存はでき、実行は理由を言って止まる（D-83）", async ({ browser, page }) => {
  test.setTimeout(180_000);
  await page.setViewportSize({ width: 1440, height: 900 });
  await loginViaKeycloak(page);
  page.on("dialog", (d) => void d.accept());
  let id = "";
  try {
    await page.goto("/workflows");
    await expect(page.getByText("読み込み中…")).toHaveCount(0);
    await page.getByRole("button", { name: "新しいワークフロー", exact: true }).click();
    await page.waitForURL(/\/workflows\/[0-9a-f-]+$/i, { timeout: 20_000 });
    id = new URL(page.url()).pathname.split("/").pop()!;
    const canvas = page.locator(".react-flow");
    const panel = page.getByRole("complementary", { name: "ブロックの設定" });

    // 条件分岐: 調べる値は「入力や結果から選ぶ」で開き、まだ何も選んでいない。
    await page.getByRole("button", { name: "最初のブロック" }).click();
    await page.getByRole("button", { name: /^条件分岐/ }).click();
    const left = panel.locator('[data-value-expr="調べる値"]');
    await expect(left.getByRole("combobox", { name: "値の入れ方" })).toHaveText("入力や結果から選ぶ");
    await expect(left.getByRole("combobox", { name: "参照先" })).toHaveText("参照先を選んでください");

    // 外部 API 呼び出し: URL は空で置き、置いた直後に URL の形の誤りを出さない。
    await canvas.getByLabel("次のブロックを追加").first().click();
    await page.getByRole("button", { name: /^外部 API 呼び出し/ }).click();
    await expect(panel.getByRole("textbox", { name: "URL", exact: true })).toHaveValue("");
    await expect(panel.getByText(/url が解析できません|url にホストがありません/)).toHaveCount(0);
    if (SHOTS) await page.screenshot({ path: `${SHOTS}/blank-required-pc.png` });

    // 保存はできる（注意つき）。保存した定義は空のまま。
    await page.getByRole("button", { name: "保存", exact: true }).click();
    await expect(page.getByText(/保存しました/).first()).toBeVisible({ timeout: 15_000 });
    const saved = (await (await page.request.get(`/api/workflows/${id}`)).json()) as {
      ir: { nodes: { type: string; params: Record<string, unknown> }[] };
    };
    const branch = saved.ir.nodes.find((n) => n.type === "control.branch");
    expect(branch?.params.condition).toEqual({ cmp: { left: "", op: "eq", right: "" } });
    expect(saved.ir.nodes.find((n) => n.type === "http.request")?.params.url).toBe("");

    // 実行は、空の欄を名指しして止まる（run は作らない）。
    await page.getByRole("button", { name: "実行", exact: true }).click();
    await page.getByRole("button", { name: "実行する" }).click();
    await expect(page.getByTestId("run-now-failure")).toContainText("実行できませんでした", { timeout: 15_000 });
    await expect(
      page.getByText("「調べる値」が空です。選ぶまで、実行も自動実行のオンもできません。").first(),
    ).toBeVisible();
    await expect(page.getByText("「URL」が空です。入れるまで、実行も自動実行のオンもできません。").first()).toBeVisible();
    if (SHOTS) await page.screenshot({ path: `${SHOTS}/blank-required-run-pc.png` });
    const runs = (await (await page.request.get(`/api/workflows/${id}/runs`)).json()) as { items: unknown[] };
    expect(runs.items).toHaveLength(0);

    // スマホ幅: 一覧から開いたシートでも、調べる値は「入力や結果から選ぶ」で開き、横にはみ出さない。
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
      await list.getByRole("button", { name: /条件分岐/ }).filter({ hasText: "条件分岐" }).first().click();
      const sheet = phone.getByRole("dialog").filter({ has: phone.locator('[data-value-expr="調べる値"]') });
      const phoneLeft = sheet.locator('[data-value-expr="調べる値"]');
      await expect(phoneLeft.getByRole("combobox", { name: "値の入れ方" })).toHaveText("入力や結果から選ぶ");
      expect(await sheet.evaluate((el) => el.scrollWidth <= el.clientWidth + 1)).toBe(true);
      if (SHOTS) await phone.screenshot({ path: `${SHOTS}/blank-required-phone.png` });
    } finally {
      await phoneCtx.close();
    }
  } finally {
    await cleanupAll(async () => {
      if (!id) return;
      const del = await page.request.delete(`/api/artifacts/${id}`, { headers: await csrfHeaders(page) });
      expect(del.status(), await del.text()).toBeLessThan(300);
    });
  }
});
