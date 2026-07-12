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

test("CSV エディタ グリッド/SQL", async ({ page }) => {
  test.skip(!SHOTS, "SHOTS_DIR 未設定");
  await page.setViewportSize({ width: 1440, height: 900 });
  await login(page);

  await page.goto("/drive");
  await page.waitForLoadState("networkidle");
  // 最初の .csv を開く。
  await page.locator("text=/\\.csv/").first().click();
  await page.waitForURL(/\/csv\/[0-9a-f-]+/i, { timeout: 60_000 });
  await page.getByTestId("csv-grid").waitFor({ timeout: 60_000 });
  await page.waitForTimeout(1500);
  await shot(page, "10-csv-grid");

  // SQL タブへ切替 → 実行。
  await page.getByRole("tab", { name: "SQL" }).click();
  await page.waitForTimeout(500);
  await page.getByTestId("sql-run").click();
  await page.waitForTimeout(1500);
  await shot(page, "11-csv-sql");
});
