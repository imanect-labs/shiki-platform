import { expect, test, type Page } from "@playwright/test";

import { cleanupAll, loginViaKeycloak, pinStubOrgDefault } from "./helpers";

/// AI エージェント（agent.invoke）の e2e（BUG-6）:
/// ブロックを置く → 依頼を書く → 使ってよいツールを選ぶ → 保存（要る権限が補われる）→ 実行 → 実行履歴に
/// エージェントの回答が残る。依頼は Python として実行されない（stub の LLM は「回答: <依頼>」を返す）。
/// 前提: compose の shiki-server が SHIKI__WORKFLOW__ENABLED=true・LLM は stub。

type SavedIr = {
  declared_scopes: string[];
  nodes: { id: string; type: string; params: Record<string, unknown> }[];
};

async function savedIr(page: Page, id: string): Promise<SavedIr> {
  return ((await (await page.request.get(`/api/workflows/${id}`)).json()) as { ir: SavedIr }).ir;
}

test("AI エージェント: 依頼を書き、道具を選んで保存・実行すると、実行履歴に回答が残る（BUG-6）", async ({ page }) => {
  test.setTimeout(180_000);
  await loginViaKeycloak(page);
  // モデルを省略した AI エージェントを実行するので、組織の既定（Auto の振り先）が実 LLM なら stub に替えて流し、
  // 最後に戻す（BUG-85）。作ったワークフローも消す。
  const restore = await pinStubOrgDefault(page);
  let id = "";
  try {
    const settings = (await (await page.request.get("/api/workflows/agent-settings")).json()) as {
      web_allowlist: string[];
    };

    await page.goto("/workflows");
    await expect(page.getByText("読み込み中…")).toHaveCount(0);
    await page.getByRole("button", { name: "新しいワークフロー", exact: true }).click();
    await page.waitForURL(/\/workflows\/[0-9a-f-]+$/i, { timeout: 20_000 });
    id = page.url().split("/").pop() ?? "";

    // 最初のブロックに「AI エージェント」を置く。既定は社内を読むツールだけが選ばれていて、一覧の上に理由を出す
    // （D-100）。
    await page.getByRole("button", { name: "最初のブロック" }).click();
    await page.getByRole("button", { name: /^AI エージェント/ }).click();
    const canvas = page.locator(".react-flow");
    await expect(canvas.getByText("AI エージェント").first()).toBeVisible();
    const panel = page.getByRole("complementary", { name: "ブロックの設定" });
    const tools = panel.getByTestId("agent-tools");
    await expect(tools.getByRole("checkbox", { name: /社内検索/ })).toBeChecked();
    await expect(tools.getByRole("checkbox", { name: /文書の本文を読む/ })).toBeChecked();
    await expect(tools.getByRole("checkbox", { name: /フォルダの一覧/ })).toBeChecked();
    await expect(tools.getByRole("checkbox", { name: /Web 検索/ })).not.toBeChecked();
    await expect(panel.getByTestId("agent-tools-default-note")).toHaveText("社内の資料を読むツールを選んであります。");
    await expect(tools.getByRole("checkbox", { name: /社内検索/ })).toHaveAccessibleDescription(/使ってよい操作: /);
    await page.screenshot({ path: "test-results/agent-default-tools-pc.png" });
    // 社外へ届くツールには目印が付く。管理者が読んでよいサイトを決めていなければ Web ページを読むツールは選べない。
    await expect(tools.getByText("社外に送信あり").first()).toBeVisible();
    const fetchBox = tools.getByRole("checkbox", { name: /Web ページを読む/ });
    if (settings.web_allowlist.length === 0) {
      await expect(fetchBox).toBeDisabled();
      await expect(tools.getByText("管理者が読んでよいサイトを決めていないため、選べません。")).toBeVisible();
    } else {
      await expect(fetchBox).toBeEnabled();
    }

    // 依頼を書き、文書の本文を読む道具を外して保存する。要る権限（スコープ）は保存で補われる。
    const instruction = panel.locator('[data-value-expr="エージェントへの依頼"]');
    await instruction.getByLabel("エージェントへの依頼").fill("旅費規程の要点をまとめて");
    await tools.getByRole("checkbox", { name: /文書の本文を読む/ }).uncheck();
    // 置いたときの組み合わせから変えたら、選んである理由は出さない。
    await expect(panel.getByTestId("agent-tools-default-note")).toHaveCount(0);
    await panel.getByLabel("手順の上限（回）").fill("4");
    await page.getByRole("button", { name: "保存", exact: true }).click();
    await expect(page.getByText(/保存しました/).first()).toBeVisible({ timeout: 15_000 });
    const ir = await savedIr(page, id);
    const agent = ir.nodes.find((n) => n.type === "agent.invoke");
    expect(agent?.params).toMatchObject({
      instruction: "旅費規程の要点をまとめて",
      allowed_tools: ["rag.search", "storage.list"],
      max_steps: 4,
    });
    expect(ir.declared_scopes).toEqual(expect.arrayContaining(["rag.query", "storage.read"]));
    expect(ir.declared_scopes).not.toContain("doc.read");

    // 実行 → 実行履歴で成功し、ブロックを開くとエージェントの回答が出る（依頼を Python として実行しない）。
    await page.getByRole("button", { name: "実行", exact: true }).click();
    await page.getByRole("button", { name: "実行する" }).click();
    await page.waitForURL(/\/runs\?run=/, { timeout: 20_000 });
    const sheet = page.locator('[role="dialog"]');
    await expect(sheet.locator("h2").getByText("成功")).toBeVisible({ timeout: 90_000 });
    await sheet.getByRole("button", { name: /AI エージェント/ }).first().click();
    const result = sheet.getByTestId("agent-result");
    await expect(result).toContainText("回答: 旅費規程の要点をまとめて", { timeout: 15_000 });
    await expect(result.getByText("道具は使っていません")).toBeVisible();
  } finally {
    // 組織の設定を戻すのと作ったワークフローを消すのは、片方が投げても両方走らせる。
    await cleanupAll(restore, async () => {
      if (!id) return;
      const csrf = (await page.context().cookies()).find((c) => c.name === "shiki_csrf")?.value ?? "";
      const res = await page.request.delete(`/api/artifacts/${id}`, { headers: { "x-csrf-token": csrf } });
      expect(res.status(), await res.text()).toBeLessThan(300);
    });
  }
});

