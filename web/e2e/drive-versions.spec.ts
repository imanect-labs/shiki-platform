import { expect, test } from "@playwright/test";

import { loginViaKeycloak, uniqueName } from "./helpers";

/// 版履歴と復元を検証する（issue #20 受け入れ条件: 版履歴から復元できる）。
/// 同名ファイルを 2 回アップロード→版が増える→旧版を復元すると新版として追記される。
test("版履歴: 再アップロードで版が増え、旧版を復元できる", async ({ page }) => {
  await loginViaKeycloak(page);
  await page.goto("/drive");

  const fileName = `${uniqueName("版テスト")}.txt`;

  // v1 アップロード（ツールバーの隠し入力＝複数選択の方）。
  await page.locator('input[type="file"][multiple]').setInputFiles({
    name: fileName,
    mimeType: "text/plain",
    buffer: Buffer.from("version one\n"),
  });
  await expect(page.getByText(fileName, { exact: true })).toBeVisible({ timeout: 20_000 });

  // 行メニューの「新しいバージョン」で v2 をアップロード（target_node_id 指定）。
  await page.getByRole("button", { name: `「${fileName}」の操作` }).click();
  const [chooser] = await Promise.all([
    page.waitForEvent("filechooser"),
    page.getByRole("menuitem", { name: "新しいバージョン" }).click(),
  ]);
  await chooser.setFiles({
    name: fileName,
    mimeType: "text/plain",
    buffer: Buffer.from("version two is longer\n"),
  });
  // 新バージョンのトーストを待つ（トースト本体＋aria-live で 2 要素マッチするため first）。
  await expect(page.getByText("新しいバージョンをアップロードしました").first()).toBeVisible({
    timeout: 20_000,
  });

  // 版履歴を開く。
  await page.getByRole("button", { name: `「${fileName}」の操作` }).click();
  await page.getByRole("menuitem", { name: "版履歴" }).click();
  const dialog = page.getByRole("dialog");
  await expect(dialog.getByText("バージョン 2")).toBeVisible({ timeout: 10_000 });
  await expect(dialog.getByText("バージョン 1")).toBeVisible();

  // 各版に作者が表示される（日時 · サイズ · 作者 の 3 セグメント・Task 11P.10）。
  const v2Meta = dialog.locator("li", { hasText: "バージョン 2" }).locator("p").last();
  await expect(v2Meta).toHaveText(/·.+·.+/);

  // バージョン 1 を復元 → 新版（バージョン 3）が追記される。
  const v1Row = dialog.locator("li", { hasText: "バージョン 1" });
  await v1Row.getByRole("button", { name: "復元" }).click();
  await expect(dialog.getByText("バージョン 3")).toBeVisible({ timeout: 10_000 });
});
