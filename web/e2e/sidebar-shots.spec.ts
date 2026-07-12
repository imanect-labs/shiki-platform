import { test, expect, type Page } from "@playwright/test";

import { submitKeycloakLogin } from "./helpers";

const SHOTS = process.env.SHOTS_DIR ?? "";
test.describe.configure({ timeout: 180_000 });

async function login(page: Page) {
  await page.goto("/login");
  await page.getByRole("button", { name: "Keycloak でログイン" }).click();
  await submitKeycloakLogin(page);
  await page.waitForURL((url) => !url.pathname.startsWith("/login"), { timeout: 30_000 });
  await expect(page.getByRole("button", { name: "アカウントメニューを開く" })).toBeVisible({
    timeout: 60_000,
  });
}

test("サイドバー", async ({ page }) => {
  test.skip(!SHOTS, "SHOTS_DIR 未設定");
  await page.setViewportSize({ width: 1440, height: 900 });
  await login(page);
  await page.goto("/drive");
  await page.waitForLoadState("networkidle");
  await page.waitForTimeout(600);
  // サイドバーだけを切り出す（幅 300 くらい）。
  for (const scheme of ["light", "dark"] as const) {
    await page.emulateMedia({ colorScheme: scheme });
    await page.waitForTimeout(300);
    await page.screenshot({
      path: `${SHOTS}/sidebar-${scheme}.png`,
      clip: { x: 0, y: 0, width: 310, height: 900 },
    });
  }
  await page.emulateMedia({ colorScheme: "light" });
});
