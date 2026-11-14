import { expect, test, type Page } from "@playwright/test";

import { cleanupQueue, deleteArtifactApi, loginViaKeycloak, uniqueName } from "./helpers";

/// ミニアプリの画面の「AI に依頼」パネルと、/apps の「AI と作る」（D-10 T11）の e2e（前提: LLM=stub）。
/// stub 駆動: `calltool:<ツール名> <入力 JSON>` でツールを決定的に呼ぶ（実 LLM は呼ばない）。
/// - 編集できる人には、アプリの画面に「AI に依頼」が出る。パネルから作った会話は、このアプリを対象に持つ。
/// - AI がこのアプリを保存したら（承認カードを経る）、その下書きをその場で開き直す。
/// - 「AI と作る」は目的 app_builder の会話を作って会話の画面へ移る。そこで AI がアプリを作ると、会話がそのアプリに
///   紐づく（以後はアプリの画面から続けられる）。
/// 作ったアプリと会話は最後に消す。

const SHOTS = process.env.E2E_SHOTS_DIR;

const cleanup = cleanupQueue();
test.afterEach(() => cleanup.run());

async function csrfHeaders(page: Page): Promise<Record<string, string>> {
  const csrf = (await page.context().cookies()).find((c) => c.name === "shiki_csrf")?.value ?? "";
  return { "x-csrf-token": csrf, "content-type": "application/json" };
}

const SCREEN_V1 = "## 受付の案内\n\n来客の受付はこちらです。\n";
const SCREEN_V2 = "## 受付の案内\n\n来客の受付は 2 階に移りました。\n";

async function createApp(page: Page, name: string): Promise<string> {
  const res = await page.request.post("/api/mini-apps", {
    headers: await csrfHeaders(page),
    data: { name, body: { description: "受付の案内（AI パネル E2E）", screen: SCREEN_V1, workflows: [] } },
  });
  expect(res.ok(), await res.text()).toBeTruthy();
  const id = ((await res.json()) as { id: string }).id;
  cleanup.add(() => deleteArtifactApi(page, id));
  return id;
}

async function removeThread(page: Page, id: string): Promise<void> {
  await page.request.delete(`/api/threads/${id}`, { headers: await csrfHeaders(page) });
}

async function threadsOf(page: Page, appId: string) {
  const res = await page.request.get(`/api/threads?origin_artifact_id=${appId}`);
  expect(res.ok(), await res.text()).toBeTruthy();
  return ((await res.json()) as { threads: { id: string; origin_artifact_kind?: string }[] }).threads;
}

test("アプリの画面の AI に依頼: AI が保存した下書きをその場で開き直す", async ({ page }) => {
  test.setTimeout(240_000);
  await page.setViewportSize({ width: 1440, height: 900 });
  await loginViaKeycloak(page);
  const name = uniqueName("e2e-ai-panel-app");
  const id = await createApp(page, name);

  await page.goto(`/apps/${id}`);
  await expect(page.getByText("来客の受付はこちらです。")).toBeVisible({ timeout: 20_000 });

  // パネルを開く。
  await page.getByTestId("miniapp-assistant-toggle").first().click();
  const panel = page.getByTestId("miniapp-chat-panel");
  await expect(panel).toBeVisible();
  await expect(panel.getByText("このアプリの直し方を頼めます")).toBeVisible();
  if (SHOTS) await page.screenshot({ path: `${SHOTS}/miniapp-ai-panel-open.png` });

  // AI に直させる（承認カードを経る）。
  const input = panel.getByLabel("メッセージを入力");
  await input.click();
  await input.fill(`calltool:miniapp_update ${JSON.stringify({ app: id, expected_version: 1, screen: SCREEN_V2 })}`);
  await panel.getByRole("button", { name: "送信" }).click();
  const card = panel.getByTestId("approval-card");
  await expect(card.getByTestId("miniapp-update-summary")).toBeVisible({ timeout: 30_000 });
  if (SHOTS) await page.screenshot({ path: `${SHOTS}/miniapp-ai-panel-approval.png` });
  await card.getByRole("button", { name: "承認して続行" }).click();

  // 保存した下書き（v2）をその場で開き直す。
  await expect(page.getByText("AI が v2 を保存しました").first()).toBeVisible({ timeout: 60_000 });
  await expect(page.getByText("来客の受付は 2 階に移りました。")).toBeVisible({ timeout: 15_000 });
  if (SHOTS) await page.screenshot({ path: `${SHOTS}/miniapp-ai-panel-saved.png` });

  // パネルから作った会話は、このアプリを対象に持つ。
  const threads = await threadsOf(page, id);
  for (const t of threads) cleanup.add(() => removeThread(page, t.id));
  expect(threads).toHaveLength(1);
  expect(threads[0].origin_artifact_kind).toBe("mini_app");
});

test("AI と作る: 会話でできたアプリに会話が紐づく", async ({ page }) => {
  test.setTimeout(240_000);
  await page.setViewportSize({ width: 1440, height: 900 });
  await loginViaKeycloak(page);

  await page.goto("/apps");
  await page.getByTestId("apps-build-with-ai").first().click();
  await page.waitForURL(/\/c\/[0-9a-f-]+/i, { timeout: 20_000 });
  const threadId = page.url().split("/c/")[1].split(/[?#]/)[0];
  cleanup.add(() => removeThread(page, threadId));
  const before = await page.request.get(`/api/threads/${threadId}`);
  expect(((await before.json()) as { purpose?: string }).purpose).toBe("app_builder");

  const name = uniqueName("e2e-ai-built-app");
  const input = page.getByLabel("メッセージを入力");
  await input.fill(
    `calltool:miniapp_create ${JSON.stringify({ name, description: "AI と作った受付", screen: SCREEN_V1 })}`,
  );
  await page.getByRole("button", { name: "送信" }).click();

  // アプリができ、会話がそのアプリに紐づく。
  await expect
    .poll(
      async () => {
        const res = await page.request.get(`/api/threads/${threadId}`);
        return ((await res.json()) as { origin_artifact_kind?: string }).origin_artifact_kind ?? null;
      },
      { timeout: 60_000 },
    )
    .toBe("mini_app");
  const after = (await (await page.request.get(`/api/threads/${threadId}`)).json()) as {
    origin_artifact_id: string;
    origin_artifact_name: string;
    purpose?: string;
  };
  cleanup.add(() => deleteArtifactApi(page, after.origin_artifact_id));
  expect(after.origin_artifact_name).toBe(name);
  expect(after.purpose).toBe("app_builder");
  if (SHOTS) await page.screenshot({ path: `${SHOTS}/miniapp-ai-built.png` });
});
