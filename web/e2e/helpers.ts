import { expect, type Page } from "@playwright/test";

/// compose のテストユーザー（deploy/keycloak/shiki-realm.json の alice/password）。
export const USER = process.env.E2E_USER ?? "alice";
export const PASS = process.env.E2E_PASS ?? "password";

/// Keycloak のログインフォーム（Shiki テーマ）に資格情報を入力して送信する。
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
  // /login は未ログインならそのまま Keycloak のログイン画面へ送る。
  await page.goto("/login");
  await submitKeycloakLogin(page, user, pass);
  // ログイン完了＝/login 以外のアプリ画面に着地（オリジン非依存）。
  await page.waitForURL((url) => !url.pathname.startsWith("/login"), { timeout: 20_000 });
  await expect(page.getByRole("button", { name: "アカウントメニューを開く" })).toBeVisible();
}

/// テスト用に一意な名前を作る（並行・再実行での衝突回避）。
export function uniqueName(prefix: string): string {
  return `${prefix}-${Date.now().toString(36)}-${Math.floor(Math.random() * 1e4)}`;
}

/// 操作ログの表示（設定 → チャット「操作ログの表示」）を変え、元の値に戻す関数を返す。
///
/// 既定の「シンプル」はツールごとの行（「ドライブで「…」を検索しました」など）をチャットに出さないので、
/// その行を確かめる spec は「詳細」にしてから送る。設定はアカウント単位なので、終わったら必ず戻す
/// （ほかの spec の表示に残さない）。
export async function setActivityDetail(
  page: Page,
  detail: "simple" | "detailed",
): Promise<() => Promise<void>> {
  const headers = {
    "X-CSRF-Token": (await page.context().cookies()).find((c) => c.name === "shiki_csrf")?.value ?? "",
  };
  const current = await page.request.get("/api/me/preferences");
  expect(current.status(), await current.text()).toBe(200);
  const original = ((await current.json()) as { preferences: Record<string, unknown> }).preferences;
  const put = async (preferences: Record<string, unknown>) => {
    const res = await page.request.put("/api/me/preferences", { headers, data: preferences });
    expect(res.status(), await res.text()).toBe(200);
  };
  await put({ ...original, activity_detail: detail });
  return () => put(original);
}
