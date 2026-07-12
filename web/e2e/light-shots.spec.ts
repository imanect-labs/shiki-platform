import { test, expect, type Page } from "@playwright/test";

import { submitKeycloakLogin } from "./helpers";

const SHOTS = process.env.SHOTS_DIR ?? "";
test.describe.configure({ timeout: 300_000 });

async function login(page: Page) {
  await page.goto("/login");
  await page.getByRole("button", { name: "Keycloak でログイン" }).click();
  await submitKeycloakLogin(page);
  await page.waitForURL((url) => !url.pathname.startsWith("/login"), { timeout: 30_000 });
  await expect(page.getByRole("button", { name: "アカウントメニューを開く" })).toBeVisible({
    timeout: 60_000,
  });
}

test("ライト全画面（全機能）", async ({ page }) => {
  test.skip(!SHOTS, "SHOTS_DIR 未設定");
  await page.setViewportSize({ width: 1440, height: 900 });
  await page.emulateMedia({ colorScheme: "light" });
  await login(page);
  const cap = async (name: string) => {
    await page.waitForTimeout(500);
    await page.screenshot({ path: `${SHOTS}/L-${name}.png` });
  };

  // ホーム / ドライブ。
  await page.goto("/");
  await page.waitForLoadState("networkidle");
  await cap("01-home");
  await page.goto("/drive");
  await page.waitForLoadState("networkidle");
  await cap("02-drive");

  // 会話（/c）: 送信 → 統一ヘッダ確認。
  await page.goto("/");
  await page.waitForLoadState("networkidle");
  const composer = page.getByRole("textbox").first();
  await composer.click();
  await composer.fill("ライト表示の確認用サンプルメッセージ");
  await page.keyboard.press("Enter");
  await page.waitForURL(/\/c\/[0-9a-f-]+/i, { timeout: 30_000 }).catch(() => {});
  await page.waitForTimeout(2500);
  await cap("03-chat");

  // ノート: ツールバー＋バブル＋分割。
  await page.goto("/drive");
  await page.waitForLoadState("networkidle");
  await page.getByRole("button", { name: "新規作成" }).click();
  await page.getByRole("menuitem", { name: "ノート" }).click();
  await page.waitForURL(/\/notes\/[0-9a-f-]+/i, { timeout: 60_000 });
  await page.getByTestId("note-editor").waitFor({ timeout: 60_000 });
  await page.waitForTimeout(1200);
  const editor = page.getByTestId("note-editor");
  await editor.click();
  await page.keyboard.type("プロジェクト計画", { delay: 6 });
  await page.keyboard.press("Enter");
  await page.keyboard.type("これはツールバーとバブルメニューの確認用テキストです。", { delay: 3 });
  await cap("04-note-toolbar");
  await page.keyboard.press("Home");
  await page.keyboard.press("Shift+End");
  await cap("05-note-bubble");
  await page.getByTestId("note-chat-toggle").click();
  await page.waitForTimeout(1500);
  await cap("06-note-split");

  // CSV: グリッド＋SQL。
  await page.goto("/drive");
  await page.waitForLoadState("networkidle");
  await page.locator("text=/\\.csv/").first().click();
  await page.waitForURL(/\/csv\/[0-9a-f-]+/i, { timeout: 60_000 });
  await page.getByTestId("csv-grid").waitFor({ timeout: 60_000 });
  await cap("07-csv-grid");
  await page.getByRole("tab", { name: "SQL" }).click();
  await page.waitForTimeout(400);
  await page.getByTestId("sql-run").click();
  await page.waitForTimeout(1200);
  await cap("08-csv-sql");

  // ワークフロー: エディタ＋設定パネル。
  await page.goto("/workflows");
  await page.waitForLoadState("networkidle");
  await page.getByRole("button", { name: "新しいワークフロー", exact: true }).click();
  await page.waitForURL(/\/workflows\/[0-9a-f-]+/i, { timeout: 60_000 });
  await page.waitForTimeout(1500);
  await page.getByRole("button", { name: /最初のブロック/ }).click();
  const menu = page.getByTestId("add-node-menu");
  await menu.waitFor({ timeout: 10_000 });
  await menu.getByPlaceholder("ブロックを検索…").fill("AI に聞く");
  await page.waitForTimeout(400);
  await menu.getByRole("button").first().click();
  await page.waitForTimeout(1200);
  await page.locator(".react-flow__node").last().click({ force: true }).catch(() => {});
  await page.waitForTimeout(1000);
  await cap("09-wf-config");
});
