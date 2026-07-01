import { expect, test } from "@playwright/test";

import { loginAs, loginViaKeycloak, uniqueName } from "./helpers";

/// 共有ダイアログのテナント分離＋共有付与を検証する（issue #20 受け入れ条件: 個人へ権限付与）。
/// alice(tenant a-corp) の検索に bob は出るが charlie(tenant b-corp) は出ない。
/// 付与した bob は別ログインで「共有済み」に当該ノードが現れる。
test("共有ダイアログ: テナント分離と共有付与", async ({ page, browser }) => {
  await loginViaKeycloak(page); // alice
  await page.goto("/drive");

  // 共有対象のフォルダを作る。
  const folder = uniqueName("共有フォルダ");
  await page.getByRole("button", { name: "新規フォルダ" }).click();
  const dialog = page.getByRole("dialog");
  await dialog.getByRole("textbox").fill(folder);
  await dialog.getByRole("button", { name: "作成" }).click();
  await expect(page.getByText(folder, { exact: true })).toBeVisible();

  // 共有ダイアログを開く。
  await page.getByRole("button", { name: `「${folder}」の操作` }).click();
  await page.getByRole("menuitem", { name: "共有" }).click();
  const share = page.getByRole("dialog");
  await expect(share.getByText(`「${folder}」を共有`)).toBeVisible();

  const search = share.getByPlaceholder("名前・メールで検索");

  // bob（同テナント）は検索に出る。
  await search.fill("bob");
  await expect(share.getByText("bob@a-corp.example.com")).toBeVisible({ timeout: 10_000 });

  // charlie（別テナント b-corp）は出ない。
  await search.fill("charlie");
  await expect(share.getByText("該当するユーザーがいません")).toBeVisible({ timeout: 10_000 });
  await expect(share.getByText("charlie@b-corp.example.com")).toHaveCount(0);

  // bob に閲覧権限を付与する。
  await search.fill("bob");
  const bobRow = share.locator("li", { hasText: "bob@a-corp.example.com" });
  await bobRow.getByRole("button", { name: "共有", exact: true }).click();
  // 共有中のメンバーに bob が現れる。
  await expect(share.getByText("00000000-0000-0000-0000-000000000002")).toBeVisible();

  // --- bob として別コンテキストでログインし、共有済みに当該フォルダが出ることを確認 ---
  const bobCtx = await browser.newContext();
  const bobPage = await bobCtx.newPage();
  await loginAs(bobPage, "bob");
  await bobPage.goto("/drive/shared");
  await expect(bobPage.getByText(folder, { exact: true })).toBeVisible({ timeout: 15_000 });
  await bobCtx.close();
});
