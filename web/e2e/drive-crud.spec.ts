import { expect, test } from "@playwright/test";

import { loginViaKeycloak, uniqueName } from "./helpers";

/// Drive の CRUD（フォルダ作成→アップロード→リネーム→移動→削除→ゴミ箱復元）を
/// UI から一気通貫で検証する（issue #20 受け入れ条件: CRUD が UI から完結）。
test("ドライブの CRUD がUIから完結する", async ({ page }) => {
  await loginViaKeycloak(page);
  await page.goto("/drive");

  const folder = uniqueName("フォルダ");
  const fileName = `${uniqueName("メモ")}.txt`;
  const renamed = `${uniqueName("改名")}.txt`;

  // --- フォルダ作成 ---
  await page.getByRole("button", { name: "新規フォルダ" }).click();
  const newFolderDialog = page.getByRole("dialog");
  await newFolderDialog.getByRole("textbox").fill(folder);
  await newFolderDialog.getByRole("button", { name: "作成" }).click();
  await expect(page.getByText(folder, { exact: true })).toBeVisible();

  // --- アップロード（ルート直下） ---
  await page.locator('input[type="file"][multiple]').setInputFiles({
    name: fileName,
    mimeType: "text/plain",
    buffer: Buffer.from("hello shiki drive e2e\n"),
  });
  await expect(page.getByText(fileName, { exact: true })).toBeVisible({ timeout: 20_000 });

  // --- リネーム ---
  await page.getByRole("button", { name: `「${fileName}」の操作` }).click();
  await page.getByRole("menuitem", { name: "名前を変更" }).click();
  const renameDialog = page.getByRole("dialog");
  await renameDialog.getByRole("textbox").fill(renamed);
  await renameDialog.getByRole("button", { name: "変更" }).click();
  await expect(page.getByText(renamed, { exact: true })).toBeVisible();

  // --- 移動（作成したフォルダへ） ---
  await page.getByRole("button", { name: `「${renamed}」の操作` }).click();
  await page.getByRole("menuitem", { name: "移動", exact: true }).click();
  const moveDialog = page.getByRole("dialog");
  // フォルダへドリルインしてから「ここへ移動」で移動先に確定する。
  await moveDialog.getByRole("button", { name: folder }).click();
  await moveDialog.getByRole("button", { name: "ここへ移動" }).click();
  await expect(moveDialog).toBeHidden();
  // ルートからは消える。
  await expect(page.getByText(renamed, { exact: true })).toHaveCount(0);

  // フォルダを開くと中に移動済みのファイルがある。
  await page.getByText(folder, { exact: true }).click();
  await expect(page.getByText(renamed, { exact: true })).toBeVisible();

  // --- フォルダ削除（パンくずのルートへ戻ってから） ---
  await page
    .getByRole("navigation", { name: "パンくず" })
    .getByRole("button", { name: "ドライブ" })
    .click();
  await page.getByRole("button", { name: `「${folder}」の操作` }).click();
  await page.getByRole("menuitem", { name: "ゴミ箱へ移動" }).click();
  await page.getByRole("dialog").getByRole("button", { name: "ゴミ箱へ移動" }).click();
  await expect(page.getByText(folder, { exact: true })).toHaveCount(0);

  // --- ゴミ箱から復元 ---
  await page.goto("/drive/trash");
  await expect(page.getByText(folder, { exact: true })).toBeVisible();
  await page.getByRole("button", { name: "復元" }).first().click();
  await expect(page.getByText(folder, { exact: true })).toHaveCount(0);

  // ルートに戻ると復元されている。
  await page.goto("/drive");
  await expect(page.getByText(folder, { exact: true })).toBeVisible();
});
