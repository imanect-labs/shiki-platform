/// アプリ（コードのアプリ・B1）の AI の誤りの見え方（D-117・BUG-140）。
///
/// 組織がモデルの選択を禁じているとき、既定と違うモデルを指定したアプリの AI の呼び出しは、生成せずに断られる。
/// アプリにはミニアプリ SDK（sdk/app-sdk）の GatewayError で、利用者に見せてよい文言と理由（code）が届く。モデルを
/// 指定しなければ組織の既定のモデルで動く（文言の案内のとおり）。PC とスマホの幅で、アプリが出した文言を目で確かめる。
///
/// アプリのバンドルには、SDK のソースを型だけ外して埋め込む（SDK の誤りの読み方をそのまま通す）。
///
/// トークンとゲートウェイの場所は、本物のシェルの経路で受け取る（SDK の hostAssistedGateway → シェルのポップアップの
/// ホスト支援 PKCE → Keycloak → /apps/oauth/callback）。BUG-172 の間は、Keycloak の管理 API でアプリのクライアントの
/// 戻り先とスコープを補い、テストがトークンを取って渡していた。インストールが正しく登録するようになったので、補いは
/// 消した。
///
/// 前提: dev-up のゲートウェイ（第2リスナ）と B1 配信（第3リスナ）・Keycloak・LLM は stub。実 LLM は呼ばない（断られる
/// 呼び出しも通る呼び出しも stub のモデルを指し、組織の既定はおまかせ（Auto）で stub のモデルへ振る）。

import { readFileSync } from "node:fs";
import path from "node:path";

import { expect, test, type Page } from "@playwright/test";
import ts from "typescript";

import { cleanupAll, loginViaKeycloak, stubChatModelId, uniqueName } from "./helpers";

const B1_ORIGIN = process.env.E2E_B1_ORIGIN ?? "http://localhost:8091";
const SHOTS = process.env.E2E_SHOTS_DIR;

async function csrf(page: Page): Promise<string> {
  return (await page.context().cookies()).find((c) => c.name === "shiki_csrf")?.value ?? "";
}

async function api(page: Page, method: "get" | "post" | "put" | "delete", p: string, body?: unknown) {
  const res = await page.request[method](`/api${p}`, {
    headers: { "content-type": "application/json", "x-csrf-token": await csrf(page) },
    data: body === undefined ? undefined : JSON.stringify(body),
  });
  expect(res.ok(), `${method} ${p}: ${res.status()} ${await res.text().catch(() => "")}`).toBeTruthy();
  return res.status() === 204 ? undefined : res.json();
}

/// ミニアプリ SDK を、バンドルの module script に入れられる JS にする（型を外すだけ）。
function sdkScript(): string {
  const src = readFileSync(path.join(__dirname, "..", "..", "sdk", "app-sdk", "src", "index.ts"), "utf8");
  return ts.transpileModule(src, {
    compilerOptions: { target: ts.ScriptTarget.ES2022, module: ts.ModuleKind.ESNext },
  }).outputText;
}

/// 試しのアプリの画面（1 枚の HTML）。ボタンで AI に聞き、断られたら SDK の GatewayError の文言を出す。
function bundle(model: string): string {
  return `<!doctype html><html lang="ja"><head><meta charset="utf-8">
<meta name="viewport" content="width=device-width, initial-scale=1">
<style>
  body { font-family: system-ui, sans-serif; margin: 0; padding: 16px; color: #1f2328; background: #fff; }
  h1 { font-size: 18px; margin: 0 0 12px; }
  .row { display: flex; flex-wrap: wrap; gap: 8px; margin-bottom: 12px; }
  button { font: inherit; padding: 8px 12px; border: 1px solid #d0d7de; border-radius: 6px; background: #f6f8fa; }
  [role=alert] { border: 1px solid #f5c2c7; background: #fff5f5; color: #9a1b1b; border-radius: 6px; padding: 8px 12px; }
  #answer { border: 1px solid #d0d7de; border-radius: 6px; padding: 8px 12px; }
  p { margin: 0 0 8px; overflow-wrap: anywhere; }
</style></head><body>
<h1>経費の要約（AI）</h1>
<div class="row">
  <button id="pick" type="button">モデルを指定して要約する</button>
  <button id="omit" type="button">組織の既定のモデルで要約する</button>
</div>
<p id="status" aria-live="polite"></p>
<p id="error" role="alert" hidden></p>
<p id="answer" hidden></p>
<script type="module">
${sdkScript()}
const $ = (id) => document.getElementById(id);
let gateway = null;
async function client() {
  // トークンとゲートウェイの場所は、シェルのホスト支援 PKCE から受け取る（トークンは期限まで使い回す）。
  gateway ??= hostAssistedGateway(["llm.invoke"]).catch((e) => {
    gateway = null;
    throw e;
  });
  return gateway;
}
async function ask(model) {
  $("error").hidden = true;
  $("answer").hidden = true;
  $("status").textContent = "要約しています…";
  let text = "";
  try {
    const gw = await client();
    const request = { messages: [{ role: "user", content: [{ type: "text", text: "交通費 1,200 円の申請を要約してください" }] }] };
    if (model) request.model = model;
    await gw.invokeLlm(request, (type, data) => {
      if (type === "text" && data && typeof data.text === "string") text += data.text;
    });
    $("answer").textContent = text;
    $("answer").hidden = false;
  } catch (e) {
    $("error").textContent = e instanceof GatewayError ? e.message : "要約できませんでした。";
    $("error").dataset.code = (e instanceof GatewayError && e.code) || "";
    $("error").hidden = false;
  } finally {
    $("status").textContent = "";
  }
}
$("pick").addEventListener("click", () => ask(${JSON.stringify(model)}));
$("omit").addEventListener("click", () => ask(null));
</script></body></html>`;
}

