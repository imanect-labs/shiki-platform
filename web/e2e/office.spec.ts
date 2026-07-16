import { expect, test } from "@playwright/test";

import { loginViaKeycloak } from "./helpers";

/// Office 統合（Task 11.5〜11.7）: Collabora コンテナが必要なため、標準 CI では skip し
/// `OFFICE_E2E=1`（office profile 相当のローカル環境）でのみ実行する。
/// 前提: api が SHIKI__OFFICE__ENABLED=true・Collabora が localhost:9980 で稼働。
test.skip(process.env.OFFICE_E2E !== "1", "OFFICE_E2E=1 のときのみ実行（Collabora が必要）");

test("新規ドキュメント→Collabora エディタが起動し文書が編集できる", async ({ page }) => {
  await loginViaKeycloak(page);
  await page.goto("/drive");

  // 新規作成 > ドキュメント → 同梱テンプレがアップロードされ /office/{id} へ遷移。
  await page.getByRole("button", { name: "新規作成" }).click();
  await page.getByTestId("new-document").click();
  await page.waitForURL(/\/office\//, { timeout: 30_000 });

  // Collabora iframe が読み込まれ、起動スピナーが消える。
  await expect(page.getByTestId("office-frame")).toBeVisible();
  await expect(page.getByText("エディタを起動しています…")).toBeHidden({ timeout: 60_000 });

  // Collabora のエディタ UI（メニューバー）が iframe 内に現れる＝WOPI CheckFileInfo/
  // GetFile が通っている。
  const frame = page.frameLocator('[data-testid="office-frame"]');
  await expect(frame.locator("#main-document-content, #document-container").first()).toBeVisible({
    timeout: 60_000,
  });
});

test("Office 未対応拡張子は開くとダウンロードにフォールバックする", async ({ page }) => {
  await loginViaKeycloak(page);
  await page.goto("/drive");
  // .bin をアップロードして開く → ダウンロードが発火する（/office へは遷移しない）。
  const name = `sample-${Date.now()}.bin`;
  await page.locator('input[type="file"][multiple]').setInputFiles({
    name,
    mimeType: "application/octet-stream",
    buffer: Buffer.from([0x00, 0x01, 0x02]),
  });
  await expect(page.getByText(name, { exact: true })).toBeVisible({ timeout: 20_000 });
  // ダウンロードは presigned URL を新規タブで開く実装（storage.ts triggerDownload）のため
  // popup の発生で検証する。
  const popupPromise = page.waitForEvent("popup", { timeout: 20_000 });
  await page.getByText(name, { exact: true }).click();
  await popupPromise;
  expect(page.url()).not.toMatch(/\/office\//);
});
