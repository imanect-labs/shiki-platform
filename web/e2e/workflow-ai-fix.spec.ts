import { expect, test, type Page } from "@playwright/test";

import { cleanupQueue, loginViaKeycloak, removeWorkflowApi, uniqueName } from "./helpers";

/// 新しい会話から既存のワークフローを直す（GAP-31）の e2e:
/// stub の `fixwf:<語>` が、list_workflows で探し、当たった先頭を read_workflow で読み、説明を書き換えて emit_workflow で
/// 新しい版として保存する（実 LLM は呼ばない）。
/// - 会話の道具の行に「「…」でワークフローを検索しました」「ワークフローを読み込みました」「ワークフローを保存しました」が出る。
/// - カードは同じワークフローの v2 で、保存された定義も v2 になり説明が変わる。ワークフローは増えない。
/// - スマホ幅でも道具の行とカードが横にはみ出さない。
/// 作ったワークフローと会話は最後に消す。

const SHOTS = process.env.E2E_SHOTS_DIR;

const cleanup = cleanupQueue();
test.afterEach(() => cleanup.run());

async function csrfHeaders(page: Page): Promise<Record<string, string>> {
  const csrf = (await page.context().cookies()).find((c) => c.name === "shiki_csrf")?.value ?? "";
  return { "x-csrf-token": csrf, "content-type": "application/json" };
}

/// 直す前のワークフロー（手動で実行・件名と金額を入れる・script のブロック 1 つ）。
async function createExpenseFlow(page: Page, name: string): Promise<string> {
  const res = await page.request.post("/api/workflows", {
    headers: await csrfHeaders(page),
    data: {
      ir: {
        ir_version: 1,
        name,
        display_name: "経費精算（E2E）",
        description: "申請を上長に回す",
        triggers: [{ kind: "interactive" }],
        input_schema: {
          type: "object",
          properties: { title: { type: "string" }, amount: { type: "number" } },
          required: ["amount"],
        },
        nodes: [
          {
            id: "compute",
            type: "script.run",
            params: { source: { inline: "function main(i){ return { ok: true }; }" } },
          },
        ],
        edges: [],
      },
    },
  });
  expect(res.ok(), await res.text()).toBeTruthy();
  const id = ((await res.json()) as { id: string }).id;
  cleanup.add(() => removeWorkflowApi(page, id));
  return id;
}

async function deleteThread(page: Page, id: string): Promise<void> {
  const res = await page.request.delete(`/api/threads/${id}`, { headers: await csrfHeaders(page) });
  expect([200, 204, 404]).toContain(res.status());
}

/// 自分のワークフローのうち、名前が `name` のものの数（一覧を最後まで読む必要は無い・新しい順の先頭に出る）。
async function countByName(page: Page, name: string): Promise<number> {
  const res = await page.request.get("/api/workflows?limit=100");
  expect(res.ok(), await res.text()).toBeTruthy();
  return ((await res.json()) as { items: { name: string }[] }).items.filter((w) => w.name === name).length;
}

test("新しい会話から既存のワークフローを探して、新しい版として直す", async ({ page }) => {
  test.setTimeout(180_000);
  await page.setViewportSize({ width: 1440, height: 900 });
  await loginViaKeycloak(page);
  const name = uniqueName("fix-expense");
  const id = await createExpenseFlow(page, name);

  await page.goto("/");
  const input = page.getByLabel("メッセージを入力");
  await input.click();
  await input.fill(`fixwf:${name}`);
  await page.getByRole("button", { name: "送信" }).click();
  await page.waitForURL(/\/c\/[0-9a-f-]+/i, { timeout: 20_000 });
  const threadId = new URL(page.url()).pathname.split("/").pop()!;
  cleanup.add(() => deleteThread(page, threadId));

  // 同じワークフローの新しい版のカード（v2）。
  const card = page.getByText("ワークフローを保存しました。エディタで確認・編集できます。").first();
  await expect(card).toBeVisible({ timeout: 60_000 });
  await expect(page.getByText("経費精算（E2E）").first()).toBeVisible();
  await expect(page.getByText("v2", { exact: true }).first()).toBeVisible();
  await expect(page.getByText("ワークフローを直しました。").first()).toBeVisible({ timeout: 30_000 });

  // 道具の行: 探す → 読む → 保存する（完了後は 1 行に畳まれるので開く）。
  const activity = page.getByTestId("tool-activity").last();
  await expect(activity).toBeVisible();
  await activity.getByRole("button").first().click();
  const expanded = page.getByTestId("tool-activity-expanded").last();
  await expect(expanded).toContainText(`「${name}」でワークフローを検索しました`);
  await expect(expanded).toContainText("ワークフローを読み込みました");
  await expect(expanded).toContainText("ワークフローを保存しました");
  if (SHOTS) await page.screenshot({ path: `${SHOTS}/workflow-ai-fix-pc.png`, fullPage: true });

  // 保存された定義は v2 で、説明が変わる。新しいワークフローは増えない。
  const saved = await page.request.get(`/api/workflows/${id}`);
  expect(saved.ok(), await saved.text()).toBeTruthy();
  const body = (await saved.json()) as { version: number; ir: { name: string; description?: string } };
  expect(body.version).toBe(2);
  expect(body.ir.name).toBe(name);
  expect(body.ir.description).toBe(`AI が直しました（${name}）`);
  expect(await countByName(page, name)).toBe(1);

  // 「エディタで開く」で同じワークフローを開く。
  await page.getByRole("link", { name: "エディタで開く" }).first().click();
  await page.waitForURL(new RegExp(`/workflows/${id}`), { timeout: 15_000 });

  // スマホ幅: 会話に戻っても道具の行とカードが横にはみ出さない。
  await page.setViewportSize({ width: 390, height: 844 });
  await page.goto(`/c/${threadId}`);
  await expect(page.getByText("ワークフローを保存しました。エディタで確認・編集できます。").first()).toBeVisible({
    timeout: 30_000,
  });
  const overflow = await page.evaluate(() => document.documentElement.scrollWidth - window.innerWidth);
  expect(overflow).toBeLessThanOrEqual(0);
  const mobileActivity = page.getByTestId("tool-activity").last();
  await mobileActivity.getByRole("button").first().click();
  await expect(page.getByTestId("tool-activity-expanded").last()).toContainText("でワークフローを検索しました");
  if (SHOTS) await page.screenshot({ path: `${SHOTS}/workflow-ai-fix-mobile.png`, fullPage: true });
});