type AiSettings = { default_model: string; allow_user_choice: boolean; auto_model: string | null };

test("アプリの AI: 組織がモデルの選択を禁じていると、指定したモデルは断られ、SDK で文言と理由が届く（D-117）", async ({
  page,
}) => {
  test.setTimeout(180_000);
  await page.setViewportSize({ width: 1440, height: 900 });
  await loginViaKeycloak(page);
  const stub = await stubChatModelId(page);
  const before = (await api(page, "get", "/org-admin/ai/settings")) as AiSettings;
  const name = uniqueName("ai-lock");
  let appId: string | null = null;
  let installed = false;
  try {
    // 組織の既定はおまかせ（Auto・振り先は stub）にし、メンバーのモデルの選択を禁じる。
    await api(page, "put", "/org-admin/ai/settings", {
      default_model: "auto",
      auto_model: stub,
      allow_user_choice: false,
    });

    // AI を使うアプリ（B1）を作り、llm.invoke を渡してインストールする。
    const manifest = {
      name,
      version: "1.0.0",
      description: "AI の誤りの見え方の確かめ",
      requested_scopes: ["llm.invoke"],
      tools: [],
      tables: [],
      workflows: [],
      budget: {},
      frontend: null,
      server: null,
      trust_tier: "in_house",
    };
    appId = ((await api(page, "post", "/apps/manifests", { manifest })) as { id: string }).id;
    const upload = await page.request.post(`/api/apps/manifests/${appId}/bundle`, {
      headers: { "content-type": "text/html", "x-csrf-token": await csrf(page) },
      data: bundle(stub),
    });
    expect(upload.ok(), await upload.text()).toBeTruthy();
    const { sha256 } = (await upload.json()) as { sha256: string };
    await api(page, "put", `/apps/manifests/${appId}`, {
      manifest: { ...manifest, frontend: { bundle_key: sha256, sha256 } },
      expected_version: 1,
    });
    await api(page, "post", `/apps/manifests/${appId}/publish`, {});
    await api(page, "post", "/apps/installations", { name, version: "1.0.0", granted_scopes: ["llm.invoke"] });
    installed = true;

    const installation = ((await api(page, "get", "/apps/installations")) as {
      items: { app_id: string; client_id_b1: string | null }[];
    }).items.find((i) => i.app_id === appId);
    expect(installation?.client_id_b1, "B1 のクライアントが登録されていない").toBeTruthy();

    await page.goto(`/apps/b1/${appId}`);
    const frame = page.frameLocator(`iframe[title="${name}"]`);
    await expect(frame.getByRole("button", { name: "モデルを指定して要約する" })).toBeVisible({ timeout: 20_000 });
    const app = page.frames().find((f) => f.url().startsWith(B1_ORIGIN));
    expect(app, "B1 の配信オリジンから開いていない").toBeDefined();

    // モデルを指定すると断られ、アプリには理由（code）と、利用者に見せてよい文言が届く。最初の呼び出しで、シェルが
    // ポップアップでトークンを取る（Keycloak にログイン済みなので、すぐに /apps/oauth/callback へ戻って閉じる）。
    const popups: Page[] = [];
    page.on("popup", (p) => popups.push(p));
    const popup = page.waitForEvent("popup");
    await frame.getByRole("button", { name: "モデルを指定して要約する" }).click();
    const login = await popup;
    await expect.poll(() => login.isClosed(), { timeout: 30_000 }).toBe(true);
    const alert = frame.getByRole("alert");
    await expect(alert).toHaveText(
      `この組織では AI のモデルを選べないため、「${stub}」は使えません。モデルを指定しないで呼び出すと、組織の既定のモデルで動きます。`,
      { timeout: 30_000 },
    );
    await expect(alert).toHaveAttribute("data-code", "model_choice_disabled");
    if (SHOTS) await page.screenshot({ path: `${SHOTS}/miniapp-ai-lock-pc.png`, fullPage: true });

    // 文言の案内のとおり、モデルを指定しなければ組織の既定（stub）で答える。
    await frame.getByRole("button", { name: "組織の既定のモデルで要約する" }).click();
    await expect(frame.locator("#answer")).toContainText("交通費", { timeout: 30_000 });
    await expect(alert).toBeHidden();

    // スマホの幅でも、文言が枠の中で折り返し、横にはみ出さない。
    await page.setViewportSize({ width: 390, height: 844 });
    await frame.getByRole("button", { name: "モデルを指定して要約する" }).click();
    await expect(alert).toHaveAttribute("data-code", "model_choice_disabled", { timeout: 30_000 });
    await expect(alert).toBeVisible();
    const overflow = await page.evaluate(() => document.documentElement.scrollWidth - window.innerWidth);
    expect(overflow).toBeLessThanOrEqual(1);
    const box = await alert.boundingBox();
    expect(box && box.x + box.width).toBeLessThanOrEqual(390);
    if (SHOTS) await page.screenshot({ path: `${SHOTS}/miniapp-ai-lock-phone.png`, fullPage: true });
    // トークンは期限まで使い回す（2 回目からの呼び出しでポップアップを開かない）。
    expect(popups).toHaveLength(1);
  } finally {
    await cleanupAll(
      () =>
        api(page, "put", "/org-admin/ai/settings", {
          default_model: before.default_model,
          auto_model: before.auto_model,
          allow_user_choice: before.allow_user_choice,
        }),
      async () => {
        if (installed && appId) await api(page, "delete", `/apps/installations/${appId}`);
      },
      async () => {
        if (appId) await api(page, "delete", `/artifacts/${appId}`);
      },
    );
  }
});
