import { expect, test, type Page } from "@playwright/test";

import { cleanupAll, loginViaKeycloak, uniqueName } from "./helpers";

/// 「外部 API 呼び出し」の「相手が誤りを返したら失敗にする」の e2e（BUG-99・D-95）:
/// - 欄の無い既存の定義はオフのまま（相手の誤りも成功として次へ渡す）で、そのことを注記で言う。
/// - 新しく置いたブロックはオンから始まり、オンのときは失敗の扱い（やり直すかどうか）を「詳しく」で言う。
/// - URL の「詳しく」は、送れる先が管理者の許可とシークレットの送り先で決まることを言う（実装どおり）。
/// 実行の振る舞い（4xx はやり直さない・5xx と 429 はやり直す）は Rust の結合テスト
/// （workflow_nodes_it の http_error_statuses_through_the_production_port）で確かめる。

const SHOTS = process.env.E2E_SHOTS_DIR;

async function csrfHeaders(page: Page): Promise<Record<string, string>> {
  const csrf = (await page.context().cookies()).find((c) => c.name === "shiki_csrf")?.value ?? "";
  return { "x-csrf-token": csrf, "content-type": "application/json" };
}

const OFF_NOTE =
  "相手が誤り（400 以上）を返しても、このブロックは失敗になりません。結果の応答のステータスコード（status）で分けてください。";
const ON_NOTE = "相手が誤り（400 以上）を返すと、このブロックは失敗し、「失敗したとき」の設定に従います。";

test("外部 API 呼び出し: 相手の誤りを失敗にする選択は、既存の定義はオフ・新しいブロックはオン（BUG-99）", async ({
  browser,
  page,
}) => {
  test.setTimeout(180_000);
  await page.setViewportSize({ width: 1440, height: 900 });
  await loginViaKeycloak(page);
  // 保存していない変更を残して画面を移るときの確認（beforeunload）は、移る側を選ぶ。
  page.on("dialog", (d) => void d.accept());
  const ids: string[] = [];
  let id = "";
  try {
    const name = uniqueName("http-status");
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
              id: "hook",
              type: "http.request",
              label: "通知を送る",
              params: { method: "POST", url: "https://hooks.example.com/notify" },
            },
          ],
          edges: [],
        },
      },
    });
    expect(created.ok(), await created.text()).toBeTruthy();
    id = ((await created.json()) as { id: string }).id;
    ids.push(id);

    await page.goto(`/workflows/${id}`);
    await page.getByTestId("rf__node-hook").click();
    const panel = page.getByRole("complementary", { name: "ブロックの設定" });
    const toggle = panel.getByRole("switch", { name: "相手が誤りを返したら失敗にする" });
    await expect(toggle).toHaveAttribute("aria-checked", "false");
    await expect(panel.getByTestId("http-fail-on-error-note")).toHaveText(OFF_NOTE);
    await expect(panel.getByText("送れる先は、管理者とシークレットの設定で決まります。", { exact: false })).toBeVisible();
    // URL の「詳しく」: シークレットを使わない送信は、管理者が許可した送り先だけ。
    const rule = panel.getByTestId("http-destination-rule");
    await rule.getByRole("button", { name: "詳しく" }).click();
    await expect(
      rule.getByText("管理者が送り先を許可していなければ、シークレットを使わない送信はできません。", { exact: false }),
    ).toBeVisible();
    await expect(panel.getByText(/重複を見分ける印（Idempotency-Key）を付けて送ります。/)).toBeVisible();
    if (SHOTS) await panel.screenshot({ path: `${SHOTS}/http-status-legacy-pc.png` });

    // オンにすると注記が変わり、やり直しの扱いを「詳しく」で言う。
    await toggle.click();
    await expect(toggle).toHaveAttribute("aria-checked", "true");
    await expect(panel.getByTestId("http-fail-on-error-note")).toHaveText(ON_NOTE);
    await toggle.click();
    await expect(panel.getByTestId("http-fail-on-error-note")).toHaveText(OFF_NOTE);


    // 新しく置いたブロックはオンから始まる（新しいワークフローの最初のブロックとして置く）。
    await page.goto("/workflows");
    await expect(page.getByText("読み込み中…")).toHaveCount(0);
    await page.getByRole("button", { name: "新しいワークフロー", exact: true }).click();
    await page.waitForURL(/\/workflows\/[0-9a-f-]+$/i, { timeout: 20_000 });
    ids.push(page.url().split("/").pop() ?? "");
    await page.getByRole("button", { name: "最初のブロック" }).click();
    await page.getByRole("button", { name: /^外部 API 呼び出し/ }).click();
    const canvas = page.locator(".react-flow");
    await canvas.getByText("外部 API 呼び出し").first().click();
    await expect(toggle).toHaveAttribute("aria-checked", "true");
    await expect(panel.getByTestId("http-fail-on-error-note")).toHaveText(ON_NOTE);
    await panel.getByRole("button", { name: "詳しく" }).last().click();
    await expect(panel.getByText(/「試す回数」が 2 以上ならやり直します。/)).toBeVisible();
    if (SHOTS) await panel.screenshot({ path: `${SHOTS}/http-status-new-pc.png` });

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
      await flow.getByRole("button", { name: /通知を送る/ }).filter({ hasText: "通知を送る" }).first().click();
      const sheet = phone.getByRole("dialog");
      await expect(sheet.getByTestId("http-fail-on-error-note")).toHaveText(OFF_NOTE);
      await sheet.getByTestId("http-fail-on-error-note").scrollIntoViewIfNeeded();
      expect(await sheet.evaluate((el) => el.scrollWidth <= el.clientWidth + 1)).toBe(true);
      if (SHOTS) await phone.screenshot({ path: `${SHOTS}/http-status-phone.png` });
    } finally {
      await phoneCtx.close();
    }
  } finally {
    await cleanupAll(
      ...ids.map((wf) => async () => {
        const del = await page.request.delete(`/api/artifacts/${wf}`, { headers: await csrfHeaders(page) });
        expect(del.status(), await del.text()).toBeLessThan(300);
      }),
    );
  }
});
