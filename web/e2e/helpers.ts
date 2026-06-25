import { expect, type Page } from "@playwright/test";

/// compose のテストユーザー（deploy/keycloak/shiki-realm.json の alice/password）。
export const USER = process.env.E2E_USER ?? "alice";
export const PASS = process.env.E2E_PASS ?? "password";

/// Keycloak のログインフォームに資格情報を入力して送信する。
export async function submitKeycloakLogin(page: Page, user = USER, pass = PASS) {
  await page.waitForURL(/:8081\/realms\/shiki\/protocol\/openid-connect\/auth/);
  await page.locator("#username").fill(user);
  await page.locator("#password").fill(pass);
  await page.locator("#kc-login").click();
}

/// /login からログインを完了し、アプリ（/login 以外）へ戻るまで待つ。
export async function loginViaKeycloak(page: Page) {
  await loginAs(page, USER, PASS);
}

/// 指定ユーザーでログインする（マルチユーザー検証用。bob/charlie 等）。
export async function loginAs(page: Page, user: string, pass = PASS) {
  await page.goto("/login");
  await page.getByRole("button", { name: "Keycloak でログイン" }).click();
  await submitKeycloakLogin(page, user, pass);
  // ログイン完了＝/login 以外のアプリ画面に着地（オリジン非依存）。
  await page.waitForURL((url) => !url.pathname.startsWith("/login"), { timeout: 20_000 });
  await expect(page.getByRole("button", { name: "アカウントメニューを開く" })).toBeVisible();
}

/// テスト用に一意な名前を作る（並行・再実行での衝突回避）。
export function uniqueName(prefix: string): string {
  return `${prefix}-${Date.now().toString(36)}-${Math.floor(Math.random() * 1e4)}`;
}
