import { expect, test, type Browser, type Page } from "@playwright/test";

import { loginViaKeycloak } from "./helpers";

/// ホームの「今日のこと」の e2e:
/// - 最近の共有: 共有されたものが多い人には、1 ページ目が空のまま続きの位置だけが付いて返ることがある（BUG-162）。
///   ホームも続きをたどって共有を出す（1 ページ目だけを読んで「共有なし」にしない）。
/// - ワークフローの承認待ち（受信箱 /approvals・GAP-08）: 件数を 1 行で出し、「承認」の画面へ移る。チャットの承認待ちと
///   言葉で見分けられる。0 件なら出さない。スマホのホーム（対応が要るものだけを出す）にも出る。
///
/// 「今日のこと」は行の数に上限があるので、ほかのテストが作った承認待ち・失敗・利用に押し出されないように、材料の
/// API を固定の応答にする（画面の組み立てを確かめる。件数の数え方は approvals.spec.ts が実データで確かめる）。

const SHOTS = process.env.E2E_SHOTS_DIR;
const PHONE = { width: 390, height: 844 };

type Count = { count: number; more: boolean };

/// 「今日のこと」の材料を固定する。`chatApproval` を渡すと、チャットの承認待ちを 1 件だけ返す。
async function fixToday(page: Page, opts: { count: () => Count; chatApproval?: string }) {
  const now = new Date().toISOString();
  await page.route(
    (u) => u.pathname === "/api/me/approvals",
    (r) =>
      r.fulfill({
        json: {
          items: opts.chatApproval
            ? [
                {
                  thread_id: "00000000-0000-4000-8000-0000000000a1",
                  thread_title: opts.chatApproval,
                  run_id: "00000000-0000-4000-8000-0000000000a2",
                  tool_call_id: "call-1",
                  tool_name: "drive.write",
                  requested_at: now,
                },
              ]
            : [],
        },
      }),
  );
  await page.route(
    (u) => u.pathname === "/api/me/workflow-runs",
    (r) => r.fulfill({ json: { window_hours: 24, succeeded: 0, failed: 0, recent_failures: [] } }),
  );
  await page.route(
    (u) => u.pathname === "/api/me/usage",
    (r) => r.fulfill({ status: 503, json: { title: "LLM が構成されていません" } }),
  );
  await page.route(
    (u) => u.pathname === "/api/approvals/count",
    (r) => r.fulfill({ json: opts.count() }),
  );
}

async function phoneContext(browser: Browser, page: Page) {
  return browser.newContext({
    storageState: await page.context().storageState(),
    viewport: PHONE,
    hasTouch: true,
    isMobile: true,
    deviceScaleFactor: 3,
    ignoreHTTPSErrors: true,
  });
}

test("最近の共有: 1 ページ目が空で続きがあるときも、続きをたどって共有を出す（BUG-162）", async ({ page }) => {
  test.setTimeout(90_000);
  await page.setViewportSize({ width: 1440, height: 900 });
  await loginViaKeycloak(page);
  await fixToday(page, { count: () => ({ count: 0, more: false }) });
  const id = "00000000-0000-4000-8000-0000000000b1";
  const name = `e2e-続きの共有-${Date.now().toString(36)}.md`;
  const now = new Date().toISOString();
  const asked: (string | null)[] = [];
  await page.route(
    (u) => u.pathname === "/api/shares/shared-with-me",
    (r) => {
      const cursor = new URL(r.request().url()).searchParams.get("cursor");
      asked.push(cursor);
      // 1 ページ目は空のまま続きの位置だけ（OpenFGA に問う数を抑えたサーバーの返し方）。続きに共有がある。
      if (!cursor) return r.fulfill({ json: { items: [], next_cursor: "page-2" } });
      return r.fulfill({
        json: {
          items: [
            {
              id,
              name,
              kind: "file",
              version: 1,
              content_type: "text/markdown",
              created_by: "bob",
              updated_by: "bob",
              updated_by_name: "bob",
              created_at: now,
              updated_at: now,
            },
          ],
          next_cursor: null,
        },
      });
    },
  );

  await page.goto("/");
  const today = page.getByRole("region", { name: "今日のこと" });
  const row = today.getByRole("link", { name: new RegExp(name.replace(/[.*+?^${}()|[\]\\]/g, "\\$&")) });
  await expect(row).toBeVisible({ timeout: 20_000 });
  await expect(row).toContainText("共有・bob");
  await expect(row).toHaveAttribute("href", `/notes/${id}`);
  // 続きの位置をたどって 2 ページ目を読んだ（1 ページ目だけで止めない）。開発のサーバーでは読み込みが 2 回走るので、
  // 回数でなく、続きの位置を付けて読んだことを見る。
  expect(asked).toContain(null);
  expect(asked).toContain("page-2");
  if (SHOTS) await today.screenshot({ path: `${SHOTS}/home-today-shares-pc.png` });
});

