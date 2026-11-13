import { expect, test, type Page } from "@playwright/test";

import { cleanupQueue, loginViaKeycloak, removeWorkflowApi, uniqueName } from "./helpers";

/// ワークフローエディタの「AI に依頼」パネル（D-10 T10・GAP-33）の e2e。stub の `fixwf:<語>` が list_workflows →
/// read_workflow → emit_workflow で、開いているワークフローの新しい版を保存する（実 LLM は呼ばない）。
/// - パネルから作った会話は、このワークフローを対象に持つ（エディタの会話一覧に出る）。
/// - 未保存の変更が無ければ、AI が保存した版をその場で読み直す（保存状態が新しい版になる）。
/// - 未保存の変更があれば読み直さずに帯を出し、「AI の版を読み込む」で自分の変更を捨てて読み直す。
/// - パネルを開いている間は設定パネルを出さない。
/// 作ったワークフローと会話は最後に消す。

const SHOTS = process.env.E2E_SHOTS_DIR;

const cleanup = cleanupQueue();
test.afterEach(() => cleanup.run());

async function csrfHeaders(page: Page): Promise<Record<string, string>> {
  const csrf = (await page.context().cookies()).find((c) => c.name === "shiki_csrf")?.value ?? "";
  return { "x-csrf-token": csrf, "content-type": "application/json" };
}

async function createFlow(page: Page, name: string): Promise<string> {
  const res = await page.request.post("/api/workflows", {
    headers: await csrfHeaders(page),
    data: {
      ir: {
        ir_version: 1,
        name,
        display_name: "経費精算（AI パネル E2E）",
        description: "申請を上長に回す",
        triggers: [{ kind: "interactive" }],
        input_schema: { type: "object", properties: { amount: { type: "number" } } },
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

async function savedVersion(page: Page, id: string): Promise<number> {
  const res = await page.request.get(`/api/workflows/${id}`);
  expect(res.ok(), await res.text()).toBeTruthy();
  return ((await res.json()) as { version: number }).version;
}

/// パネルの入力欄に書いて送る（最初の発話は会話を作ってから送る）。
async function ask(page: Page, text: string): Promise<void> {
  const panel = page.getByTestId("workflow-chat-panel");
  const input = panel.getByLabel("メッセージを入力");
  await input.click();
  await input.fill(text);
  await panel.getByRole("button", { name: "送信" }).click();
}

test("AI に依頼: 保存した版を読み直し、未保存の変更があれば帯で選ばせる", async ({ page }) => {
  test.setTimeout(240_000);
  await page.setViewportSize({ width: 1440, height: 900 });
  await loginViaKeycloak(page);
  const name = uniqueName("editor-ai");
  const id = await createFlow(page, name);

  await page.goto(`/workflows/${id}`);
  const canvas = page.locator(".react-flow");
  await expect(canvas.getByText("スクリプト").first()).toBeVisible({ timeout: 20_000 });

  // パネルを開く（設定パネルは出さない）。
  await page.getByTestId("workflow-assistant-toggle").first().click();
  const panel = page.getByTestId("workflow-chat-panel");
  await expect(panel).toBeVisible();
  await expect(panel.getByText("このワークフローの直し方を頼めます")).toBeVisible();
  if (SHOTS) await page.screenshot({ path: `${SHOTS}/workflow-editor-ai-open.png` });

  // 未保存の変更が無い: AI が保存した v2 をその場で読み直す。
  await ask(page, `fixwf:${name}`);
  await expect(page.getByText("AI が v2 を保存しました").first()).toBeVisible({ timeout: 60_000 });
  await expect.poll(() => savedVersion(page, id)).toBe(2);
  await expect(page.getByText(/v2/).first()).toBeVisible();
  await expect(page.getByTestId("workflow-ai-version-banner")).toHaveCount(0);

  // パネルから作った会話は、このワークフローを対象に持つ。
  const threads = await page.request.get(`/api/threads?origin_artifact_id=${id}`);
  expect(threads.ok(), await threads.text()).toBeTruthy();
  const list = (await threads.json()) as {
    threads: { id: string; origin_artifact_kind?: string; origin_artifact_name?: string }[];
  };
  expect(list.threads).toHaveLength(1);
  expect(list.threads[0].origin_artifact_kind).toBe("workflow");
  for (const t of list.threads) {
    cleanup.add(async () => {
      await page.request.delete(`/api/threads/${t.id}`, { headers: await csrfHeaders(page) });
    });
  }
  if (SHOTS) await page.screenshot({ path: `${SHOTS}/workflow-editor-ai-saved.png` });

  // 未保存の変更を作る（パネルを閉じて、設定パネルからブロックを消す）。
  await page.getByRole("button", { name: "チャットを閉じる" }).first().click();
  await canvas.getByText("スクリプト").first().click();
  await page.getByRole("button", { name: "このブロックを削除" }).click();
  await expect(canvas.getByText("スクリプト")).toHaveCount(0);

  // パネルに「保存していない変更があります」が出る。AI の保存は読み直さずに帯で知らせる。
  await page.getByTestId("workflow-assistant-toggle").first().click();
  await expect(page.getByTestId("workflow-assistant-unsaved")).toBeVisible();
  await ask(page, `fixwf:${name}`);
  const banner = page.getByTestId("workflow-ai-version-banner");
  await expect(banner).toBeVisible({ timeout: 60_000 });
  await expect(banner).toContainText("v3");
  await expect(canvas.getByText("スクリプト")).toHaveCount(0);
  if (SHOTS) await page.screenshot({ path: `${SHOTS}/workflow-editor-ai-banner.png` });

  // AI の版を読み込む: 自分の変更を捨てて v3 を読み直す。
  await banner.getByRole("button", { name: /AI の版を読み込む/ }).click();
  await expect(banner).toHaveCount(0);
  await expect(canvas.getByText("スクリプト").first()).toBeVisible({ timeout: 15_000 });
  await expect(page.getByTestId("workflow-assistant-unsaved")).toHaveCount(0);
});
