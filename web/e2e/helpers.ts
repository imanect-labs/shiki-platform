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

/// 種別が stub のプロバイダの id（組織の管理者の API・alice で呼ぶ）。
async function stubProviderIds(page: Page): Promise<Set<string>> {
  const res = await page.request.get("/api/org-admin/ai/providers");
  expect(res.ok(), await res.text()).toBeTruthy();
  return new Set(
    ((await res.json()) as { id: string; kind: string }[]).filter((p) => p.kind === "stub").map((p) => p.id),
  );
}

/// 実 LLM を呼ばない stub の決定器の判定用のモデル（カタログに無ければ undefined）。
///
/// 「元にする chat モデルを持たない判定用のモデル」だけでは選ばない: TypeSafe 互換の判定用のモデル（外部の API）も
/// `decision_via` を持たないので、振り先のプロバイダの種別が stub であることまで確かめる。
export async function stubDecisionModel<T extends { id: string; modality: string; provider_id: string; decision_via?: string | null }>(
  page: Page,
  models: T[],
): Promise<T | undefined> {
  const stub = await stubProviderIds(page);
  return models.find((m) => m.modality === "decision" && !m.decision_via && stub.has(m.provider_id));
}

type AdminChatModel = {
  id: string;
  label: string;
  modality: string;
  provider_id: string;
  enabled: boolean;
  personal: boolean;
};
type AdminAiSettings = {
  default_model: string;
  auto_model: string | null;
  auto_target_model: string | null;
};

async function adminAi(page: Page): Promise<{ models: AdminChatModel[]; settings: AdminAiSettings }> {
  const res = await page.request.get("/api/org-admin/ai/models");
  expect(res.ok(), await res.text()).toBeTruthy();
  return (await res.json()) as { models: AdminChatModel[]; settings: AdminAiSettings };
}

/// 組織で使える stub の chat モデル（組織の管理者の API で引く）と、組織の AI 設定。
async function stubChatModels(page: Page): Promise<{ stub: AdminChatModel[]; settings: AdminAiSettings }> {
  const providers = await stubProviderIds(page);
  const { models, settings } = await adminAi(page);
  const stub = models.filter((m) => m.modality === "chat" && m.enabled && !m.personal && providers.has(m.provider_id));
  return { stub, settings };
}

/// 組織で使える stub の chat モデル（実 LLM を呼ばない・無ければ undefined・BUG-85）。
///
/// 手元の検証の設定（`llm-stub.toml`）は既定が stub でも、実 LLM（Haiku）を追加プロバイダとして並べ、一覧の先頭に
/// 出す。「先頭のモデル」を選ぶと実 LLM を呼ぶので、振り先のプロバイダの種別が stub のモデルを選ぶ。
export async function stubChatModel(page: Page): Promise<{ id: string; label: string } | undefined> {
  return (await stubChatModels(page)).stub[0];
}

/// 組織の既定（モデルを省略した呼び出し・Auto の振り先）が stub のモデルで動くようにする（実 LLM を呼ばない・BUG-85）。
///
/// 既に stub で動くなら何も変えない。変えたら元に戻す関数を返す（呼び出し側が finally で呼ぶ）。既定が Auto なら振り先の
/// 上書きを、特定のモデルなら既定そのものを stub にする。stub のモデルが組織で使えなければ落とす（黙って実 LLM を呼ばない）。
export async function pinStubOrgDefault(page: Page): Promise<() => Promise<void>> {
  const { stub, settings } = await stubChatModels(page);
  expect(stub.length, "組織で使える stub のモデルが無い（実 LLM を呼ばないよう止める）").toBeGreaterThan(0);
  const auto = settings.default_model === "auto";
  const running = auto ? settings.auto_target_model : settings.default_model;
  if (stub.some((m) => m.id === running)) return async () => {};
  const put = async (data: Record<string, unknown>) => {
    const csrf = (await page.context().cookies()).find((c) => c.name === "shiki_csrf")?.value ?? "";
    const res = await page.request.put("/api/org-admin/ai/settings", {
      headers: { "x-csrf-token": csrf, "content-type": "application/json" },
      data,
    });
    expect(res.ok(), await res.text()).toBeTruthy();
  };
  await put(auto ? { auto_model: stub[0].id } : { default_model: stub[0].id });
  return async () => put(auto ? { auto_model: settings.auto_model } : { default_model: settings.default_model });
}
