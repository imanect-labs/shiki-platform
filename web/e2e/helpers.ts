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

/// 組織で使える stub の chat モデルの id（無ければ落とす・モデルの id を決め打ちしない・BUG-114）。
///
/// 手元の検証の設定は stub のモデルを `default`、CI の compose は `shiki-default` と呼ぶので、id を書かずにここで引く。
export async function stubChatModelId(page: Page): Promise<string> {
  const m = await stubChatModel(page);
  expect(m, "組織で使える stub のモデルが無い（実 LLM を呼ばないよう止める）").toBeDefined();
  return m!.id;
}

/// 組織の既定（モデルを省略した呼び出し・おまかせで使うモデル）が stub のモデルで動かないときの理由（動くなら
/// null・BUG-115）。チャットを送る E2E の多くは、組織の既定が stub であることを前提にしている。
export async function orgDefaultProblem(page: Page): Promise<string | null> {
  const { stub, settings } = await stubChatModels(page);
  const auto = settings.default_model === "auto";
  const running = auto ? settings.auto_target_model : settings.default_model;
  if (running && stub.some((m) => m.id === running)) return null;
  const what = auto ? "おまかせで使うモデル" : "組織の既定のモデル";
  return (
    `${USER} の組織の${what}が stub のモデルではありません（いま: ${running ?? "なし"}）。` +
    "チャットを送る E2E が実 LLM を呼ぶので、流す前に止めました。" +
    `管理コンソールの「AI」で${what}を stub のモデル（${stub.map((m) => m.id).join("、") || "なし"}）にしてから流してください。` +
    "実 LLM のまま流すと決めたときだけ、E2E_ALLOW_REAL_LLM_DEFAULT=1 を付けます。"
  );
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

/// テストで作ったものの片付けを積み、テストの後（`test.afterEach`）に、作った逆の順で互いに独立して全部走らせる
/// （失敗したテストでも走る・BUG-126）。テストが多く、作る処理を関数にまとめている spec で使う。
///
/// ```ts
/// const cleanup = cleanupQueue();
/// test.afterEach(() => cleanup.run());
/// // テストの中: cleanup.add(() => removeWorkflowApi(page, id));
/// ```
export function cleanupQueue(): { add: (step: () => Promise<unknown>) => void; run: () => Promise<void> } {
  let steps: (() => Promise<unknown>)[] = [];
  return {
    add(step) {
      steps.push(step);
    },
    async run() {
      const pending = steps.reverse();
      steps = [];
      await cleanupAll(...pending);
    },
  };
}

/// [`cleanupQueue`] が返す登録簿。
export type CleanupQueue = ReturnType<typeof cleanupQueue>;

/// 画面の操作で作られたもの（ブラウザーからの作成の応答）を、片付けの登録簿に積む（一覧の「新しいワークフロー」・
/// アプリの作成・会話の「アプリとして保存」・スキルの作成・テーブルの作成・BUG-126）。`page.request` で作ったものは
/// 画面の応答に出ないので、呼び出し側が積む。
export function trackCreatedResources(page: Page, queue: CleanupQueue): void {
  page.on("response", (res) => {
    if (res.request().method() !== "POST" || !res.ok()) return;
    const path = new URL(res.url()).pathname;
    const remove: ((id: string) => Promise<void>) | null =
      path === "/api/workflows"
        ? (id) => removeWorkflowApi(page, id)
        : path === "/api/mini-apps" || path === "/api/skills" || path.endsWith("/save-as-app")
          ? (id) => deleteArtifactApi(page, id)
          : path === "/api/data/tables"
            ? (id) => deleteTableApi(page, id)
            : null;
    if (!remove) return;
    void res
      .json()
      .then((body: { id?: string }) => {
        if (body.id) queue.add(() => remove(body.id!));
      })
      .catch(() => undefined);
  });
}

async function csrfOf(page: Page): Promise<string> {
  return (await page.context().cookies()).find((c) => c.name === "shiki_csrf")?.value ?? "";
}

/// アーティファクト（ワークフロー・状態の進め方・アプリ・スキル・ビュー）を消す（もう無ければ何もしない）。
export async function deleteArtifactApi(page: Page, id: string): Promise<void> {
  const res = await page.request.delete(`/api/artifacts/${id}`, { headers: { "x-csrf-token": await csrfOf(page) } });
  expect([204, 404], `DELETE /artifacts/${id} → ${res.status()}`).toContain(res.status());
}

/// テーブルを消す（もう無ければ何もしない）。
export async function deleteTableApi(page: Page, id: string): Promise<void> {
  const res = await page.request.delete(`/api/data/tables/${id}`, { headers: { "x-csrf-token": await csrfOf(page) } });
  expect([204, 404], `DELETE /data/tables/${id} → ${res.status()}`).toContain(res.status());
}

/// ワークフローの動いている実行（順番待ち・実行中・承認や変更を待っている実行）を止める（承認の依頼を「承認」の
/// 画面に残さない・BUG-126）。ワークフローがもう無ければ何もしない。
export async function cancelActiveRunsApi(page: Page, workflowId: string): Promise<void> {
  const res = await page.request.get(`/api/workflows/${workflowId}/runs?status=queued,running&limit=100`);
  if (res.status() === 404) return;
  expect(res.ok(), await res.text()).toBeTruthy();
  const { items } = (await res.json()) as { items: { run_id: string }[] };
  const headers = { "x-csrf-token": await csrfOf(page), "content-type": "application/json" };
  for (const run of items) {
    const c = await page.request.post(`/api/workflows/${workflowId}/runs/${run.run_id}/cancel`, { headers, data: {} });
    expect([200, 404], `cancel ${run.run_id} → ${c.status()}`).toContain(c.status());
  }
}

/// ドライブのファイルをゴミ箱へ移す（もう無ければ何もしない）。
export async function trashFileApi(page: Page, id: string): Promise<void> {
  const res = await page.request.delete(`/api/files/${id}`, { headers: { "x-csrf-token": await csrfOf(page) } });
  expect([204, 404], `DELETE /files/${id} → ${res.status()}`).toContain(res.status());
}

/// ワークフローを片付ける: 動いている実行を止めてから消す（消すと自動実行・きっかけも止まる・BUG-128）。
export async function removeWorkflowApi(page: Page, id: string): Promise<void> {
  await cleanupAll(
    () => cancelActiveRunsApi(page, id),
    () => deleteArtifactApi(page, id),
  );
}

/// 後片付けを互いに独立して全部走らせる（どれかが投げても残りを走らせ、最初の誤りを最後に投げる）。
export async function cleanupAll(...steps: (() => Promise<unknown>)[]): Promise<void> {
  const errors: unknown[] = [];
  for (const step of steps) {
    try {
      await step();
    } catch (e) {
      errors.push(e);
    }
  }
  if (errors.length > 0) throw errors[0];
}

const FGA_URL = process.env.E2E_OPENFGA_URL ?? "http://localhost:8082";
const FGA_STORE = process.env.E2E_OPENFGA_STORE ?? "shiki";

/// サーバと同じ OpenFGA の store の id。
async function fgaStore(page: Page): Promise<string> {
  let storeId: string | null = null;
  let token = "";
  // store の名前の絞り込みが効かない版でも見つかるよう、全ページを見る（authz の find_store と同じ）。
  for (;;) {
    const q = new URLSearchParams({ name: FGA_STORE, page_size: "100" });
    if (token) q.set("continuation_token", token);
    const res = await page.request.get(`${FGA_URL}/stores?${q}`);
    expect(res.ok(), await res.text()).toBeTruthy();
    const body = (await res.json()) as { stores: { id: string; name: string }[]; continuation_token?: string };
    storeId = body.stores.find((s) => s.name === FGA_STORE)?.id ?? null;
    if (storeId || !body.continuation_token) break;
    token = body.continuation_token;
  }
  expect(storeId, `OpenFGA の store ${FGA_STORE} が見つかりません`).not.toBeNull();
  return storeId!;
}

/// テーブルを `userId` の editor にし、外す関数を返す。テーブルの共有はまだ画面にも API にも無い（作った人だけが
/// owner になる）ので、サーバと同じ OpenFGA の store にタプルを直接書く（名前は authz の Namespace と同じ
/// `<型>:<tenant>|<id>`）。
export async function grantTableEditor(
  page: Page,
  tenant: string,
  userId: string,
  tableId: string,
): Promise<() => Promise<void>> {
  const storeId = await fgaStore(page);
  const tuple = { user: `user:${tenant}|${userId}`, relation: "editor", object: `data_table:${tenant}|${tableId}` };
  const res = await page.request.post(`${FGA_URL}/stores/${storeId}/write`, {
    data: { writes: { tuple_keys: [tuple] } },
  });
  expect(res.ok(), await res.text()).toBeTruthy();
  return async () => {
    const del = await page.request.post(`${FGA_URL}/stores/${storeId}/write`, {
      data: { deletes: { tuple_keys: [tuple] } },
    });
    expect(del.ok(), await del.text()).toBeTruthy();
  };
}

