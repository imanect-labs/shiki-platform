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
  // ログイン完了＝Keycloak（/realms/）・BFF の往復（/auth/）・/login を抜けてアプリの画面に着地し（オリジン
  // 非依存）、そのセッションで /api/me が読める。画面の部品（アカウントメニュー）では確かめない: スマホ幅では
  // ドロワの中にあって見えない。
  await page.waitForURL((url) => !/^\/(login|auth\/|realms\/)/.test(url.pathname), { timeout: 20_000 });
  await expect.poll(async () => (await page.request.get("/api/me")).status(), { timeout: 10_000 }).toBe(200);
}

/// テスト用に一意な名前を作る（並行・再実行での衝突回避）。
export function uniqueName(prefix: string): string {
  return `${prefix}-${Date.now().toString(36)}-${Math.floor(Math.random() * 1e4)}`;
}

/// 実 LLM を呼ばない stub の決定器の判定用のモデル（カタログに無ければ undefined）。
///
/// 「元にする chat モデルを持たない判定用のモデル」だけでは選ばない: TypeSafe 互換の判定用のモデル（外部の API）も
/// `decision_via` を持たないので、振り先のプロバイダの種別が stub であることまで確かめる。
export async function stubDecisionModel<T extends { id: string; modality: string; provider_id: string; decision_via?: string | null }>(
  page: Page,
  models: T[],
): Promise<T | undefined> {
  const res = await page.request.get("/api/org-admin/ai/providers");
  expect(res.ok(), await res.text()).toBeTruthy();
  const stub = new Set(
    ((await res.json()) as { id: string; kind: string }[]).filter((p) => p.kind === "stub").map((p) => p.id),
  );
  return models.find((m) => m.modality === "decision" && !m.decision_via && stub.has(m.provider_id));
}
