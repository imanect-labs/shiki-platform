import { expect, test, type Page } from "@playwright/test";

import { loginAs, loginViaKeycloak } from "./helpers";

/// 文書検索の内訳の e2e（BUG-94・D-80）: 内訳（各段の件数・権限で除いた文書の数）のスイッチは組織の管理者にだけ
/// 出す。管理者でない人が `debug: true` で検索しても、サーバーは内訳を返さない（403 にもしない）。

async function csrfHeaders(page: Page): Promise<Record<string, string>> {
  const csrf = (await page.context().cookies()).find((c) => c.name === "shiki_csrf")?.value ?? "";
  return { "x-csrf-token": csrf, "content-type": "application/json" };
}

test("文書検索の内訳: 組織の管理者にだけスイッチを出し、ほかの人には内訳を返さない（BUG-94）", async ({ browser, page }) => {
  test.setTimeout(90_000);
  await loginViaKeycloak(page);
  await page.goto("/search");
  await expect(page.getByRole("searchbox", { name: "検索クエリ" })).toBeVisible();
  await expect(page.getByText("文書検索の内訳を表示")).toBeVisible();

  const bobCtx = await browser.newContext();
  try {
    const bob = await bobCtx.newPage();
    await loginAs(bob, "bob");
    await bob.goto("/search");
    await expect(bob.getByRole("searchbox", { name: "検索クエリ" })).toBeVisible();
    await expect(bob.getByText("文書検索の内訳を表示")).toHaveCount(0);
    // サーバーも返さない（検索の仕組みが動いていない構成では 503 なので、そのときは画面だけを見る）。
    const res = await bob.request.post("/api/search", {
      headers: await csrfHeaders(bob),
      data: { query: "経費", debug: true },
    });
    if (res.status() !== 503) {
      expect(res.ok(), await res.text()).toBeTruthy();
      const body = (await res.json()) as Record<string, unknown>;
      expect(body.debug).toBeUndefined();
    }
  } finally {
    await bobCtx.close();
  }
});
