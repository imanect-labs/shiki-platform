import { expect, test, type Page } from "@playwright/test";

import { cleanupQueue, loginViaKeycloak, removeWorkflowApi, uniqueName } from "./helpers";

/// ワークフローのエディターの上のバー（PC）の e2e（BUG-231）:
/// - バーは、ビューポートでなくサイドバーを除いたバーの幅で操作を畳み、タイトルと保存の状態をボタンの下に隠さない。
/// - 1440px と 1280px（サイドバーを開いた既定）: 畳まない（実行履歴・バージョン履歴・共有・自動実行のボタンと、自動実行が
///   有効なことのバッジを出す）。
/// - 1024px でサイドバーを開いている: 実行履歴・バージョン履歴・共有・自動実行を ⋯ に畳み、有効なだけのバッジは出さない。
/// - 800px でサイドバーを開いている: 元に戻す・やり直すも ⋯ に畳み、保存の状態をタイトルの下に置く。
/// どの幅でも、タイトルと保存の状態は欄の中に収まり、ボタンと重ならない。スマホ（390px）は今までどおり右上の ⋯ に
/// 畳む。作ったワークフローは最後に消す。
/// `E2E_SHOTS_DIR` を渡すとスクリーンショットを撮る。

const SHOTS = process.env.E2E_SHOTS_DIR;

const cleanup = cleanupQueue();
test.afterEach(() => cleanup.run());

async function csrfHeaders(page: Page): Promise<Record<string, string>> {
  const csrf = (await page.context().cookies()).find((c) => c.name === "shiki_csrf")?.value ?? "";
  return { "x-csrf-token": csrf, "content-type": "application/json" };
}

/// タイトルと保存の状態が欄の中に見えていて、右の操作と重ならない。
async function expectTitleClear(page: Page, name: string): Promise<{ stacked: boolean }> {
  const area = page.getByTestId("chrome-bar-title");
  const actions = page.getByTestId("chrome-bar-actions");
  const title = area.getByText(name);
  const saved = area.getByText("v1・保存済み");
  await expect(title).toBeVisible();
  await expect(saved).toBeVisible();
  const [a, b, t, s] = await Promise.all([area.boundingBox(), actions.boundingBox(), title.boundingBox(), saved.boundingBox()]);
  if (!a || !b || !t || !s) throw new Error("バーの欄が描かれていない");
  expect(a.x + a.width, "タイトルの欄は右の操作より左で終わる").toBeLessThanOrEqual(b.x + 1);
  expect(s.x + s.width, "保存の状態は欄の中に収まる（切られていない）").toBeLessThanOrEqual(a.x + a.width + 1);
  expect(t.width, "タイトルが読める幅で出る").toBeGreaterThan(40);
  return { stacked: s.y > t.y + 4 };
}

