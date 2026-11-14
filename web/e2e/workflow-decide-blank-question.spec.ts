import { expect, test, type Page } from "@playwright/test";

import { cleanupAll, loginViaKeycloak, uniqueName } from "./helpers";

/// 置いたばかりの「AI で判定」は、「AI に尋ねること」を書く前でも保存できる（D-108・D-83 と同じ扱い）:
/// - 空の「AI に尋ねること」の下に「入力してください」を出す。
/// - 保存は通り、保存した定義の問いは空のまま。
/// - 実行は「「AI に尋ねること」が空です。…」と名指しして止まる（run は作らない・AI は呼ばない）。
/// 前提: shiki-server が SHIKI__WORKFLOW__ENABLED=true・LLM は stub。作ったワークフローは最後に消す。

const SHOTS = process.env.E2E_SHOTS_DIR;

async function csrfHeaders(page: Page): Promise<Record<string, string>> {
  const csrf = (await page.context().cookies()).find((c) => c.name === "shiki_csrf")?.value ?? "";
  return { "x-csrf-token": csrf, "content-type": "application/json" };
}

type SavedNode = { id: string; type: string; params: { questions?: { instructions: string }[] } };

test("置いたばかりの AI で判定は、AI に尋ねることを書く前に保存でき、実行は理由を言って止まる（D-108）", async ({
  browser,
  page,
}) => {
  test.setTimeout(180_000);
  await page.setViewportSize({ width: 1440, height: 900 });
  await loginViaKeycloak(page);
  let id = "";
  try {
    const name = uniqueName("wf-decide-blank");
    const res = await page.request.post("/api/workflows", {
      headers: await csrfHeaders(page),
      data: {
        ir: {
          ir_version: 1,
          name,
          display_name: name,
          triggers: [{ kind: "interactive" }],
          nodes: [],
          edges: [],
        },
      },
    });
    expect(res.status(), await res.text()).toBe(201);
    id = ((await res.json()) as { id: string }).id;

    await page.goto(`/workflows/${id}`);
    await page.getByRole("button", { name: "最初のブロック" }).click();
    await page.getByRole("button", { name: /^AI で判定/ }).click();
    const panel = page.getByRole("complementary", { name: "ブロックの設定" });
    const question = panel.getByRole("group", { name: /^質問 1/ });
    const ask = question.getByRole("textbox", { name: "AI に尋ねること" });
    await expect(ask).toHaveValue("");
    await expect(ask).toHaveAttribute("placeholder", "例: 問い合わせの種類はどれですか");
    await expect(question.getByText("入力してください")).toBeVisible();
    await question.getByText("入力してください").scrollIntoViewIfNeeded();
    if (SHOTS) await page.screenshot({ path: `${SHOTS}/decide-blank-question-pc.png` });

    // 保存は通る。保存した定義の問いは空のまま（勝手に埋めない）。
    await page.getByRole("button", { name: "保存", exact: true }).click();
    await expect(page.getByText(/保存しました/).first()).toBeVisible({ timeout: 15_000 });
    const saved = (await (await page.request.get(`/api/workflows/${id}`)).json()) as { ir: { nodes: SavedNode[] } };
    const decide = saved.ir.nodes.find((n) => n.type === "ai.decide");
    expect(decide?.params.questions?.[0]?.instructions).toBe("");

    // 実行は、空の欄を名指しして止まる（run は作らない）。
    await page.getByRole("button", { name: "実行", exact: true }).click();
    await page.getByRole("button", { name: "実行する" }).click();
    await expect(page.getByTestId("run-now-failure")).toContainText("実行できませんでした", { timeout: 15_000 });
    await expect(
      page.getByText("「AI に尋ねること」が空です。入れるまで、実行も自動実行のオンもできません。").first(),
    ).toBeVisible();
    if (SHOTS) await page.screenshot({ path: `${SHOTS}/decide-blank-question-run-pc.png` });
    const runs = (await (await page.request.get(`/api/workflows/${id}/runs`)).json()) as { items: unknown[] };
    expect(runs.items).toHaveLength(0);

    // スマホ幅: 一覧から開いたシートでも同じ欄と知らせが出て、横にはみ出さない。
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
      await list.getByRole("button", { name: /AI で判定/ }).filter({ hasText: "AI で判定" }).first().click();
      const sheet = phone.getByRole("dialog").filter({ has: phone.getByRole("group", { name: /^質問 1/ }) });
      const phoneQuestion = sheet.getByRole("group", { name: /^質問 1/ });
      const phoneAsk = phoneQuestion.getByRole("textbox", { name: "AI に尋ねること" });
      await expect(phoneAsk).toHaveValue("");
      await expect(phoneQuestion.getByText("入力してください")).toBeVisible();
      await phoneQuestion.getByText("入力してください").scrollIntoViewIfNeeded();
      expect(await sheet.evaluate((el) => el.scrollWidth <= el.clientWidth + 1)).toBe(true);
      if (SHOTS) await phone.screenshot({ path: `${SHOTS}/decide-blank-question-phone.png` });
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
