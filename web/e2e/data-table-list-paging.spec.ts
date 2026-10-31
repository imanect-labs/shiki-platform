import { expect, test, type Page, type Route } from "@playwright/test";

import { loginViaKeycloak } from "./helpers";

/// /data のテーブルの一覧が、続きの位置をたどって読む（BUG-149 のレビュー）:
/// - 見られるテーブルが多い人では、サーバーが OpenFGA に問う数を抑えるので、1 ページが短いか 0 件のまま、続きの位置
///   （next_cursor）だけが付いて返ることがある。画面は続きをたどって読み、往復の上限で止まったら「さらに読み込む」を出す。
///   0 件でも続きがあるあいだは「まだテーブルがありません」と言わない（付録 C の P5）。
/// - 一覧を読めなかったときも、空の状態に見せずに理由と「もう一度読み込む」を出す。
///
/// サーバーの応答は route で差し替える（大きな組織を手元に作らない）。差し替えはテストの終わりに外す。
/// `E2E_SHOTS_DIR` があれば、PC（1440px）とスマホ（390px）の画面を撮る。

const SHOTS = process.env.E2E_SHOTS_DIR;
const LIST = /\/api\/data\/tables(\?.*)?$/;

function table(n: number) {
  return {
    id: `00000000-0000-4000-8000-${String(n).padStart(12, "0")}`,
    name: `見つかったテーブル ${n}`,
    schema: { fields: [{ key: "title", display_name: "件名", type: "text" }], field_policy: [] },
    schema_version: 1,
    created_by: "someone-else",
    created_at: "2026-10-30T00:00:00Z",
    updated_at: "2026-10-30T00:00:00Z",
  };
}

/// 一覧の頼みに、続きの位置ごとの応答を返す（`cursor` が無い頼みは `""`）。頼まれた位置を順に残す。
async function serveList(page: Page, pages: Record<string, { items: unknown[]; next_cursor: string | null }>) {
  const asked: string[] = [];
  const handler = async (route: Route) => {
    if (route.request().method() !== "GET") return route.fallback();
    const cursor = new URL(route.request().url()).searchParams.get("cursor") ?? "";
    asked.push(cursor);
    const body = pages[cursor];
    if (!body) return route.fulfill({ status: 500, contentType: "application/json", body: JSON.stringify({ status: 500 }) });
    return route.fulfill({ status: 200, contentType: "application/json", body: JSON.stringify(body) });
  };
  await page.route(LIST, handler);
  return { asked, stop: () => page.unroute(LIST, handler) };
}

test("見られるテーブルが多い人の一覧: 0 件のページでも続きをたどり、「さらに読み込む」で残りを出す", async ({ page, browser }) => {
  test.setTimeout(120_000);
  await page.setViewportSize({ width: 1440, height: 900 });
  await loginViaKeycloak(page);
  // 先頭から 5 ページは 0 件（見えないテーブルばかり）で、6 ページ目に 1 件、7 ページ目で終わる。
  const pages: Record<string, { items: unknown[]; next_cursor: string | null }> = {};
  for (let i = 0; i < 5; i += 1) pages[i === 0 ? "" : `c${i}`] = { items: [], next_cursor: `c${i + 1}` };
  pages.c5 = { items: [table(1)], next_cursor: "c6" };
  pages.c6 = { items: [table(2)], next_cursor: null };
  const served = await serveList(page, pages);
  try {
    await page.goto("/data");
    // 往復の上限（5 回）まで読んでも見つからないあいだは、空の状態ではなく、続きを探せることを言う。
    const pending = page.getByTestId("data-table-list-pending");
    await expect(pending).toContainText("まだ見つかっていません。「さらに読み込む」で続きを探します。", { timeout: 15_000 });
    await expect(page.getByText("まだテーブルがありません")).toHaveCount(0);
    expect(served.asked).toEqual(["", "c1", "c2", "c3", "c4"]);
    if (SHOTS) await page.screenshot({ path: `${SHOTS}/data-list-pending-pc.png` });

    // 続きを読むと、残りのテーブルが出て、続きが無くなればボタンも消える。
    await page.getByTestId("data-table-list-more").click();
    const list = page.getByTestId("data-table-list");
    await expect(list).toContainText("見つかったテーブル 1");
    await expect(list).toContainText("見つかったテーブル 2");
    await expect(page.getByTestId("data-table-list-more")).toHaveCount(0);
    expect(served.asked.slice(5)).toEqual(["c5", "c6"]);
    if (SHOTS) await page.screenshot({ path: `${SHOTS}/data-list-more-pc.png` });

    // スマホ幅でも、続きを探す案内とボタンが横にはみ出さない。
    if (SHOTS) {
      const phone = await browser.newContext({
        storageState: await page.context().storageState(),
        viewport: { width: 390, height: 844 },
        deviceScaleFactor: 2,
        isMobile: true,
        hasTouch: true,
      });
      try {
        const p = await phone.newPage();
        const phoneServed = await serveList(p, pages);
        await p.goto("/data");
        await expect(p.getByTestId("data-table-list-pending")).toBeVisible({ timeout: 15_000 });
        expect(await p.evaluate(() => document.documentElement.scrollWidth <= window.innerWidth + 1)).toBe(true);
        await p.screenshot({ path: `${SHOTS}/data-list-pending-phone.png` });
        await phoneServed.stop();
      } finally {
        await phone.close();
      }
    }
  } finally {
    await served.stop();
  }
});

test("テーブルの一覧を読めなかったときは、空の状態に見せずに読み直せる", async ({ page }) => {
  test.setTimeout(120_000);
  await page.setViewportSize({ width: 1440, height: 900 });
  await loginViaKeycloak(page);
  // 1 回目は読めず（応答の無い位置は 500）、読み直すと 1 件。
  const pages: Record<string, { items: unknown[]; next_cursor: string | null }> = {};
  const served = await serveList(page, pages);
  try {
    await page.goto("/data");
    const error = page.getByTestId("data-table-list-error");
    await expect(error).toContainText("テーブルの一覧を読み込めませんでした。少し待ってから、もう一度読み込んでください。", {
      timeout: 15_000,
    });
    await expect(page.getByText("まだテーブルがありません")).toHaveCount(0);
    if (SHOTS) await page.screenshot({ path: `${SHOTS}/data-list-error-pc.png` });
    pages[""] = { items: [table(3)], next_cursor: null };
    await error.getByRole("button", { name: "もう一度読み込む" }).click();
    await expect(page.getByTestId("data-table-list")).toContainText("見つかったテーブル 3");
  } finally {
    await served.stop();
  }
});
