/// アプリ（コードのアプリ・B1）の AI の誤りの見え方（D-117・BUG-140）。
///
/// 組織がモデルの選択を禁じているとき、既定と違うモデルを指定したアプリの AI の呼び出しは、生成せずに断られる。
/// アプリにはミニアプリ SDK（sdk/app-sdk）の GatewayError で、利用者に見せてよい文言と理由（code）が届く。モデルを
/// 指定しなければ組織の既定のモデルで動く（文言の案内のとおり）。PC とスマホの幅で、アプリが出した文言を目で確かめる。
///
/// アプリのバンドルには、SDK のソースを型だけ外して埋め込む（SDK の誤りの読み方をそのまま通す）。
///
/// トークンは、シェルのホスト支援 PKCE（SDK の hostAssistedToken）ではなく、テストが同じアプリのクライアントで取って
/// アプリに渡す。いまの B1 のクライアントの登録には 2 つの不具合があり（別に起票する）、シェルの経路ではトークンが届かない:
/// - インストールが登録する戻り先（BFF の /auth/callback）と、シェルのポップアップの戻り先（/apps/oauth/callback）が
///   食い違い、Keycloak がシェルの要求を「無効なパラメーター: redirect_uri」で断る。
/// - クライアントに既定のスコープ（basic）も、ログインのクライアント（shiki-web）にある tenant・groups・roles の
///   マッパーも付かない。トークンに sub と tenant が無いので、ゲートウェイが 401 で断る。
/// テストは Keycloak の管理 API でこれらを足し（[`prepareB1Client`]）、登録された戻り先でコードを受け取る。ゲートウェイ・
/// B1 の配信・SDK・誤りの文言は本物を通す。
///
/// 前提: dev-up のゲートウェイ（第2リスナ）と B1 配信（第3リスナ）・LLM は stub。実 LLM は呼ばない（断られる呼び出しも
/// 通る呼び出しも stub のモデルを指し、組織の既定はおまかせ（Auto）で stub のモデルへ振る）。

import { createHash, randomBytes } from "node:crypto";
import { readFileSync } from "node:fs";
import path from "node:path";

import { expect, request, test, type Page } from "@playwright/test";
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
  if (gateway) return gateway;
  // テストが渡したトークンとゲートウェイの場所（シェルのホスト支援 PKCE の代わり・冒頭の注記）。
  const { token, gatewayOrigin } = window.__shikiTest;
  gateway = new ShikiGateway({ baseUrl: gatewayOrigin, getToken: () => token });
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

