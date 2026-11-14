import { expect, test, type Page } from "@playwright/test";

import { cleanupQueue, deleteArtifactApi, loginViaKeycloak, removeWorkflowApi, uniqueName } from "./helpers";

/// 会話の AI がミニアプリを作る・直す道具（miniapp_create / miniapp_update・D-10 の T08）（前提: LLM=stub）。
///
/// - 作るのは確認なし（承認カードを出さない）。部品の版を省くと最新の版に固定し、下書き（未公開）のアプリができる。
/// - 直す承認カードは今のアプリを読み、公開中の版への影響・ボタンの実行先・入力・束ねる部品の版の変わり方を出す。
///   AI が読んだ後に変わっていれば言う。承認すると下書きの新しい版になり、却下すると版は動かない。
///
/// チャットはツールの結果を描画しないので、結果はアプリの本当の状態（API）で確かめる。
/// stub 駆動: `calltool:<ツール名> <入力 JSON>` でツールを決定的に呼ぶ。

const SHOTS = process.env.E2E_SHOTS_DIR;

const cleanup = cleanupQueue();
test.afterEach(() => cleanup.run());

async function csrfHeaders(page: Page): Promise<Record<string, string>> {
  const csrf = (await page.context().cookies()).find((c) => c.name === "shiki_csrf")?.value ?? "";
  return { "x-csrf-token": csrf, "content-type": "application/json" };
}

/// 件名・金額を受けるワークフロー（v1）を作り、同じ定義で v2 を積む。
async function createFlow(page: Page): Promise<{ id: string; name: string }> {
  const name = uniqueName("e2e-ai-app-flow");
  const ir = {
    ir_version: 1,
    name,
    display_name: name,
    triggers: [{ kind: "interactive" }],
    input_schema: {
      type: "object",
      required: ["title", "amount"],
      properties: { title: { type: "string", title: "件名" }, amount: { type: "integer", title: "金額" } },
    },
    nodes: [
      {
        id: "compute",
        type: "script.run",
        label: "計算する",
        params: { source: { inline: "function main(i){ return { ok: true }; }" } },
      },
    ],
    edges: [],
  };
  const res = await page.request.post("/api/workflows", { headers: await csrfHeaders(page), data: { ir } });
  expect(res.ok(), await res.text()).toBeTruthy();
  const id = ((await res.json()) as { id: string }).id;
  cleanup.add(() => removeWorkflowApi(page, id));
  const put = await page.request.put(`/api/workflows/${id}`, {
    headers: await csrfHeaders(page),
    data: { ir: { ...ir, display_name: `${name} v2` }, expected_version: 1 },
  });
  expect(put.ok(), await put.text()).toBeTruthy();
  return { id, name };
}

const SCREEN = [
  "```ui",
  "app 経費",
  "## 経費の申請",
  "件名 = text 必須 as title",
  "金額 = number 必須 as amount",
  "button 申請する -> workflow 申請",
  "```",
  "",
].join("\n");

type App = { id: string; name: string; currentVersion: number; publishedVersion: number | null };

async function findApp(page: Page, name: string): Promise<App | null> {
  const res = await page.request.get(`/api/artifacts?kind=mini_app&limit=100`);
  if (!res.ok()) return null;
  const body = (await res.json()) as {
    items: { id: string; name: string; current_version: number; published_version: number | null }[];
  };
  const a = body.items.find((x) => x.name === name);
  return a ? { id: a.id, name: a.name, currentVersion: a.current_version, publishedVersion: a.published_version } : null;
}

async function appBody(page: Page, id: string, version: number) {
  const res = await page.request.get(`/api/mini-apps/${id}/resolved?version=${version}`);
  expect(res.ok(), await res.text()).toBeTruthy();
  return ((await res.json()) as { body: { screen: string; workflows: { alias: string; version: number }[] } }).body;
}

async function ask(page: Page, tool: string, args: Record<string, unknown>) {
  await page.goto("/");
  const input = page.getByLabel("メッセージを入力");
  await input.fill(`calltool:${tool} ${JSON.stringify(args)}`);
  await page.getByRole("button", { name: "送信" }).click();
  await page.waitForURL(/\/c\/[0-9a-f-]+/i, { timeout: 20_000 });
}

async function waitAnswered(page: Page) {
  await expect(page.getByRole("button", { name: "生成を停止" })).toHaveCount(0, { timeout: 60_000 });
}

test("作るのは確認なしで下書きになり、直す承認カードは今の版との差と公開への影響を出す", async ({ page }) => {
  test.setTimeout(240_000);
  await page.setViewportSize({ width: 1440, height: 900 });
  await loginViaKeycloak(page);
  const flow = await createFlow(page);

  // 作る（承認カードを出さない）。版を省いた部品は最新（v2）に固定する。
  const name = uniqueName("e2e-AI の経費アプリ");
  await ask(page, "miniapp_create", {
    name,
    description: "経費を申請する",
    screen: SCREEN,
    workflows: [{ alias: "申請", workflow: flow.name }],
  });
  await waitAnswered(page);
  await expect(page.getByTestId("approval-card")).toHaveCount(0);
  await expect.poll(() => findApp(page, name), { timeout: 30_000 }).not.toBeNull();
  const app = await findApp(page, name);
  if (!app) throw new Error("アプリができていない");
  cleanup.add(() => deleteArtifactApi(page, app.id));
  expect(app.currentVersion).toBe(1);
  expect(app.publishedVersion).toBeNull();
  expect((await appBody(page, app.id, 1)).workflows).toEqual([expect.objectContaining({ alias: "申請", version: 2 })]);

  const args = {
    app: app.id,
    expected_version: 1,
    screen: SCREEN.replace("button 申請する", "button 送る"),
    workflows: [{ alias: "申請", workflow: flow.id, version: 1 }],
  };
  // 読んだ版が古いと言う（承認しても保存されない）。却下すると版は動かない。
  await ask(page, "miniapp_update", { ...args, expected_version: 7 });
  const card = page.getByTestId("approval-card");
  const stale = card.getByTestId("miniapp-update-summary");
  await expect(stale).toBeVisible({ timeout: 30_000 });
  await expect(stale).toContainText("AI が読んだ後にアプリが変わっています（今は v1）");
  await card.getByRole("button", { name: "却下" }).click();
  await waitAnswered(page);
  expect((await findApp(page, name))?.currentVersion).toBe(1);

  await ask(page, "miniapp_update", args);
  const summary = card.getByTestId("miniapp-update-summary");
  await expect(summary).toBeVisible({ timeout: 30_000 });
  await expect(summary).toContainText(`ミニアプリ「${name}」を直します（v1 の次の版）。`);
  await expect(summary).toContainText("保存は下書きになります。まだ公開していないので");
  await expect(summary).not.toContainText("AI が読んだ後に");
  await expect(summary).toContainText("＋ ボタン 送る → 申請");
  await expect(summary).toContainText("－ ボタン 申請する → 申請");
  await expect(summary).not.toContainText("入力 ");
  await expect(summary).toContainText(`「申請」（${flow.name}） を v2 → v1`);
  if (SHOTS) await card.screenshot({ path: `${SHOTS}/ai-miniapp-update-card.png` });
  await card.getByRole("button", { name: "承認して続行" }).click();
  await waitAnswered(page);
  await expect.poll(async () => (await findApp(page, name))?.currentVersion, { timeout: 30_000 }).toBe(2);
  const body = await appBody(page, app.id, 2);
  expect(body.screen).toContain("button 送る -> workflow 申請");
  expect(body.workflows).toEqual([expect.objectContaining({ alias: "申請", version: 1 })]);
});
