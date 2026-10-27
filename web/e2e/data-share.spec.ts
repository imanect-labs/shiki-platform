import { expect, test, type Page } from "@playwright/test";

import { loginAs, loginViaKeycloak, uniqueName } from "./helpers";

/// テーブルの共有（#864）: alice（owner）が bob へ閲覧で共有 → bob の /data 一覧に「共有」として出て開ける
/// （共有の操作は出ない）→ alice が解除すると bob の一覧から消え、開いても見つからない。

async function csrf(page: Page): Promise<string> {
  return (await page.context().cookies()).find((c) => c.name === "shiki_csrf")?.value ?? "";
}

async function seedTable(page: Page, name: string): Promise<string> {
  const headers = { "X-CSRF-Token": await csrf(page) };
  const res = await page.request.post("/api/data/tables", {
    headers,
    data: { name, schema: { fields: [{ key: "name", display_name: "件名", type: "text", indexed: true }] } },
  });
  expect(res.status(), await res.text()).toBe(201);
  const id = ((await res.json()) as { id: string }).id;
  const r = await page.request.post(`/api/data/tables/${id}/records`, { headers, data: { data: { name: "共有の行" } } });
  expect(r.status(), await r.text()).toBe(201);
  return id;
}

test("テーブルを閲覧で共有すると相手の一覧に出て開け、解除で消える", async ({ page, browser }) => {
  // bob を先に一度ログインさせる（ディレクトリに載せて検索に出す）。
  const bobCtx = await browser.newContext();
  const bob = await bobCtx.newPage();
  await loginAs(bob, "bob");

  await loginViaKeycloak(page); // alice
  const name = uniqueName("e2e-共有テーブル");
  const tableId = await seedTable(page, name);
  await page.goto(`/data/${tableId}`);
  await expect(page.getByTestId("data-grid")).toHaveAttribute("data-loaded", "1");

  // owner には共有の操作が出る。ダイアログで bob を検索して閲覧で共有する。
  await page.getByRole("button", { name: "テーブルを共有" }).click();
  const share = page.getByTestId("table-share-dialog");
  await expect(share.getByText(`「${name}」を共有`)).toBeVisible();
  await expect(share.getByText("まだ誰にも共有していません。")).toBeVisible();
  await share.getByPlaceholder("名前・メールで検索").fill("bob");
  const bobRow = share.locator("li", { hasText: "bob@" });
  await expect(bobRow).toBeVisible({ timeout: 10_000 });
  await bobRow.getByRole("button", { name: "共有", exact: true }).click();
  const entry = share.getByTestId("share-entry").filter({ hasText: "bob@" });
  await expect(entry).toBeVisible({ timeout: 10_000 });

  // bob: 一覧に「共有」として出て、開ける（行が読める・共有の操作は出ない）。
  await bob.goto("/data");
  const row = bob.getByTestId("data-table-list").locator("li", { hasText: name });
  await expect(row).toBeVisible({ timeout: 15_000 });
  await expect(row.getByText("共有", { exact: true })).toBeVisible();
  await row.getByRole("button").first().click();
  await expect(bob.getByTestId("data-grid")).toHaveAttribute("data-loaded", "1", { timeout: 15_000 });
  await expect(bob.getByRole("button", { name: "テーブルを共有" })).toHaveCount(0);
  await expect(bob.getByTestId("add-record")).toHaveCount(0);

  // alice が解除する。
  await entry.getByRole("button", { name: /の共有を解除$/ }).click();
  await expect(share.getByText("まだ誰にも共有していません。")).toBeVisible({ timeout: 10_000 });

  // bob: 一覧から消え、開いても見つからない。
  await bob.goto("/data");
  // 一覧の読み込みを待ってから（bob に他のテーブルが無ければ空の表示）消えたことを確かめる。
  await expect(bob.getByTestId("data-table-list").or(bob.getByText("まだテーブルがありません"))).toBeVisible({
    timeout: 15_000,
  });
  await expect(bob.getByText(name)).toHaveCount(0);
  await bob.goto(`/data/${tableId}`);
  await expect(bob.getByText("テーブルが見つかりません。削除されたか、見る権限がありません。")).toBeVisible({
    timeout: 15_000,
  });
  await bobCtx.close();
});
