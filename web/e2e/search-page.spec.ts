import { expect, test } from "@playwright/test";

import { loginViaKeycloak } from "./helpers";

/// 文書検索ページの表示スモーク（Task 2.10）。
/// 実インジェスト E2E（アップロード→索引→検索）は worker/Qdrant を要するため
/// compose 検証で行い、CI ではページの結線（ルート・空状態・入力 UI）のみを見る。
test("文書検索ページが表示され、検索 UI が結線されている", async ({ page }) => {
  await loginViaKeycloak(page);

  await page.goto("/search");
  await expect(page.getByRole("heading", { name: "文書検索" })).toBeVisible();
  // 空状態（検索前）の案内が出る。
  await expect(page.getByText("ドライブの文書を検索")).toBeVisible();
  // 検索入力とモード切替が存在する。
  await expect(page.getByRole("textbox", { name: "検索クエリ" })).toBeVisible();
  await expect(page.getByRole("radio", { name: "ハイブリッド" })).toBeVisible();
  // クエリ未入力では検索ボタンは押せない。
  await expect(page.getByRole("button", { name: "検索" })).toBeDisabled();
});

test("サイドバーとホームから文書検索へ遷移できる", async ({ page }) => {
  await loginViaKeycloak(page);

  // サイドバーの一次ナビ。
  await page.getByRole("button", { name: "文書検索" }).click();
  await page.waitForURL(/\/search$/);
  await expect(page.getByRole("heading", { name: "文書検索" })).toBeVisible();
});