test("ワークフローの承認待ち: 件数を出して「承認」の画面へ移り、0 件なら出さない（PC・スマホ・GAP-08）", async ({
  browser,
  page,
}) => {
  test.setTimeout(120_000);
  await page.setViewportSize({ width: 1440, height: 900 });
  await loginViaKeycloak(page);
  let count: Count = { count: 3, more: false };
  const chat = `e2e-チャットの承認-${Date.now().toString(36)}`;
  await fixToday(page, { count: () => count, chatApproval: chat });

  // チャットの承認待ち（会話ごと）と、ワークフローの承認待ち（件数の 1 行）を言葉で見分けられる。
  await page.goto("/");
  const today = page.getByRole("region", { name: "今日のこと" });
  const workflow = today.getByRole("link", { name: /ワークフローの承認待ち 3 件/ });
  await expect(workflow).toBeVisible({ timeout: 20_000 });
  await expect(workflow).toContainText("「承認」の画面で判断します");
  await expect(workflow).toHaveAttribute("href", "/approvals");
  await expect(today.getByRole("link", { name: new RegExp(chat) })).toContainText("チャットの承認待ち・");
  if (SHOTS) await today.screenshot({ path: `${SHOTS}/home-today-workflow-approvals-pc.png` });

  // 押すと「承認」の画面（受信箱）へ移る。
  await workflow.click();
  await page.waitForURL(/\/approvals$/, { timeout: 15_000 });
  await expect(page.getByRole("heading", { name: "承認", exact: true })).toBeVisible();

  // 数え切れずに数えた分が 0 件なら、数を言わずに「あるかもしれません」と出す（サイドバーのバッジと同じ）。
  count = { count: 0, more: true };
  await page.goto("/");
  await expect(today.getByRole("link", { name: /ワークフローの承認待ちがあるかもしれません/ })).toBeVisible({
    timeout: 20_000,
  });
  await expect(today).not.toContainText("0 件");

  // 0 件なら出さない（チャットの承認待ちは残る）。
  count = { count: 0, more: false };
  await page.goto("/");
  await expect(today.getByRole("link", { name: new RegExp(chat) })).toBeVisible({ timeout: 20_000 });
  await expect(today).not.toContainText("ワークフローの承認待ち");

  // スマホのホームは対応が要るものだけを出す。ワークフローの承認待ちも出て、横にはみ出さない。
  count = { count: 12, more: false };
  const ctx = await phoneContext(browser, page);
  try {
    const phone = await ctx.newPage();
    await fixToday(phone, { count: () => count });
    await phone.goto("/");
    const phoneToday = phone.getByRole("region", { name: "今日のこと" });
    const row = phoneToday.getByRole("link", { name: /ワークフローの承認待ち 12 件/ });
    await expect(row).toBeVisible({ timeout: 20_000 });
    await row.scrollIntoViewIfNeeded();
    expect(await phone.evaluate(() => document.documentElement.scrollWidth)).toBeLessThanOrEqual(PHONE.width);
    if (SHOTS) await phone.screenshot({ path: `${SHOTS}/home-today-workflow-approvals-phone.png` });
    await row.tap();
    await phone.waitForURL(/\/approvals$/, { timeout: 15_000 });
  } finally {
    await ctx.close();
  }
});
