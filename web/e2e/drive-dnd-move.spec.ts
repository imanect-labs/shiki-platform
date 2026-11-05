import { expect, test, type Page } from "@playwright/test";

import { loginViaKeycloak, uniqueName } from "./helpers";

/// ドライブ内の D&D 移動（#1045）。行をフォルダ行へ落とすと移動し、パンくずへ落とすと
/// 祖先へ戻る。フォルダをフォルダへ入れることもできる。
///
/// ドライブ直下は他の spec の残りで長くなり、画面外の行へのドラッグはスクロールの途中で
/// 座標がずれるため、毎回新しい作業フォルダの中で操作する（全行が画面内に収まる）。

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

  const work = uniqueName("DnD作業");
  const folderA = uniqueName("DnD先");
  const folderB = uniqueName("DnD入れ子");
  const fileName = `${uniqueName("DnDメモ")}.txt`;

  await newFolder(page, work);
  await page.getByText(work, { exact: true }).click();
  await expect(page.getByRole("navigation", { name: "パンくず" })).toContainText(work);

  await newFolder(page, folderA);
  await newFolder(page, folderB);
  await page.locator('input[type="file"][multiple]').setInputFiles({
    name: fileName,
    mimeType: "text/plain",
    buffer: Buffer.from("drag and drop\n"),
  });
  await expect(page.getByText(fileName, { exact: true })).toBeVisible({ timeout: 20_000 });
  // 進捗パネルが閉じて一覧の位置が落ち着いてからドラッグする。
  await expect(page.getByLabel("アップロード中")).toHaveCount(0);

  // --- ファイル → フォルダ A ---
  await page.getByText(fileName, { exact: true }).dragTo(page.getByText(folderA, { exact: true }));
  await expect(page.getByText(`「${fileName}」を「${folderA}」へ移動しました`, { exact: true })).toBeVisible();
  await expect(page.getByText(fileName, { exact: true })).toHaveCount(0);

  // --- フォルダ B → フォルダ A（フォルダごと入れる） ---
  await page.getByText(folderB, { exact: true }).dragTo(page.getByText(folderA, { exact: true }));
  await expect(page.getByText(`「${folderB}」を「${folderA}」へ移動しました`, { exact: true })).toBeVisible();
  await expect(page.getByText(folderB, { exact: true })).toHaveCount(0);

  // --- A の中: 両方ある。ファイルをパンくずの作業フォルダへ落として 1 つ上へ戻す ---
  await page.getByText(folderA, { exact: true }).click();
  await expect(page.getByText(fileName, { exact: true })).toBeVisible();
  await expect(page.getByText(folderB, { exact: true })).toBeVisible();
  const crumbs = page.getByRole("navigation", { name: "パンくず" });
  await page.getByText(fileName, { exact: true }).dragTo(crumbs.getByRole("button", { name: work }));
  await expect(page.getByText(`「${fileName}」を「${work}」へ移動しました`, { exact: true })).toBeVisible();
  await expect(page.getByText(fileName, { exact: true })).toHaveCount(0);

  // 作業フォルダへ戻ると、ファイルがある。
  await crumbs.getByRole("button", { name: work }).click();
  await expect(page.getByText(fileName, { exact: true })).toBeVisible();
});
