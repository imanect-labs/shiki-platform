import { expect, type Page } from "@playwright/test";

/// compose のテストユーザー（deploy/keycloak/shiki-realm.json の alice/password）。
export const USER = process.env.E2E_USER ?? "alice";
export const PASS = process.env.E2E_PASS ?? "password";

/// Keycloak のログインフォームに資格情報を入力して送信する。
export async function submitKeycloakLogin(page: Page) {
  await page.waitForURL(/:8081\/realms\/shiki\/protocol\/openid-connect\/auth/);
  await page.locator("#username").fill(USER);
  await page.locator("#password").fill(PASS);
  await page.locator("#kc-login").click();
}

/// /login からログインを完了し、アプリ（/login 以外）へ戻るまで待つ。
export async function loginViaKeycloak(page: Page) {
  await page.goto("/login");
  await page.getByRole("button", { name: "Keycloak でログイン" }).click();
  await submitKeycloakLogin(page);
  await page.waitForURL(/localhost:3000\/(?!login)/, { timeout: 20_000 });
  await expect(page.getByRole("button", { name: "アカウントメニューを開く" })).toBeVisible();
}
