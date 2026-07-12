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
  await page.waitForTimeout(300);
  await page.screenshot({ path: `${SHOTS}/${name}-light.png` });
  await page.emulateMedia({ colorScheme: "dark" });
  await page.waitForTimeout(400);
  await page.screenshot({ path: `${SHOTS}/${name}-dark.png` });
  await page.emulateMedia({ colorScheme: "light" });
}

test("ワークフロー エディタ", async ({ page }) => {
  test.skip(!SHOTS, "SHOTS_DIR 未設定");
  await page.setViewportSize({ width: 1440, height: 900 });
  await login(page);

  await page.goto("/workflows");
  await page.waitForLoadState("networkidle");
  await page.getByRole("button", { name: "新しいワークフロー", exact: true }).click();
  await page.waitForURL(/\/workflows\/[0-9a-f-]+/i, { timeout: 60_000 });
  await page.waitForTimeout(2000);

  // 最初のブロックを追加（ヘッダの「最初のブロック」→ AddNodeMenu）。
  await page.getByRole("button", { name: /最初のブロック/ }).click();
  const menu = page.getByTestId("add-node-menu");
  await menu.waitFor({ timeout: 10_000 });
  await menu.getByPlaceholder("ブロックを検索…").fill("AI に聞く");
  await page.waitForTimeout(400);
  await menu.getByRole("button").first().click();
  await page.waitForTimeout(1500);
  await shot(page, "12-wf-editor");

  // ノードをクリックして設定パネル（フロート）を開く。
  await page.locator(".react-flow__node").last().click({ force: true }).catch(() => {});
  await page.waitForTimeout(1200);
  await shot(page, "13-wf-config");
});