test("ワークフローのエディターの上のバーは、狭いと操作を ⋯ に畳み、タイトルと保存の状態を隠さない（BUG-231）", async ({
  page,
  browser,
}) => {
  test.setTimeout(150_000);
  await page.setViewportSize({ width: 1440, height: 900 });
  await loginViaKeycloak(page);
  const slug = uniqueName("narrow-bar");
  // バーに出すのは表示名（日本語の長めの名前で、切れても読めるかを見る）。
  const name = `週ごとの経費の集計 ${slug.slice(-4)}`;
  const res = await page.request.post("/api/workflows", {
    headers: await csrfHeaders(page),
    data: {
      ir: {
        ir_version: 1,
        name: slug,
        display_name: name,
        // 自動実行を有効にしてバッジも出す（発火は月曜 9:00。テストの最後にワークフローごと消す）。
        triggers: [{ kind: "schedule", cron: "0 9 * * 1", tz: "Asia/Tokyo", catchup: "skip" }],
        nodes: [{ id: "day", type: "transform.date", label: "予定の日", params: {} }],
        edges: [],
      },
    },
  });
  expect(res.ok(), await res.text()).toBeTruthy();
  const id = ((await res.json()) as { id: string }).id;
  cleanup.add(() => removeWorkflowApi(page, id));
  const bar = page.getByTestId("chrome-bar");
  const more = page.getByTestId("workflow-bar-more");
  const narrowMore = page.getByTestId("workflow-bar-more-narrow");

  // --- 1440px: 畳まない。
  await page.goto(`/workflows/${id}`);
  await expect(bar.getByRole("button", { name: "保存" })).toBeVisible({ timeout: 30_000 });
  await expectTitleClear(page, name);
  await expect(bar.getByText("ブロック", { exact: true })).toBeVisible();
  await expect(bar.getByTestId("workflow-share")).toBeVisible();
  await expect(bar.getByRole("button", { name: "自動実行を有効化" })).toBeVisible();
  await expect(more).toBeHidden();
  await expect(narrowMore).toBeHidden();
  if (SHOTS) await page.screenshot({ path: `${SHOTS}/narrow-bar-1440.png` });

  // 自動実行をオンにする（いちばん幅を取るバッジとボタンが並ぶ）。
  await bar.getByRole("button", { name: "自動実行を有効化" }).click();
  const consent = page.getByRole("dialog");
  await consent.getByRole("button", { name: "同意してオンにする" }).click();
  await expect(bar.getByText("自動実行 有効（v1）")).toBeVisible({ timeout: 15_000 });
  await expect(consent).toHaveCount(0);
  await expectTitleClear(page, name);

  // --- 1280px（ほかの e2e の既定の幅）: ブロックと入力項目は名前を隠すが、ほかは畳まない。
  await page.setViewportSize({ width: 1280, height: 800 });
  const at1280 = await expectTitleClear(page, name);
  expect(at1280.stacked).toBe(false);
  await expect(bar.getByTestId("workflow-share")).toBeVisible();
  await expect(bar.getByRole("button", { name: "自動実行の設定" })).toBeVisible();
  // 有効なバッジは、隣の「自動実行の設定」と重なる語を省いて「有効（v1）」と見せる（読み上げは省かない）。
  await expect(bar.getByText("自動実行 有効（v1）")).toBeVisible();
  await expect(bar.getByText("自動実行", { exact: true })).toHaveCSS("position", "absolute");
  await expect(bar.getByRole("button", { name: "元に戻す" })).toBeVisible();
  await expect(more).toBeHidden();
  if (SHOTS) await page.screenshot({ path: `${SHOTS}/narrow-bar-1280.png` });

  // --- 1024px でサイドバーを開いている: 実行履歴・バージョン履歴・共有・自動実行を ⋯ に畳む。
  await page.setViewportSize({ width: 1024, height: 768 });
  await expect(more).toBeVisible();
  await expectTitleClear(page, name);
  await expect(bar.getByTestId("workflow-share")).toBeHidden();
  await expect(bar.getByRole("button", { name: "自動実行の設定" })).toBeHidden();
  await expect(bar.getByText("自動実行 有効（v1）")).toBeHidden();
  await expect(bar.getByRole("button", { name: "元に戻す" })).toBeVisible();
  if (SHOTS) await page.screenshot({ path: `${SHOTS}/narrow-bar-1024.png` });
  await more.click();
  const menu = page.getByRole("menu");
  await expect(menu.getByRole("menuitem")).toHaveText(["実行履歴", "バージョン履歴", "共有", "自動実行の設定"]);
  if (SHOTS) await page.screenshot({ path: `${SHOTS}/narrow-bar-1024-menu.png` });
  await menu.getByRole("menuitem", { name: "共有" }).click();
  const share = page.getByRole("dialog");
  await expect(share).toBeVisible();
  await share.getByRole("button", { name: "閉じる" }).click();
  await expect(share).toHaveCount(0);

  // --- 800px でサイドバーを開いている: 元に戻す・やり直すも畳み、保存の状態をタイトルの下に置く。
  await page.setViewportSize({ width: 800, height: 700 });
  await expect(narrowMore).toBeVisible();
  await expect(more).toBeHidden();
  await expect(bar.getByRole("button", { name: "元に戻す" })).toBeHidden();
  const at800 = await expectTitleClear(page, name);
  expect(at800.stacked).toBe(true);
  expect(await page.evaluate(() => document.documentElement.scrollWidth)).toBeLessThanOrEqual(800);
  if (SHOTS) await page.screenshot({ path: `${SHOTS}/narrow-bar-800.png` });
  await narrowMore.click();
  await expect(page.getByRole("menu").getByRole("menuitem")).toHaveText([
    "元に戻す",
    "やり直す",
    "実行履歴",
    "バージョン履歴",
    "共有",
    "自動実行の設定",
  ]);
  if (SHOTS) await page.screenshot({ path: `${SHOTS}/narrow-bar-800-menu.png` });
  await page.keyboard.press("Escape");

  // --- スマホ（390px）: PC のバーの ⋯ は出さず、今までどおり右上の ⋯ に畳む。
  const phoneCtx = await browser.newContext({
    storageState: await page.context().storageState(),
    viewport: { width: 390, height: 844 },
    hasTouch: true,
    isMobile: true,
    deviceScaleFactor: 2,
    ignoreHTTPSErrors: true,
  });
  try {
    const phone = await phoneCtx.newPage();
    await phone.goto(`/workflows/${id}`);
    await expect(phone.getByTestId("workflow-mobile-flow")).toBeVisible({ timeout: 30_000 });
    await expect(phone.getByTestId("workflow-bar-more")).toHaveCount(0);
    await expect(phone.getByTestId("workflow-bar-more-narrow")).toHaveCount(0);
    expect(await phone.evaluate(() => document.documentElement.scrollWidth)).toBeLessThanOrEqual(390);
    if (SHOTS) await phone.screenshot({ path: `${SHOTS}/narrow-bar-phone.png` });
    await phone.getByTestId("chrome-menu").click();
    await expect(phone.getByRole("menu").getByRole("menuitem", { name: "共有" })).toBeVisible();
    if (SHOTS) await phone.screenshot({ path: `${SHOTS}/narrow-bar-phone-menu.png` });
  } finally {
    await phoneCtx.close();
  }
});
