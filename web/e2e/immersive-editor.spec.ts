import { expect, test } from "@playwright/test";

import { loginViaKeycloak } from "./helpers";

/// 没入エディタ（human 要望）: エディタを開くとサイドバーが畳まれ、シェル既定の現在地バー
/// （📁 現在地）が消える。ノート/スライドは usePageHeader で独自ヘッダ（戻る/名前/AI に依頼）を
/// **唯一のバー**として注入する（app-shell の ShellHeader 参照）。ドライブへ戻ると既定の
/// 現在地バーへ復元する（手動 pref は汚さない）。

test("エディタで没入し、離れると復元する", async ({ page }) => {
  await loginViaKeycloak(page);
  await page.goto("/drive");

  const aside = page.locator("aside[data-collapsed]");
  // ドライブでは展開・既定の現在地バー（ドライブ見出し）あり。
  await expect(aside).toHaveAttribute("data-collapsed", "false");
  const banner = page.getByRole("banner");
  await expect(banner).toBeVisible();
  await expect(banner.getByRole("img", { name: "ドライブ" })).toBeVisible();

  // ノートを開く → 畳み＋既定の現在地バーは消え、ノートの注入ヘッダが唯一のバーになる。
  await page.getByRole("button", { name: "新規作成" }).click();
  await page.getByTestId("new-note").click();
  await page.waitForURL(/\/notes\//, { timeout: 30_000 });
  await expect(page.getByTestId("note-editor")).toBeVisible({ timeout: 20_000 });
  await expect(aside).toHaveAttribute("data-collapsed", "true");
  // 既定の現在地バー（ドライブ見出し）は消え、ノートヘッダ（戻る/AI に依頼）へ置き換わる。
  await expect(banner.getByRole("img", { name: "ドライブ" })).toHaveCount(0);
  await expect(banner.getByRole("link", { name: "ドライブへ戻る" })).toBeVisible();
  await expect(banner.getByTestId("note-ask-ai")).toBeVisible();

  // ドライブへ戻る → 復元（ハードリロードでも展開＝手動 pref を localStorage に
  // 汚していないことの確認。ルート由来の折りたたみは一時状態）。
  await page.goto("/drive");
  await expect(aside).toHaveAttribute("data-collapsed", "false");
  await expect(banner.getByRole("img", { name: "ドライブ" })).toBeVisible();
});
