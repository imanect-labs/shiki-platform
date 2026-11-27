import { expect, test, type Browser, type Page } from "@playwright/test";

import { cleanupQueue, loginViaKeycloak, pinStubOrgDefault, removeWorkflowApi, uniqueName } from "./helpers";

/// テストで作ったもの（ワークフロー）を、テストの後に片付ける（失敗したテストでも走る・BUG-126）。
const cleanup = cleanupQueue();
const SHOTS = process.env.E2E_SHOTS_DIR;
test.afterEach(() => cleanup.run());


/// ワークフローの「AI に聞く」のモデル欄: 「組織の既定に従う」の行（BUG-12・BUG-33・D-5・D-30・D-46）。
///
/// - 一覧は `/llm/models?purpose=workflow`（保存時の照合・実行時の解決と同じ見え方）。
/// - 先頭の「組織の既定に従う（現在: …）」を選ぶと、保存した IR から model が消える。
/// - 省略したブロックを実行すると、step の出力の model が組織の既定（Auto なら振り先）になる。
/// - 組織がモデルの選択を禁じていれば、モデルの行は選べず、既定と違うモデルは外すよう促す。
/// - 使えるモデルが 0 件なら（組織が公開していない・編集する人のプロバイダの制限で外れた）、どちらでも当たる言い方で
///   管理者に相談するよう言う（判定用のモデルの欄も・D-169）。
/// 前提: compose の shiki-server が SHIKI__WORKFLOW__ENABLED=true・LLM は stub。

type LlmModel = { id: string; label: string };
type LlmModels = {
  default_model: string;
  allow_user_choice: boolean;
  auto: { model: string; label: string; effort: "low" | "medium" | "high" } | null;
  models: LlmModel[];
} & Record<string, unknown>;

const EFFORT_LABEL = { low: "速い", medium: "標準", high: "じっくり" } as const;

/// ワークフローのモデル欄が読む一覧の URL（query の有無によらず当てる）。
const isWorkflowModels = (url: URL) => url.pathname.endsWith("/api/llm/models");

/// ワークフローで使えるモデルの一覧。LLM が構成されていなければ null。
async function workflowModels(page: Page): Promise<LlmModels | null> {
  const res = await page.request.get("/api/llm/models?purpose=workflow");
  return res.ok() ? ((await res.json()) as LlmModels) : null;
}

/// 「現在: …」に出る組織の既定（model-field.tsx の orgDefaultLabel と同じ書き方）。
function orgDefaultLabel(data: LlmModels): string {
  if (data.default_model !== "auto") {
    return data.models.find((m) => m.id === data.default_model)?.label ?? data.default_model;
  }
  return data.auto ? `Auto（${data.auto.label}・${EFFORT_LABEL[data.auto.effort]}）` : "Auto";
}

/// 実行したときに動くモデルの id（Auto なら振り先）。
function orgDefaultModel(data: LlmModels): string | undefined {
  return data.default_model === "auto" ? data.auto?.model : data.default_model;
}

async function csrfHeader(page: Page): Promise<Record<string, string>> {
  const csrf = (await page.context().cookies()).find((c) => c.name === "shiki_csrf")?.value ?? "";
  return { "x-csrf-token": csrf, "content-type": "application/json" };
}