const base64url = (b: Buffer) => b.toString("base64").replace(/\+/g, "-").replace(/\//g, "_").replace(/=+$/, "");

/// Keycloak の realm の URL（BFF のログインの入口が返す行き先から読む・環境で違う）。
async function realmUrl(page: Page): Promise<string> {
  const bare = await request.newContext({ baseURL: new URL(page.url()).origin });
  const login = await bare.get("/auth/login", { maxRedirects: 0 });
  await bare.dispose();
  const href = login.headers()["location"] ?? "";
  expect(href, "BFF のログインの入口が Keycloak へ送らない").toContain("/protocol/openid-connect/auth");
  return href.slice(0, href.indexOf("/protocol/openid-connect/auth"));
}

/// B1 のクライアントに、トークンの sub を載せる既定のスコープ（basic）と、ログインのクライアントと同じ tenant・groups・
/// roles のマッパーを足し、登録された戻り先を返す（冒頭の注記の不具合の回避・既にあれば何もしない）。管理 API は、
/// インストールがクライアントを登録するのと同じ資格情報で呼ぶ。
async function prepareB1Client(realm: string, clientId: string): Promise<string> {
  const adminBase = realm.replace("/realms/", "/admin/realms/");
  const http = await request.newContext();
  try {
    const login = await http.post(`${realm}/protocol/openid-connect/token`, {
      form: {
        grant_type: "client_credentials",
        client_id: process.env.E2E_KC_PROVISIONER_ID ?? "shiki-provisioner",
        client_secret: process.env.E2E_KC_PROVISIONER_SECRET ?? "shiki-provisioner-dev-secret",
      },
    });
    expect(login.ok(), await login.text()).toBeTruthy();
    const headers = { authorization: `Bearer ${((await login.json()) as { access_token: string }).access_token}` };
    const found = (await (
      await http.get(`${adminBase}/clients?clientId=${encodeURIComponent(clientId)}`, { headers })
    ).json()) as { id: string; redirectUris: string[]; defaultClientScopes?: string[] }[];
    expect(found.length, `Keycloak に ${clientId} が無い`).toBe(1);
    const client = found[0];
    if (!(client.defaultClientScopes ?? []).includes("basic")) {
      const scopes = (await (await http.get(`${adminBase}/client-scopes`, { headers })).json()) as {
        id: string;
        name: string;
      }[];
      const basic = scopes.find((c) => c.name === "basic");
      expect(basic, "realm に basic のスコープが無い").toBeDefined();
      const put = await http.put(`${adminBase}/clients/${client.id}/default-client-scopes/${basic!.id}`, { headers });
      expect(put.ok(), await put.text()).toBeTruthy();
    }
    type Mapper = { name: string; protocol: string; protocolMapper: string; config: Record<string, string> };
    const mappers = async (id: string) =>
      (await (await http.get(`${adminBase}/clients/${id}/protocol-mappers/models`, { headers })).json()) as Mapper[];
    const web = (await (await http.get(`${adminBase}/clients?clientId=shiki-web`, { headers })).json()) as {
      id: string;
    }[];
    expect(web.length, "Keycloak に shiki-web が無い").toBe(1);
    const have = new Set((await mappers(client.id)).map((m) => m.name));
    for (const m of await mappers(web[0].id)) {
      if (!["tenant", "groups", "roles"].includes(m.name) || have.has(m.name)) continue;
      const add = await http.post(`${adminBase}/clients/${client.id}/protocol-mappers/models`, {
        headers,
        data: { name: m.name, protocol: m.protocol, protocolMapper: m.protocolMapper, config: m.config },
      });
      expect(add.ok(), await add.text()).toBeTruthy();
    }
    expect(client.redirectUris.length, "戻り先が登録されていない").toBeGreaterThan(0);
    return client.redirectUris[0];
  } finally {
    await http.dispose();
  }
}

/// アプリのクライアント（B1・public＋PKCE）で、ログイン中の利用者のゲートウェイのトークンを取る。
///
/// 認可の要求はブラウザーの文脈の Cookie（Keycloak のログイン）で送り、戻り先へは進まずにコードだけを受け取る。
async function gatewayToken(page: Page, clientId: string): Promise<string> {
  const issuer = await realmUrl(page);
  const redirectUri = await prepareB1Client(issuer, clientId);
  const verifier = base64url(randomBytes(48));
  const challenge = base64url(createHash("sha256").update(verifier).digest());
  const auth = new URL(`${issuer}/protocol/openid-connect/auth`);
  for (const [k, v] of Object.entries({
    client_id: clientId,
    response_type: "code",
    redirect_uri: redirectUri,
    scope: "openid llm.invoke",
    state: base64url(randomBytes(16)),
    code_challenge: challenge,
    code_challenge_method: "S256",
  }))
    auth.searchParams.set(k, v);
  const authorized = await page.request.get(auth.href, { maxRedirects: 0 });
  const back = new URL(authorized.headers()["location"] ?? "", redirectUri);
  const code = back.searchParams.get("code");
  expect(code, `認可コードが無い: ${authorized.status()} ${back.href}`).toBeTruthy();
  const token = await page.request.post(`${issuer}/protocol/openid-connect/token`, {
    form: { grant_type: "authorization_code", client_id: clientId, code: code!, redirect_uri: redirectUri, code_verifier: verifier },
  });
  expect(token.ok(), await token.text()).toBeTruthy();
  return ((await token.json()) as { access_token: string }).access_token;
}

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
    const token = await gatewayToken(page, installation!.client_id_b1!);

    await page.goto(`/apps/b1/${appId}`);
    const frame = page.frameLocator(`iframe[title="${name}"]`);
    await expect(frame.getByRole("button", { name: "モデルを指定して要約する" })).toBeVisible({ timeout: 20_000 });
    const app = page.frames().find((f) => f.url().startsWith(B1_ORIGIN));
    expect(app, "B1 の配信オリジンから開いていない").toBeDefined();
    // ゲートウェイの場所は、配信の CSP が通信を許す先（connect-src）と同じ。
    const csp = (await page.request.get(app!.url())).headers()["content-security-policy"] ?? "";
    const gatewayOrigin = /connect-src ([^;\s]+)/.exec(csp)?.[1];
    expect(gatewayOrigin, csp).toBeTruthy();
    await app!.evaluate((v) => Object.assign(window, { __shikiTest: v }), { token, gatewayOrigin });

    // モデルを指定すると断られ、アプリには理由（code）と、利用者に見せてよい文言が届く。
    await frame.getByRole("button", { name: "モデルを指定して要約する" }).click();
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
