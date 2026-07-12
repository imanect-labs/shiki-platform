import { test, type Page } from "@playwright/test";

import { loginViaKeycloak } from "./helpers";

/// UIUX 刷新の視覚確認用キャプチャ（SHOTS_DIR 設定時のみ実行）。
/// 例: SHOTS_DIR=/tmp/shots pnpm exec playwright test uiux-shots
const SHOTS = process.env.SHOTS_DIR ?? "";

test.describe.configure({ timeout: 180_000 });

async function shot(page: Page, name: string) {
  // ライト → ダークの両方を撮る（next-themes は system 追従なので emulateMedia で切替）。
  await page.emulateMedia({ colorScheme: "light" });
  await page.waitForTimeout(250);
  await page.screenshot({ path: `${SHOTS}/${name}-light.png`, fullPage: false });
  await page.emulateMedia({ colorScheme: "dark" });
  await page.waitForTimeout(250);
  await page.screenshot({ path: `${SHOTS}/${name}-dark.png`, fullPage: false });
  await page.emulateMedia({ colorScheme: "light" });
}

test("UIUX Phase 1 スクショ", async ({ page }) => {
  test.skip(!SHOTS, "SHOTS_DIR 未設定");
  await page.setViewportSize({ width: 1440, height: 900 });
  await loginViaKeycloak(page);

  // ホーム（トップ・デザイン言語の基準）。
  await page.goto("/");
  await page.waitForLoadState("networkidle");
  await page.waitForTimeout(400);
  await shot(page, "01-home");

  // ドライブ。
  await page.goto("/drive");
  await page.waitForLoadState("networkidle");
  await page.waitForTimeout(600);
  await shot(page, "02-drive");

  // 検索パレット（⌘K）。
  await page.goto("/");
  await page.waitForLoadState("networkidle");
  await page.keyboard.press("Meta+k");
  await page.waitForTimeout(400);
  await shot(page, "03-search-palette");
  await page.keyboard.press("Escape");

  // ワークフロー一覧。
  await page.goto("/workflows");
  await page.waitForLoadState("networkidle");
  await page.waitForTimeout(500);
  await shot(page, "04-workflows");

  // 会話（/c）: ホームでメッセージ送信 → スレッドへ遷移。統一ヘッダ（共有/設定）を確認。
  await page.goto("/");
  await page.waitForLoadState("networkidle");
  const composer = page.getByRole("textbox").first();
  await composer.click();
  await composer.fill("UIUX のスクリーンショット用サンプルメッセージです。");
  await page.keyboard.press("Enter");
  await page.waitForURL(/\/c\/[0-9a-f-]+/i, { timeout: 30_000 }).catch(() => {});
  await page.waitForTimeout(2500);
  await shot(page, "05-chat");

  // サイドバー折りたたみ。
  await page.getByRole("button", { name: "サイドバーを折りたたむ" }).click().catch(() => {});
  await page.waitForTimeout(500);
  await shot(page, "06-sidebar-collapsed");
});
