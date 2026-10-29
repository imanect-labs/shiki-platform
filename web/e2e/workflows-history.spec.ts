import { expect, test, type Page } from "@playwright/test";

import { cleanupAll, loginViaKeycloak, removeWorkflowApi, uniqueName } from "./helpers";

/// 実行履歴 UI の e2e（Task 10.14 DoD）:
/// 失敗する run → 詳細シートで失敗ステップ強調＋エラー内容 → 状態フィルタ →
/// 「失敗したところから再開」→ ライブ更新（失敗 → 実行中/順番待ち → 再失敗・試行 2 回目）。

async function createFailingFlow(page: Page, name: string): Promise<string> {
  const cookies = await page.context().cookies();
  const csrf = cookies.find((c) => c.name === "shiki_csrf")?.value ?? "";
  const res = await page.request.post("/api/workflows", {
    headers: { "x-csrf-token": csrf, "content-type": "application/json" },
    data: {
      ir: {
        ir_version: 1,
        name,
        display_name: "失敗フロー",
        triggers: [{ kind: "interactive" }],
        nodes: [
          {
            id: "boom",
            type: "script.run",
            label: "爆発する",
            params: {
              source: { inline: "function main(){ throw new Error('boom-e2e'); }" },
            },
          },
        ],
        edges: [],
      },
    },
  });
  expect(res.ok()).toBeTruthy();
  const body = (await res.json()) as { id: string };
  return body.id;
}

test("実行履歴: 失敗詳細→フィルタ→失敗ステップから再開→ライブ更新", async ({ page }) => {
  test.setTimeout(240_000);
  await loginViaKeycloak(page);
  const id = await createFailingFlow(page, uniqueName("history-e2e"));
  try {
    // エディタから実行 → 実行履歴の deep-link へ。
    await page.goto(`/workflows/${id}`);
    await page.getByRole("button", { name: "実行", exact: true }).click();
    await page.getByRole("button", { name: "実行する" }).click();
    await page.waitForURL(/\/runs\?run=/, { timeout: 20_000 });

    // 詳細シート: SSE ライブ更新で「失敗」に到達し、失敗ステップにエラー内容が出る。
    const sheet = page.locator('[role="dialog"]');
    await expect(sheet.locator("h2").getByText("失敗")).toBeVisible({ timeout: 60_000 });
    await expect(sheet.getByText(/boom-e2e/).first()).toBeVisible();
    // 失敗の理由は、画面に返す結果と同じ分類の文言（サーバーが Rust の単一定義から組む・BUG-113）。
    await expect(sheet.getByTestId("run-failure")).toHaveText(
      "ワークフローの設定に問題があり、処理を完了できませんでした。作成者に連絡してください。",
    );
    // スクリプトの誤りは読むデータで変わりうるので、再開を出し、原因を直してから再開することを言う（BUG-132）。
    await expect(sheet.getByTestId("run-resume-note")).toContainText("失敗の原因を直してから再開すると");

    // 「失敗したところから再開」→ 失敗 step が即 ready に戻り再実行される。
    // 決定的に再失敗するため過渡状態（実行中）は数秒で消える。再実行の証跡は
    // step タイムラインの「試行 2 回目」バッジ（SSE ライブ更新で反映）で確認する。
    await sheet.getByRole("button", { name: "失敗したところから再開" }).click();
    // toast は aria-live 領域にも複製されるため first() で strict 違反を避ける。
    await expect(page.getByText(/失敗したところから再開しました/).first()).toBeVisible({
      timeout: 15_000,
    });
    await expect(sheet.getByText(/2 回目/)).toBeVisible({ timeout: 60_000 });
    await expect(sheet.locator("h2").getByText("失敗")).toBeVisible({ timeout: 60_000 });
    await sheet.getByRole("button", { name: "閉じる" }).click();

    // テーブルの状態フィルタ「失敗」で絞れる。
    await page.getByRole("button", { name: /^状態/ }).click();
    await page.getByRole("button", { name: "失敗" }).click();
    await page.keyboard.press("Escape");
    const table = page.locator("table");
    await expect(table.getByText("失敗").first()).toBeVisible({ timeout: 10_000 });

    // 行クリックで詳細シートが再び開く（?run= deep-link 復元）。
    await table.locator("tbody tr").first().click();
    await expect(page.locator('[role="dialog"]').getByText("実行の詳細")).toBeVisible();
  } finally {
    // 作ったワークフローを消す（BUG-126）。
    await cleanupAll(() => removeWorkflowApi(page, id));
  }
});

const SHOTS = process.env.E2E_SHOTS_DIR;

