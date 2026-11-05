import { expect, test, type Page } from "@playwright/test";

import { loginViaKeycloak, uniqueName } from "./helpers";

/// ドライブ内の D&D 移動（#1045）。行をフォルダ行へ落とすと移動し、パンくずへ落とすと
/// 祖先（ドライブ直下）へ戻る。フォルダをフォルダへ入れることもできる。

async function newFolder(page: Page, name: string) {
  await page.getByRole("button", { name: "新規フォルダ" }).click();
  const dialog = page.getByRole("dialog");
  await dialog.getByRole("textbox").fill(name);
  await dialog.getByRole("button", { name: "作成" }).click();
  await expect(page.getByText(name, { exact: true })).toBeVisible();
}

test("ドライブの項目を D&D でフォルダへ移動・パンくずで戻せる", async ({ page }) => {
  await loginViaKeycloak(page);
  await page.goto("/drive");

  const folderA = uniqueName("DnD先");
  const folderB = uniqueName("DnD入れ子");
  const fileName = `${uniqueName("DnDメモ")}.txt`;

  await newFolder(page, folderA);
  await newFolder(page, folderB);
  await page.locator('input[type="file"][multiple]').setInputFiles({
    name: fileName,
    mimeType: "text/plain",
    buffer: Buffer.from("drag and drop\n"),
  });
  await expect(page.getByText(fileName, { exact: true })).toBeVisible({ timeout: 20_000 });

  // --- ファイル → フォルダ A ---
  await page.getByText(fileName, { exact: true }).dragTo(page.getByText(folderA, { exact: true }));
  await expect(page.getByText(`「${fileName}」を「${folderA}」へ移動しました`)).toBeVisible();
  await expect(page.getByText(fileName, { exact: true })).toHaveCount(0);

  // --- フォルダ B → フォルダ A（フォルダごと入れる） ---
  await page.getByText(folderB, { exact: true }).dragTo(page.getByText(folderA, { exact: true }));
  await expect(page.getByText(folderB, { exact: true })).toHaveCount(0);

  // --- A の中: 両方ある。ファイルをパンくずの「ドライブ」へ落として直下へ戻す ---
  await page.getByText(folderA, { exact: true }).click();
  await expect(page.getByText(fileName, { exact: true })).toBeVisible();
  await expect(page.getByText(folderB, { exact: true })).toBeVisible();
  const crumbs = page.getByRole("navigation", { name: "パンくず" });
  await page.getByText(fileName, { exact: true }).dragTo(crumbs.getByRole("button", { name: "ドライブ" }));
  await expect(page.getByText(`「${fileName}」を「ドライブ」へ移動しました`)).toBeVisible();
  await expect(page.getByText(fileName, { exact: true })).toHaveCount(0);

  // 直下へ戻ると、ファイルがある。
  await crumbs.getByRole("button", { name: "ドライブ" }).click();
  await expect(page.getByText(fileName, { exact: true })).toBeVisible();
});