/// 「要約する」（AI に聞く）1 つだけのワークフローを作る（model は渡したときだけ書く）。
async function createLlmFlow(page: Page, model?: string): Promise<string> {
  const name = uniqueName("wf-model");
  const res = await page.request.post("/api/workflows", {
    headers: await csrfHeader(page),
    data: {
      ir: {
        ir_version: 1,
        name,
        display_name: name,
        triggers: [{ kind: "interactive" }],
        nodes: [
          {
            id: "ask",
            type: "llm.invoke",
            label: "要約する",
            params: { prompt: "今日の予定をまとめてください", ...(model ? { model } : {}) },
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

async function deleteFlow(page: Page, id: string) {
  const csrf = (await page.context().cookies()).find((c) => c.name === "shiki_csrf")?.value ?? "";
  const res = await page.request.delete(`/api/artifacts/${id}`, { headers: { "x-csrf-token": csrf } });
  expect(res.status(), await res.text()).toBeLessThan(300);
}

async function openBlock(page: Page, id: string) {
  await page.goto(`/workflows/${id}`);
  const canvas = page.locator(".react-flow");
  await expect(canvas.getByText("要約する").first()).toBeVisible({ timeout: 15_000 });
  await canvas.getByText("要約する").first().click();
  return page.getByRole("complementary", { name: "ブロックの設定" });
}

async function saveFlow(page: Page) {
  await page.getByRole("button", { name: "保存", exact: true }).click();
  await expect(page.getByText(/保存しました/).first()).toBeVisible({ timeout: 15_000 });
}

async function savedParams(page: Page, id: string): Promise<Record<string, unknown>> {
  const ir = ((await (await page.request.get(`/api/workflows/${id}`)).json()) as {
    ir: { nodes: { params: Record<string, unknown> }[] };
  }).ir;
  return ir.nodes[0].params;
}

test("モデル: 先頭の「組織の既定に従う」に今の既定が出て、選んで保存すると IR から model が消える", async ({ page }) => {
  test.setTimeout(120_000);
  await loginViaKeycloak(page);
  const models = await workflowModels(page);
  test.skip(!models || models.models.length === 0, "LLM が構成されていない（使えるモデルがない）環境");
  const data = models!;
  const first = data.models[0];
  const id = await createLlmFlow(page, first.id);
  const panel = await openBlock(page, id);

  const button = panel.getByTestId("workflow-model-button");
  await expect(button).toContainText(first.label);
  await button.click();
  const picker = page.getByTestId("workflow-model-picker");
  const row = picker.getByTestId("model-default-option");
  await expect(row).toContainText("組織の既定に従う");
  await expect(row).toContainText(`現在: ${orgDefaultLabel(data)}`);
  await row.click();
  await expect(picker).toHaveCount(0);
  await expect(button).toContainText(`組織の既定（${orgDefaultLabel(data)}）`);
  await expect(panel.getByText("省略すると組織の既定で動きます")).toBeVisible();

  await saveFlow(page);
  const params = await savedParams(page, id);
  expect(params).not.toHaveProperty("model");
  expect(params.prompt).toBe("今日の予定をまとめてください");
});

test("モデル: キーボードだけで一覧を開き、「組織の既定に従う」を選べる", async ({ page }) => {
  test.setTimeout(120_000);
  await loginViaKeycloak(page);
  const models = await workflowModels(page);
  test.skip(!models || models.models.length === 0, "LLM が構成されていない（使えるモデルがない）環境");
  const id = await createLlmFlow(page, models!.models[0].id);
  const panel = await openBlock(page, id);

  const button = panel.getByTestId("workflow-model-button");
  await button.focus();
  await page.keyboard.press("Enter");
  const picker = page.getByTestId("workflow-model-picker");
  await expect(picker.getByRole("listbox", { name: "モデル" })).toBeVisible();
  // 開いた直後の注目は今のモデル（既定の行の次）。上へ 1 つで既定の行、Enter で選ぶ。
  await expect(picker.locator('[role="option"][data-active="true"]')).toContainText(models!.models[0].label);
  await page.keyboard.press("ArrowUp");
  await expect(picker.getByTestId("model-default-option")).toHaveAttribute("data-active", "true");
  await page.keyboard.press("Enter");
  await expect(picker).toHaveCount(0);
  await expect(button).toContainText("組織の既定（");

  await saveFlow(page);
  expect(await savedParams(page, id)).not.toHaveProperty("model");
});

test("モデル: 省略したブロックを実行すると、step の出力の model が組織の既定になる", async ({ page }) => {
  test.setTimeout(120_000);
  await loginViaKeycloak(page);
  const before = await workflowModels(page);
  test.skip(!before || before.models.length === 0, "LLM が構成されていない（使えるモデルがない）環境");
  // 実行するので、組織の既定（Auto の振り先）が実 LLM なら stub に替えて流し、最後に戻す（BUG-85）。
  const restore = await pinStubOrgDefault(page);
  let id: string | null = null;
  try {
    const models = (await workflowModels(page))!;
    const expected = orgDefaultModel(models);
    id = await createLlmFlow(page);

    const start = await page.request.post(`/api/workflows/${id}/runs`, {
      headers: await csrfHeader(page),
      data: { input: {} },
    });
    expect(start.status(), await start.text()).toBe(202);
    const runId = ((await start.json()) as { run_id: string | null }).run_id;
    expect(runId).toBeTruthy();

    // 実行は非同期なので、step が終わるまで待つ。
    let step: { status: string; output: { model?: string; text?: string } } | null = null;
    await expect
      .poll(
        async () => {
          const res = await page.request.get(`/api/workflows/${id}/runs/${runId}/steps?path=ask`);
          if (!res.ok()) return "pending";
          step = (await res.json()) as typeof step;
          return step!.status;
        },
        { timeout: 60_000 },
      )
      .toBe("succeeded");
    expect(step!.output.model).toBe(expected);
    // stub が答えた（実 LLM ではない）ことも本文で確かめる（stub は「回答: <指示>」を返す）。
    expect(step!.output.text).toContain("回答: 今日の予定をまとめてください");
  } finally {
    await restore();
    if (id) await deleteFlow(page, id);
  }
});

test("モデル: 組織がモデルの選択を禁じていると、モデルの行は選べず、既定と違うモデルは外すよう促す", async ({
  page,
}) => {
  test.setTimeout(120_000);
  await loginViaKeycloak(page);
  const models = await workflowModels(page);
  test.skip(!models || models.models.length === 0, "LLM が構成されていない（使えるモデルがない）環境");
  const saved = models!.models[0];
  // 保存の後に管理者が選択を禁じ、既定を Auto にした状況（一覧の応答だけを差し替える）。
  const locked: LlmModels = {
    ...models!,
    default_model: "auto",
    allow_user_choice: false,
    auto: models!.auto ?? { model: saved.id, label: saved.label, effort: "low" },
  };
  const id = await createLlmFlow(page, saved.id);
  await page.route(isWorkflowModels, (route) => route.fulfill({ json: locked }));
  const panel = await openBlock(page, id);

  await expect(panel.getByText("この組織ではモデルを選べません。「組織の既定に従う」にしてください")).toBeVisible();
  const button = panel.getByTestId("workflow-model-button");
  await button.click();
  const picker = page.getByTestId("workflow-model-picker");
  await expect(picker.getByTestId("workflow-model-locked")).toContainText("この組織ではモデルを選べません");
  const modelRow = picker.getByTestId(`model-option-${saved.id}`);
  await expect(modelRow).toHaveAttribute("aria-disabled", "true");
  // 選べない行は押しても何も起きない（一覧は開いたまま・ボタンの表示も変わらない）。
  await modelRow.click({ force: true });
  await expect(picker).toBeVisible();
  await expect(button).toContainText(saved.label);
  await button.click();
  await expect(picker).toHaveCount(0);

  await panel.getByRole("button", { name: "組織の既定に戻す" }).click();
  await expect(button).toContainText(`組織の既定（${orgDefaultLabel(locked)}）`);
  await expect(panel.getByRole("button", { name: "組織の既定に戻す" })).toHaveCount(0);
  await expect(panel.getByText(/この組織ではモデルを選べません（管理者の設定）/)).toBeVisible();

  await saveFlow(page);
  expect(await savedParams(page, id)).not.toHaveProperty("model");
});

/// 「AI に聞く」と「AI で判定」を 1 つずつ持つワークフロー（判定のモデルは空のまま・置いたばかりと同じ・D-83）。
async function createLlmAndDecideFlow(page: Page): Promise<string> {
  const name = uniqueName("wf-model-none");
  const res = await page.request.post("/api/workflows", {
    headers: await csrfHeader(page),
    data: {
      ir: {
        ir_version: 1,
        name,
        display_name: name,
        triggers: [{ kind: "interactive" }],
        nodes: [
          { id: "ask", type: "llm.invoke", label: "要約する", params: { prompt: "今日の予定をまとめてください" } },
          {
            id: "triage",
            type: "ai.decide",
            label: "問い合わせを判定",
            params: {
              model: "",
              state: { $from: "input" },
              questions: [{ key: "urgent", type: "boolean", instructions: "急ぎの問い合わせか" }],
            },
          },
        ],
        edges: [{ from: "ask", to: "triage" }],
      },
    },
  });
  expect(res.ok(), await res.text()).toBeTruthy();
  const id = ((await res.json()) as { id: string }).id;
  cleanup.add(() => removeWorkflowApi(page, id));
  return id;
}

/// 使えるモデルが 0 件の一覧（応答だけを差し替える。組織が公開していないときも、編集する人のプロバイダの制限で
/// 外れたときも同じ応答になる）。
function noModels(models: LlmModels): LlmModels {
  return { ...models, auto: null, models: [] };
}

test("モデル: 使えるモデルが無いときは、モデルの欄も判定用のモデルの欄も管理者に相談するよう言う", async ({ page }) => {
  test.setTimeout(120_000);
  await page.setViewportSize({ width: 1440, height: 900 });
  await loginViaKeycloak(page);
  const models = await workflowModels(page);
  test.skip(!models, "LLM が構成されていない環境");
  const id = await createLlmAndDecideFlow(page);
  await page.route(isWorkflowModels, (route) => route.fulfill({ json: noModels(models!) }));
  const panel = await openBlock(page, id);
  await expect(panel.getByText("使えるモデルがありません。管理者に相談してください")).toBeVisible();
  await expect(panel.getByLabel("モデル ID")).toHaveAttribute("placeholder", "空欄で組織の既定");
  if (SHOTS) await page.screenshot({ path: `${SHOTS}/workflow-model-none-pc.png` });

  const canvas = page.locator(".react-flow");
  await canvas.getByText("問い合わせを判定").first().click();
  await expect(panel.getByText("使える判定用のモデルがありません。管理者に相談してください")).toBeVisible();
  // 組織が公開していないとは言わない（制限で外れた人にも当たる言い方）。
  await expect(panel.getByText(/公開してもらって/)).toHaveCount(0);
  if (SHOTS) await page.screenshot({ path: `${SHOTS}/workflow-decide-model-none-pc.png` });
});

test("スマホ: 使える判定用のモデルが無いときも、設定のシートで管理者に相談するよう言う", async ({ browser, page }) => {
  test.setTimeout(180_000);
  const phone = await phonePage(browser, page);
  const models = await workflowModels(page);
  test.skip(!models, "LLM が構成されていない環境");
  const id = await createLlmAndDecideFlow(page);
  await phone.route(isWorkflowModels, (route) => route.fulfill({ json: noModels(models!) }));
  await phone.goto(`/workflows/${id}`);
  const flow = phone.getByTestId("workflow-mobile-flow");
  await expect(flow).toBeVisible({ timeout: 20_000 });
  await flow.getByRole("button", { name: /問い合わせを判定/ }).filter({ hasText: "問い合わせを判定" }).click();
  const sheet = phone.getByRole("dialog");
  await expect(sheet.getByText("使える判定用のモデルがありません。管理者に相談してください")).toBeVisible();
  expect(await sheet.evaluate((el) => el.scrollWidth <= el.clientWidth + 1)).toBe(true);
  if (SHOTS) await phone.screenshot({ path: `${SHOTS}/workflow-decide-model-none-phone.png` });
  await phone.context().close();
});

/// スマホ幅（設定は下からのシート）: シートの中に広げた一覧でも既定に従う行を選べ、横にはみ出さない。
test("スマホ: シートの中の一覧で「組織の既定に従う」を選べる", async ({ browser, page }) => {
  test.setTimeout(180_000);
  const phone = await phonePage(browser, page);
  const models = await workflowModels(page);
  test.skip(!models || models.models.length === 0, "LLM が構成されていない（使えるモデルがない）環境");
  const id = await createLlmFlow(page, models!.models[0].id);
  await phone.goto(`/workflows/${id}`);
  const flow = phone.getByTestId("workflow-mobile-flow");
  await expect(flow).toBeVisible({ timeout: 15_000 });

  await flow.getByRole("button", { name: /要約する/ }).filter({ hasText: "要約する" }).click();
  const sheet = phone.getByRole("dialog");
  await sheet.getByTestId("workflow-model-button").click();
  const picker = sheet.getByTestId("workflow-model-picker");
  await expect(picker).toBeVisible();
  await picker.getByTestId("model-default-option").click();
  await expect(picker).toHaveCount(0);
  await expect(sheet.getByTestId("workflow-model-button")).toContainText(`組織の既定（${orgDefaultLabel(models!)}）`);
  expect(await sheet.evaluate((el) => el.scrollWidth <= el.clientWidth + 1)).toBe(true);
  await expect(sheet).toBeVisible();
});

/// ログインは PC の画面で済ませ、そのセッションをタッチ端末の文脈へ持ち込む（chat-mobile.spec と同じ）。
async function phonePage(browser: Browser, page: Page): Promise<Page> {
  await loginViaKeycloak(page);
  const ctx = await browser.newContext({
    storageState: await page.context().storageState(),
    viewport: { width: 390, height: 844 },
    hasTouch: true,
    isMobile: true,
    deviceScaleFactor: 3,
    ignoreHTTPSErrors: true,
  });
  return ctx.newPage();
}