/// 承認の実行主体が失権したときに step に残る誤り（D-62・`relay_approval` の文言とコード）。
const ACTOR_UNAVAILABLE = {
  code: "actor_unavailable",
  message: "承認を待つ間に、ワークフローを実行する権限（委譲またはテーブルの権限）が外れたため、止めました",
};

/// 画面の中で `word` が占める行の数（語の途中で折り返すと 2 以上）。
async function linesOf(page: Page, word: string): Promise<number> {
  return page.evaluate((w) => {
    const walker = document.createTreeWalker(document.body, NodeFilter.SHOW_TEXT);
    for (let n = walker.nextNode(); n; n = walker.nextNode()) {
      const at = n.textContent?.indexOf(w) ?? -1;
      if (at < 0) continue;
      const range = document.createRange();
      range.setStart(n, at);
      range.setEnd(n, at + w.length);
      return new Set([...range.getClientRects()].map((r) => Math.round(r.top))).size;
    }
    return 0;
  }, word);
}

test("実行履歴: step の誤りのコードは、スマホでも語の途中で折り返さない（BUG-72）", async ({ page, browser }) => {
  test.setTimeout(180_000);
  await loginViaKeycloak(page);
  const id = await createFailingFlow(page, uniqueName("history-code"));
  const phoneCtx = await browser.newContext({
    storageState: await page.context().storageState(),
    viewport: { width: 390, height: 844 },
    hasTouch: true,
    isMobile: true,
    deviceScaleFactor: 2,
    ignoreHTTPSErrors: true,
  });
  try {
    const cookies = await page.context().cookies();
    const csrf = cookies.find((c) => c.name === "shiki_csrf")?.value ?? "";
    const started = await page.request.post(`/api/workflows/${id}/runs`, {
      headers: { "x-csrf-token": csrf, "content-type": "application/json" },
      data: { input: {} },
    });
    expect(started.ok(), await started.text()).toBeTruthy();
    const runId = ((await started.json()) as { run_id: string }).run_id;
    await expect
      .poll(
        async () =>
          ((await (await page.request.get(`/api/workflows/${id}/runs/${runId}`)).json()) as { status: string }).status,
        { timeout: 60_000, intervals: [1_000] },
      )
      .toBe("failed");

    // 失敗した step の誤りを、承認の実行主体が失権したときのもの（長いコード）にして描かせる（描画だけを見る）。
    const asActorUnavailable = async (p: Page) =>
      p.route(`**/api/workflows/${id}/runs/${runId}`, async (route) => {
        if (route.request().method() !== "GET") return route.continue();
        const res = await route.fetch();
        const body = (await res.json()) as { steps: { status: string; error: unknown }[] };
        for (const st of body.steps) if (st.status === "failed") st.error = ACTOR_UNAVAILABLE;
        await route.fulfill({ response: res, json: body });
      });

    const phone = await phoneCtx.newPage();
    await asActorUnavailable(phone);
    await phone.goto(`/workflows/${id}/runs?run=${runId}`);
    const sheet = phone.locator('[role="dialog"]');
    await expect(sheet.getByText(ACTOR_UNAVAILABLE.message)).toBeVisible({ timeout: 30_000 });
    // コードは語の途中で折り返さず（1 行に収まり）、省略もされない。シートも横にはみ出さない。
    expect(await linesOf(phone, ACTOR_UNAVAILABLE.code)).toBe(1);
    const code = sheet.getByTestId("step-error-code");
    await expect(code).toHaveText(`（${ACTOR_UNAVAILABLE.code}）`);
    expect(await code.evaluate((el) => el.scrollWidth <= el.clientWidth)).toBe(true);
    expect(await phone.evaluate(() => document.documentElement.scrollWidth)).toBeLessThanOrEqual(390);
    if (SHOTS) await phone.screenshot({ path: `${SHOTS}/bug72-step-error-phone.png` });

    // PC でも同じ。
    await page.setViewportSize({ width: 1440, height: 900 });
    await asActorUnavailable(page);
    await page.goto(`/workflows/${id}/runs?run=${runId}`);
    const pcSheet = page.locator('[role="dialog"]');
    await expect(pcSheet.getByText(ACTOR_UNAVAILABLE.message)).toBeVisible({ timeout: 30_000 });
    expect(await linesOf(page, ACTOR_UNAVAILABLE.code)).toBe(1);
    if (SHOTS) await page.screenshot({ path: `${SHOTS}/bug72-step-error-pc.png` });
  } finally {
    await page.unrouteAll({ behavior: "ignoreErrors" });
    await phoneCtx.close().catch(() => undefined);
    const cookies = await page.context().cookies();
    const csrf = cookies.find((c) => c.name === "shiki_csrf")?.value ?? "";
    const res = await page.request.delete(`/api/artifacts/${id}`, { headers: { "x-csrf-token": csrf } });
    expect([204, 404]).toContain(res.status());
  }
});
