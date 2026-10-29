import { expect, test, type Page } from "@playwright/test";

import { cleanupQueue, loginViaKeycloak, removeWorkflowApi, stubChatModelId, uniqueName } from "./helpers";

/// 「データを調べる」（data.query）の結果の候補の e2e（BUG-108 の残り）:
/// 件数（count）と集計の結果（groups）は、件数を数える設定（with_count）と集計（aggregate）を決めたときだけ値が入る。
/// 決めていないときは、後ろのブロックの「差し込む」の候補に出さない（選ぶと値が無く、実行で失敗していた）。決めれば出る。
/// 候補を出すかは、カタログの単一定義（OutputField の present_if）で決める。実行はしない（実 LLM を呼ばない）。

const SHOTS = process.env.E2E_SHOTS_DIR;

const cleanup = cleanupQueue();
test.afterEach(() => cleanup.run());

async function csrfHeaders(page: Page): Promise<Record<string, string>> {
  const csrf = (await page.context().cookies()).find((c) => c.name === "shiki_csrf")?.value ?? "";
  return { "x-csrf-token": csrf, "content-type": "application/json" };
}

function ir(name: string, model: string, query: Record<string, unknown>) {
  return {
    ir_version: 1,
    name,
    display_name: name,
    triggers: [{ kind: "interactive" }],
    nodes: [
      {
        id: "find",
        type: "data.query",
        label: "申請を探す",
        params: { table: "8c8a6f6e-2ab7-4a44-a815-9a2b53c4e9a1", query },
      },
      { id: "note", type: "llm.invoke", label: "まとめる", params: { model, prompt: "件数を書き出してください" } },
    ],
    edges: [{ from: "find", from_port: "out", to: "note" }],
  };
}

/// 後ろのブロック（まとめる）の「差し込む」の候補の名前。
async function insertCandidates(page: Page, id: string): Promise<string[]> {
  await page.goto(`/workflows/${id}`);
  const node = page.getByTestId("rf__node-note");
  await expect(node).toBeVisible({ timeout: 15_000 });
  await node.click();
  const panel = page.getByRole("complementary", { name: "ブロックの設定" });
  await panel.locator('[data-value-expr="AI への指示"]').getByRole("button", { name: "差し込む" }).click();
  const items = page.getByRole("menuitem", { name: /^「申請を探す」の/ });
  await expect(items.first()).toBeVisible({ timeout: 15_000 });
  return items.allTextContents();
}

test("データを調べる: 件数と集計の結果は、決めたときだけ差し込む候補に出る（BUG-108）", async ({ page }) => {
  test.setTimeout(120_000);
  await page.setViewportSize({ width: 1440, height: 900 });
  await loginViaKeycloak(page);
  const model = await stubChatModelId(page);
  const name = uniqueName("query-candidates");
  const res = await page.request.post("/api/workflows", {
    headers: await csrfHeaders(page),
    data: { ir: ir(name, model, { limit: 50 }) },
  });
  expect(res.ok(), await res.text()).toBeTruthy();
  const id = ((await res.json()) as { id: string }).id;
  cleanup.add(() => removeWorkflowApi(page, id));

  // 件数も集計も決めていない: 見つかったレコードと続きの印は出て、件数と集計の結果は出ない。
  const plain = await insertCandidates(page, id);
  expect(plain).toEqual(expect.arrayContaining(["「申請を探す」の items", "「申請を探す」の next_cursor"]));
  expect(plain).not.toContain("「申請を探す」の count");
  expect(plain).not.toContain("「申請を探す」の groups");
  if (SHOTS) await page.screenshot({ path: `${SHOTS}/bug108-candidates-plain-pc.png` });

  // 件数を数え、集計を決めた版: 件数と集計の結果も出る。
  const put = await page.request.put(`/api/workflows/${id}`, {
    headers: await csrfHeaders(page),
    data: { expected_version: 1, ir: ir(name, model, { limit: 50, with_count: true, aggregate: [{ metric: "count" }] }) },
  });
  expect(put.ok(), await put.text()).toBeTruthy();
  const counted = await insertCandidates(page, id);
  expect(counted).toEqual(
    expect.arrayContaining(["「申請を探す」の items", "「申請を探す」の count", "「申請を探す」の groups"]),
  );
  if (SHOTS) await page.screenshot({ path: `${SHOTS}/bug108-candidates-counted-pc.png` });
});
