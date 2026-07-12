import { test, expect, type Page } from "@playwright/test";

import { submitKeycloakLogin } from "./helpers";

const SHOTS = process.env.SHOTS_DIR ?? "";
test.describe.configure({ timeout: 240_000 });

async function login(page: Page) {
  await page.goto("/login");
  await page.getByRole("button", { name: "Keycloak でログイン" }).click();
  await submitKeycloakLogin(page);
  await page.waitForURL((url) => !url.pathname.startsWith("/login"), { timeout: 30_000 });
  await expect(page.getByRole("button", { name: "アカウントメニューを開く" })).toBeVisible({
    timeout: 60_000,
  });
}

async function shot(page: Page, name: string) {
  await page.emulateMedia({ colorScheme: "light" });
  await page.waitForTimeout(200);
  await page.screenshot({ path: `${SHOTS}/${name}-light.png` });
  await page.emulateMedia({ colorScheme: "dark" });
  await page.waitForTimeout(200);
  await page.screenshot({ path: `${SHOTS}/${name}-dark.png` });
  await page.emulateMedia({ colorScheme: "light" });
}

test("ノートエディタ ツールバー/バブル", async ({ page }) => {
  test.skip(!SHOTS, "SHOTS_DIR 未設定");
  await page.setViewportSize({ width: 1440, height: 900 });
  await login(page);

  // 新規ノート作成（drive の新規作成 > ノート）。
  await page.goto("/drive");
  await page.waitForLoadState("networkidle");
  await page.getByRole("button", { name: "新規作成" }).click();
  await page.getByRole("menuitem", { name: "ノート" }).click();
  await page.waitForURL(/\/notes\/[0-9a-f-]+/i, { timeout: 60_000 });
  await page.getByTestId("note-editor").waitFor({ timeout: 60_000 });
  await page.waitForTimeout(1500);

  // 本文を入力（見出し・段落）。
  const editor = page.getByTestId("note-editor");
  await editor.click();
  await page.keyboard.type("プロジェクト計画", { delay: 8 });
  await page.keyboard.press("Enter");
  await page.keyboard.type("これはツールバーとバブルメニューの確認用テキストです。", { delay: 4 });
  await page.waitForTimeout(400);
  // ツールバーが見える状態（07）。
  await shot(page, "07-note-toolbar");

  // 段落テキストを選択してバブルメニューを出す（08）。
  await page.keyboard.press("Home");
  await page.keyboard.press("Shift+End");
  await page.waitForTimeout(600);
  await shot(page, "08-note-bubble");

  // 分割ビュー（アシスタント）を開く（09）。
  await page.getByTestId("note-chat-toggle").click();
  await page.waitForTimeout(1500);
  await shot(page, "09-note-split");
});