test("AI エージェント: スマホのシートでも道具の一覧を選べ、横にはみ出さない（BUG-6）", async ({ browser, page }) => {
  test.setTimeout(120_000);
  await loginViaKeycloak(page);
  const id = await createAgentFlow(page);
  // ログインは PC の画面で済ませ、そのセッションをタッチ端末の文脈へ持ち込む（workflows-editor.spec と同じ）。
  const ctx = await browser.newContext({
    storageState: await page.context().storageState(),
    viewport: { width: 390, height: 844 },
    hasTouch: true,
    isMobile: true,
    deviceScaleFactor: 3,
    ignoreHTTPSErrors: true,
  });
  const phone = await ctx.newPage();
  await phone.goto(`/workflows/${id}`);
  const flow = phone.getByTestId("workflow-mobile-flow");
  await expect(flow).toBeVisible({ timeout: 15_000 });
  await flow.getByRole("button", { name: /規程を調べる/ }).filter({ hasText: "規程を調べる" }).click();
  const sheet = phone.getByRole("dialog");
  const tools = sheet.getByTestId("agent-tools");
  await expect(tools).toBeVisible();
  await expect(tools.getByRole("checkbox", { name: /社内検索/ })).toBeChecked();
  await tools.getByRole("checkbox", { name: /Web 検索/ }).check();
  await expect(tools.getByRole("checkbox", { name: /Web 検索/ })).toBeChecked();
  expect(await sheet.evaluate((el) => el.scrollWidth <= el.clientWidth + 1)).toBe(true);
  await ctx.close();
});

/// 「規程を調べる」エージェントだけのフローを API で作る（道具は社内検索）。
async function createAgentFlow(page: Page): Promise<string> {
  const csrf = (await page.context().cookies()).find((c) => c.name === "shiki_csrf")?.value ?? "";
  const name = `wf-agent-${Date.now().toString(36)}`;
  const res = await page.request.post("/api/workflows", {
    headers: { "x-csrf-token": csrf, "content-type": "application/json" },
    data: {
      ir: {
        ir_version: 1,
        name,
        display_name: name,
        triggers: [{ kind: "interactive" }],
        nodes: [
          {
            id: "agent",
            type: "agent.invoke",
            label: "規程を調べる",
            params: { instruction: "旅費規程の要点をまとめて", allowed_tools: ["rag.search"] },
          },
        ],
        edges: [],
      },
    },
  });
  expect(res.ok(), await res.text()).toBeTruthy();
  return ((await res.json()) as { id: string }).id;
}
