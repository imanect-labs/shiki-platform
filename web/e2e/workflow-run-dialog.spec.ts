import { expect, test, type Page } from "@playwright/test";

import { cleanupAll, loginViaKeycloak, uniqueName } from "./helpers";

/// 手動の実行のダイアログの e2e（BUG-109）:
/// - 説明は決まった文（使ってよい操作でさらに絞る）で言う。
/// - 必須の欄が空なら送らずに欄の下で知らせる。サーバーの入力の誤りは、本文を捨てず欄の下に出す（以前は
///   「API 400」とだけ出た）。どちらでも実行は作られない。

const SHOTS = process.env.E2E_SHOTS_DIR;

async function csrfHeaders(page: Page): Promise<Record<string, string>> {
  const csrf = (await page.context().cookies()).find((c) => c.name === "shiki_csrf")?.value ?? "";
  return { "x-csrf-token": csrf, "content-type": "application/json" };
}

async function createFlow(page: Page, ir: Record<string, unknown>): Promise<string> {
  const res = await page.request.post("/api/workflows", { headers: await csrfHeaders(page), data: { ir } });
  expect(res.ok(), await res.text()).toBeTruthy();
  return ((await res.json()) as { id: string }).id;
}

async function runCount(page: Page, id: string): Promise<number> {
  const res = await page.request.get(`/api/workflows/${id}/runs`);
  expect(res.ok(), await res.text()).toBeTruthy();
  return ((await res.json()) as { items: unknown[] }).items.length;
}

test("実行のダイアログ: 決まった文で説明し、入力の誤りを欄の下に出す（BUG-109）", async ({ browser, page }) => {
  test.setTimeout(120_000);
  await page.setViewportSize({ width: 1440, height: 900 });
  await loginViaKeycloak(page);
  const name = uniqueName("run-dialog-input");
  const id = await createFlow(page, {
    ir_version: 1,
    name,
    display_name: name,
    input_schema: {
      type: "object",
      required: ["amount"],
      properties: {
        amount: { type: "integer", minimum: 1, title: "金額", description: "税込みの金額（円）" },
        memo: { type: "string", title: "メモ" },
      },
      "x-order": ["amount", "memo"],
    },
    triggers: [{ kind: "interactive" }],
    nodes: [{ id: "calc", type: "script.run", label: "計算する", params: { source: { inline: "function main(i){ return i; }" } } }],
    edges: [],
  });
  try {
    await page.goto(`/workflows/${id}`);
    await page.getByRole("button", { name: "実行", exact: true }).click();
    const dialog = page.getByRole("dialog", { name: "いま実行する" });
    await expect(
      dialog.getByText("あなたの権限で 1 回実行します。このワークフローが使ってよい操作のうち、あなたにできることだけを行います。"),
    ).toBeVisible();
    await expect(dialog.getByText("読める範囲・書ける範囲はあなたと同じ")).toHaveCount(0);
    // 項目の説明は欄の下に出す（placeholder にしない）。
    await expect(dialog.getByText("税込みの金額（円）")).toBeVisible();

    // 必須の欄が空: 送らずに欄の下で知らせる。
    await dialog.getByRole("button", { name: "実行する" }).click();
    await expect(dialog.getByText("「金額」を入れてください。")).toBeVisible();
    await expect(dialog.getByRole("textbox", { name: "金額" }).or(dialog.getByRole("spinbutton", { name: "金額" }))).toHaveAttribute(
      "aria-invalid",
      "true",
    );

    // サーバーの入力の誤り: 欄の下に理由を出す（「API 400」にしない）。
    await dialog.getByRole("spinbutton", { name: "金額" }).fill("0");
    await dialog.getByRole("button", { name: "実行する" }).click();
    await expect(dialog.getByText("1 以上にしてください。")).toBeVisible({ timeout: 15_000 });
    await expect(page.getByText(/API 400/)).toHaveCount(0);
    if (SHOTS) await page.waitForTimeout(400).then(() => page.screenshot({ path: `${SHOTS}/run-dialog-input-error-pc.png` }));
    expect(await runCount(page, id)).toBe(0);

    // スマホ幅でも、誤りが欄の下に収まる。
    const phoneCtx = await browser.newContext({
      storageState: await page.context().storageState(),
      viewport: { width: 390, height: 844 },
      hasTouch: true,
      isMobile: true,
      deviceScaleFactor: 3,
    });
    try {
      const phone = await phoneCtx.newPage();
      await phone.goto(`/workflows/${id}`);
      await phone.getByRole("button", { name: "実行", exact: true }).click();
      const sheet = phone.getByRole("dialog", { name: "いま実行する" });
      await sheet.getByRole("spinbutton", { name: "金額" }).fill("0");
      await sheet.getByRole("button", { name: "実行する" }).click();
      await expect(sheet.getByText("1 以上にしてください。")).toBeVisible({ timeout: 15_000 });
      expect(await sheet.evaluate((el) => el.scrollWidth <= el.clientWidth + 1)).toBe(true);
      if (SHOTS) await phone.waitForTimeout(400).then(() => phone.screenshot({ path: `${SHOTS}/run-dialog-input-error-phone.png` }));
    } finally {
      await phoneCtx.close();
    }
    expect(await runCount(page, id)).toBe(0);
  } finally {
    await cleanupAll(async () => {
      const del = await page.request.delete(`/api/artifacts/${id}`, { headers: await csrfHeaders(page) });
      expect(del.status(), await del.text()).toBeLessThan(300);
    });
  }
});
