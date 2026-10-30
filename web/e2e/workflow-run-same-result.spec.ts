import { expect, test, type Page } from "@playwright/test";

import { cleanupAll, loginViaKeycloak, removeWorkflowApi, uniqueName } from "./helpers";

/// 実行履歴の詳細で、再開しても同じ結果になる失敗の案内（BUG-132）。
///
/// - 前のブロックの結果で変わりうる失敗（参照の解決・expr_resolve_error）は、エディターを開くリンクと一緒に
///   「同じ入力でもう一度実行」も残し、このボタンがこの実行のバージョンの設定で動くことを言う。
/// - この実行のバージョンの設定だけで決まる失敗（使ってよい操作の天井・知らないブロック）は、「同じ入力でもう一度
///   実行」もこの実行のバージョンで動いて同じ失敗をくり返すので出さない。出し分けはサーバーの失敗の分類の単一定義
///   （resume.offer_rerun）で決まる。保存の検証がそうした設定を通さないので、画面で起こせない。ここでは詳細の答えの
///   offer_rerun だけを差し替えて、画面の出し分けを確かめる（サーバーの決め方は Rust の failure.rs と outcome_it）。

const SHOTS = process.env.E2E_SHOTS_DIR;

async function csrfHeaders(page: Page): Promise<Record<string, string>> {
  const csrf = (await page.context().cookies()).find((c) => c.name === "shiki_csrf")?.value ?? "";
  return { "x-csrf-token": csrf, "content-type": "application/json" };
}

type RunDetail = {
  status: string;
  steps: { node_id: string; status: string; error?: { code?: string } | null }[];
  resume?: { advice: string; offer: boolean; offer_rerun: boolean; note: string } | null;
};

test("実行履歴: 再開しても同じ結果になる失敗は、エディターを開くよう案内し、同じ入力の実行はサーバーが勧めるときだけ出す（BUG-132）", async ({
  browser,
  page,
}) => {
  test.setTimeout(180_000);
  await page.setViewportSize({ width: 1440, height: 900 });
  await loginViaKeycloak(page);
  let id = "";
  try {
    // 入力に無い値を参照する（参照の解決の誤り）。
    const name = uniqueName("same-result-expr");
    const created = await page.request.post("/api/workflows", {
      headers: await csrfHeaders(page),
      data: {
        ir: {
          ir_version: 1,
          name,
          display_name: name,
          triggers: [{ kind: "interactive" }],
          nodes: [
            {
              id: "rd",
              type: "storage.read",
              label: "書類を読む",
              params: { file: { $from: "input", path: "/missing" } },
            },
          ],
          edges: [],
        },
      },
    });
    expect(created.ok(), await created.text()).toBeTruthy();
    id = ((await created.json()) as { id: string }).id;
    const started = await page.request.post(`/api/workflows/${id}/runs`, {
      headers: await csrfHeaders(page),
      data: { input: {} },
    });
    expect(started.ok(), await started.text()).toBeTruthy();
    const runId = ((await started.json()) as { run_id: string }).run_id;
    const detailUrl = `/api/workflows/${id}/runs/${runId}`;
    const get = async () => (await (await page.request.get(detailUrl)).json()) as RunDetail;
    await expect
      .poll(async () => (await get()).status, { timeout: 60_000, intervals: [1_000] })
      .toMatch(/^(succeeded|failed)$/);
    const detail = await get();
    expect(detail.status).toBe("failed");
    expect(detail.steps.find((s) => s.node_id === "rd")?.error?.code).toBe("expr_resolve_error");
    expect(detail.resume).toMatchObject({ advice: "same_result", offer: false, offer_rerun: true });

    // 前のブロックの結果で変わりうる失敗: エディターを開くリンクと「同じ入力でもう一度実行」の両方。
    await page.goto(`/workflows/${id}/runs?run=${runId}`);
    const sheet = page.getByRole("dialog");
    await expect(sheet.getByTestId("run-resume-note")).toHaveText(detail.resume!.note, { timeout: 30_000 });
    await expect(sheet.getByTestId("run-resume-note")).toContainText("この実行のバージョンの設定で動きます");
    await expect(sheet.getByTestId("run-resume-note")).toContainText("前の実行で済んだ書き込みや送信も、もう一度行われます");
    await expect(sheet.getByRole("button", { name: "失敗したところから再開" })).toHaveCount(0);
    await expect(sheet.getByRole("link", { name: "エディターを開く" })).toBeVisible();
    await expect(sheet.getByRole("button", { name: "同じ入力でもう一度実行" })).toBeVisible();
    if (SHOTS) await page.screenshot({ path: `${SHOTS}/run-same-result-data-pc.png` });

    // サーバーが同じ入力の実行を勧めない（この実行のバージョンの設定で決まる）なら、ボタンを出さない。
    const pinnedNote = "この実行のバージョンの設定で決まる失敗なので、（e2e の差し替え）";
    const pin = async (p: Page) =>
      p.route(`**${detailUrl}`, async (route) => {
        const res = await route.fetch();
        const body = (await res.json()) as RunDetail;
        body.resume = { ...body.resume!, offer_rerun: false, note: pinnedNote };
        await route.fulfill({ response: res, json: body });
      });
    await pin(page);
    await page.reload();
    await expect(sheet.getByTestId("run-resume-note")).toHaveText(pinnedNote, { timeout: 30_000 });
    await expect(sheet.getByRole("button", { name: "同じ入力でもう一度実行" })).toHaveCount(0);
    await expect(sheet.getByRole("button", { name: "失敗したところから再開" })).toHaveCount(0);
    const editor = sheet.getByRole("link", { name: "エディターを開く" });
    await expect(editor).toBeVisible();
    if (SHOTS) await page.screenshot({ path: `${SHOTS}/run-same-result-pinned-pc.png` });

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
      await pin(phone);
      await phone.goto(`/workflows/${id}/runs?run=${runId}`);
      const phoneSheet = phone.getByRole("dialog");
      await expect(phoneSheet.getByRole("link", { name: "エディターを開く" })).toBeVisible({ timeout: 30_000 });
      await expect(phoneSheet.getByRole("button", { name: "同じ入力でもう一度実行" })).toHaveCount(0);
      expect(await phone.evaluate(() => document.documentElement.scrollWidth)).toBeLessThanOrEqual(390);
      if (SHOTS) await phone.screenshot({ path: `${SHOTS}/run-same-result-pinned-phone.png` });
    } finally {
      // 詳細の画面は実行の詳細を読み直し続けるので、差し替えの途中の応答が閉じた画面に当たらないよう、閉じる前に
      // 差し替えを外す（途中のものは待たずに捨てる）。
      for (const p of phoneCtx.pages()) await p.unrouteAll({ behavior: "ignoreErrors" });
      await phoneCtx.close();
    }

    // リンクはエディターへ移り、そこに「実行」がある。エディターでは差し替えを使わないので、移る前に外す
    // （テストの終わりに、途中の差し替えが閉じた画面に当たって落ちない）。
    await page.unrouteAll({ behavior: "ignoreErrors" });
    await editor.click();
    await page.waitForURL(new RegExp(`/workflows/${id}$`), { timeout: 20_000 });
    await expect(page.getByRole("button", { name: "実行", exact: true })).toBeVisible({ timeout: 30_000 });
  } finally {
    await cleanupAll(() => (id ? removeWorkflowApi(page, id) : Promise.resolve()));
  }
});
