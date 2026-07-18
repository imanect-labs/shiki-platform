import { expect, test } from "@playwright/test";

import { loginViaKeycloak } from "./helpers";

/// 没入エディタ（human 要望）: エディタを開くとサイドバーが畳まれ、シェル上部バーが消える。
/// ドライブへ戻ると元の状態（展開・上部バーあり）へ復元する（手動 pref は汚さない）。

test("エディタで没入し、離れると復元する", async ({ page }) => {
  await loginViaKeycloak(page);
  await page.goto("/drive");

  const aside = page.locator("aside[data-collapsed]");
  // ドライブでは展開・上部バー（現在地見出し）あり。
  await expect(aside).toHaveAttribute("data-collapsed", "false");
  await expect(page.getByRole("banner")).toBeVisible();

  // ノートを開く → 畳み＋上部バー無し。
  await page.getByRole("button", { name: "新規作成" }).click();
  await page.getByTestId("new-note").click();
  await page.waitForURL(/\/notes\//, { timeout: 30_000 });
  await expect(page.getByTestId("note-editor")).toBeVisible({ timeout: 20_000 });
  await expect(aside).toHaveAttribute("data-collapsed", "true");
  await expect(page.getByRole("banner")).toHaveCount(0);

  // ドライブへ戻る → 復元（ハードリロードでも展開＝手動 pref を localStorage に
  // 汚していないことの確認。ルート由来の折りたたみは一時状態）。
  await page.goto("/drive");
  await expect(aside).toHaveAttribute("data-collapsed", "false");
  await expect(page.getByRole("banner")).toBeVisible();
});
