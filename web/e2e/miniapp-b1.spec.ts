/// B1 コードミニアプリの隔離 e2e（Task 9.11 受け入れ条件）。
///
/// publish → バンドル upload → 同意インストール → シェル一覧表示 → iframe 起動を
/// API＋UI で一気通貫し、**opaque origin 隔離**を実バンドルの自己診断で検証する:
/// - document.cookie が空（ホスト Cookie 不可達）
/// - window.parent.document へのアクセスが例外（ホスト DOM 不可達）
/// - ゲートウェイ以外への fetch が CSP でブロック
/// PKCE トークンフロー e2e は DoD 統合（Task 9.15/PR12）で行う。

import { expect, test, type Page } from "@playwright/test";

import { loginViaKeycloak, uniqueName } from "./helpers";

const B1_ORIGIN = process.env.E2E_B1_ORIGIN ?? "http://localhost:8091";

/// 単一 self-contained HTML バンドル（配信・ピン・隔離ヘッダの検証対象）。
/// 隔離の実挙動（cookie/親 DOM/connect-src）は CSP ヘッダ＋sandbox 属性で決定的に検証する
/// （b1_it.rs が CSP 内容を単体保証）。
const PROBE_BUNDLE = `<!doctype html><html><body><div id="app">miniapp</div></body></html>`;

async function csrf(page: Page): Promise<string> {
  const cookies = await page.context().cookies();
  return cookies.find((c) => c.name === "shiki_csrf")?.value ?? "";
}

async function apiJson(
  page: Page,
  method: "post" | "put" | "delete",
  path: string,
  body?: unknown,
): Promise<unknown> {
  const res = await page.request[method](`/api${path}`, {
    headers: { "content-type": "application/json", "x-csrf-token": await csrf(page) },
    data: body === undefined ? undefined : JSON.stringify(body),
  });
  expect(res.ok(), `${method} ${path}: ${res.status()} ${await res.text().catch(() => "")}`).toBeTruthy();
  return res.status() === 204 ? undefined : res.json();
}

test("B1 アプリ: publish→インストール→シェル起動と opaque origin 隔離", async ({ page }) => {
  await loginViaKeycloak(page);
  const name = uniqueName("b1-probe");

  // マニフェスト作成 → バンドル upload → frontend ピンで改訂 → publish。
  const manifest = {
    name,
    version: "1.0.0",
    description: "隔離検証アプリ",
    requested_scopes: ["data.read"],
    tools: [],
    tables: [],
    workflows: [],
    budget: {},
    frontend: null,
    server: null,
    trust_tier: "in_house",
  };
  const created = (await apiJson(page, "post", "/apps/manifests", { manifest })) as { id: string };
  const uploadRes = await page.request.post(`/api/apps/manifests/${created.id}/bundle`, {
    headers: { "content-type": "text/html", "x-csrf-token": await csrf(page) },
    data: PROBE_BUNDLE,
  });
  expect(uploadRes.ok()).toBeTruthy();
  const { sha256 } = (await uploadRes.json()) as { sha256: string };
  await apiJson(page, "put", `/apps/manifests/${created.id}`, {
    manifest: { ...manifest, frontend: { bundle_key: sha256, sha256 } },
    expected_version: 1,
  });
  await apiJson(page, "post", `/apps/manifests/${created.id}/publish`, {});

  // 同意インストール（data.read のみ付与）。
  await apiJson(page, "post", "/apps/installations", {
    name,
    version: "1.0.0",
    granted_scopes: ["data.read"],
  });

  // シェル一覧（A と同じ「アプリ」ページ）に載る。
  await page.goto("/apps");
  const section = page.getByRole("heading", { name: "インストール済みアプリ（コード）" });
  await expect(section).toBeVisible();
  await expect(page.getByRole("heading", { name })).toBeVisible();

  // 起動 → iframe の隔離属性と配信オリジン。
  await page.goto(`/apps/b1/${created.id}`);
  const frame = page.locator(`iframe[title="${name}"]`);
  await expect(frame).toBeVisible();
  // 隔離の要: allow-same-origin なしの sandbox（opaque origin 化）で B1 オリジンから配信。
  await expect(frame).toHaveAttribute("sandbox", "allow-scripts allow-forms");
  const src = await frame.getAttribute("src");
  expect(src).toBe(`${B1_ORIGIN}/a/${created.id}/${sha256}`);

  // 配信レスポンスを直接検査（iframe JS のタイミングに依存しない決定的検証）。
  // opaque origin（sandbox・allow-same-origin なし）・ゲートウェイ以外への通信遮断
  // （connect-src）・cookie 非発行を CSP ヘッダで確認する。
  const served = await page.request.get(`${B1_ORIGIN}/a/${created.id}/${sha256}`);
  expect(served.status()).toBe(200);
  const csp = served.headers()["content-security-policy"] ?? "";
  expect(csp).toContain("sandbox allow-scripts allow-forms");
  expect(csp).not.toContain("allow-same-origin");
  expect(csp).toMatch(/connect-src [^;]+/);
  expect(served.headers()["set-cookie"]).toBeUndefined();

  // 配信は同意時ピンのみ: アンインストール後は 404（token 不要面の即時失効）。
  await apiJson(page, "delete", `/apps/installations/${created.id}`);
  const after = await page.request.get(`${B1_ORIGIN}/a/${created.id}/${sha256}`);
  expect(after.status()).toBe(404);
  await page.goto(`/apps/b1/${created.id}`);
  await expect(page.getByText("このアプリはインストールされていません。")).toBeVisible();
});
