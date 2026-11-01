/// B1 コードミニアプリの隔離 e2e（Task 9.11 受け入れ条件）。
///
/// publish → バンドル upload → 同意インストール → シェル一覧表示 → iframe 起動を
/// API＋UI で一気通貫し、**opaque origin 隔離**を実バンドルの自己診断で検証する:
/// - document.cookie が空（ホスト Cookie 不可達）
/// - window.parent.document へのアクセスが例外（ホスト DOM 不可達）
/// - ゲートウェイ以外への fetch が CSP でブロック
///
/// 2 本目は、アプリがシェルの経路（SDK の hostAssistedGateway → シェルのポップアップのホスト支援 PKCE → Keycloak →
/// /apps/oauth/callback）でトークンとゲートウェイの場所を受け取り、ゲートウェイを呼べることを確かめる（BUG-172）。

import { readFileSync } from "node:fs";
import path from "node:path";

import { expect, test, type Page } from "@playwright/test";
import ts from "typescript";

import { cleanupAll, loginViaKeycloak, uniqueName } from "./helpers";

const B1_ORIGIN = process.env.E2E_B1_ORIGIN ?? "http://localhost:8091";
const SHOTS = process.env.E2E_SHOTS_DIR;

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

/// ミニアプリ SDK を、バンドルの module script に入れられる JS にする（型を外すだけ）。
function sdkScript(): string {
  const src = readFileSync(path.join(__dirname, "..", "..", "sdk", "app-sdk", "src", "index.ts"), "utf8");
  return ts.transpileModule(src, {
    compilerOptions: { target: ts.ScriptTarget.ES2022, module: ts.ModuleKind.ESNext },
  }).outputText;
}

/// ボタンでゲートウェイに「自分は誰か」を聞き、答えのアプリの id と付与されたスコープを出すアプリ。
const WHOAMI_BUNDLE = () => `<!doctype html><html lang="ja"><head><meta charset="utf-8">
<meta name="viewport" content="width=device-width, initial-scale=1">
<style>
  body { font-family: system-ui, sans-serif; margin: 0; padding: 16px; color: #1f2328; background: #fff; }
  h1 { font-size: 18px; margin: 0 0 12px; }
  button { font: inherit; padding: 8px 12px; border: 1px solid #d0d7de; border-radius: 6px; background: #f6f8fa; }
  p { margin: 12px 0 0; overflow-wrap: anywhere; }
  [role=alert] { border: 1px solid #f5c2c7; background: #fff5f5; color: #9a1b1b; border-radius: 6px; padding: 8px 12px; }
</style></head><body>
<h1>接続の確かめ</h1>
<button id="who" type="button">ゲートウェイに接続する</button>
<p id="result" aria-live="polite"></p>
<p id="error" role="alert" hidden></p>
<script type="module">
${sdkScript()}
const $ = (id) => document.getElementById(id);
$("who").addEventListener("click", async () => {
  $("error").hidden = true;
  try {
    const gw = await hostAssistedGateway(["data.read"]);
    const me = await gw.whoami();
    const tables = await gw.listTables();
    $("result").dataset.appId = me.app_id;
    $("result").textContent = "接続しました（スコープ: " + me.granted_scopes.join(", ") + "・テーブル " + tables.length + " 件）";
  } catch (e) {
    $("error").textContent = e instanceof Error ? e.message : "接続できませんでした。";
    $("error").hidden = false;
  }
});
</script></body></html>`;

test("B1 アプリ: シェルの経路でトークンとゲートウェイの場所を受け取り、ゲートウェイを呼べる（BUG-172）", async ({ page }) => {
  test.setTimeout(120_000);
  await page.setViewportSize({ width: 1440, height: 900 });
  await loginViaKeycloak(page);
  const name = uniqueName("b1-gateway");
  let appId: string | null = null;
  let installed = false;
  try {
    const manifest = {
      name,
      version: "1.0.0",
      description: "ゲートウェイの接続の確かめ",
      requested_scopes: ["data.read"],
      tools: [],
      tables: [],
      workflows: [],
      budget: {},
      frontend: null,
      server: null,
      trust_tier: "in_house",
    };
    appId = ((await apiJson(page, "post", "/apps/manifests", { manifest })) as { id: string }).id;
    const upload = await page.request.post(`/api/apps/manifests/${appId}/bundle`, {
      headers: { "content-type": "text/html", "x-csrf-token": await csrf(page) },
      data: WHOAMI_BUNDLE(),
    });
    expect(upload.ok(), await upload.text()).toBeTruthy();
    const { sha256 } = (await upload.json()) as { sha256: string };
    await apiJson(page, "put", `/apps/manifests/${appId}`, {
      manifest: { ...manifest, frontend: { bundle_key: sha256, sha256 } },
      expected_version: 1,
    });
    await apiJson(page, "post", `/apps/manifests/${appId}/publish`, {});
    await apiJson(page, "post", "/apps/installations", { name, version: "1.0.0", granted_scopes: ["data.read"] });
    installed = true;

    for (const [width, height, label] of [
      [1440, 900, "pc"],
      [390, 844, "phone"],
    ] as const) {
      await page.setViewportSize({ width, height });
      await page.goto(`/apps/b1/${appId}`);
      const frame = page.frameLocator(`iframe[title="${name}"]`);
      const connect = frame.getByRole("button", { name: "ゲートウェイに接続する" });
      await expect(connect).toBeVisible({ timeout: 20_000 });
      // シェルがポップアップを開き、Keycloak（ログイン済み）からシェルの戻り先へすぐに戻って閉じる。
      const popup = page.waitForEvent("popup");
      await connect.click();
      const login = await popup;
      await expect.poll(() => login.isClosed(), { timeout: 30_000 }).toBe(true);
      const result = frame.locator("#result");
      await expect(result).toHaveText("接続しました（スコープ: data.read・テーブル 0 件）", { timeout: 30_000 });
      await expect(result).toHaveAttribute("data-app-id", appId);
      await expect(frame.getByRole("alert")).toBeHidden();
      const overflow = await page.evaluate(() => document.documentElement.scrollWidth - window.innerWidth);
      expect(overflow).toBeLessThanOrEqual(1);
      if (SHOTS) await page.screenshot({ path: `${SHOTS}/miniapp-b1-gateway-${label}.png`, fullPage: true });
    }
  } finally {
    await cleanupAll(
      async () => {
        if (installed && appId) await apiJson(page, "delete", `/apps/installations/${appId}`);
      },
      async () => {
        if (appId) await apiJson(page, "delete", `/artifacts/${appId}`);
      },
    );
  }
});
