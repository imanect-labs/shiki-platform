import { expect, test, type Browser, type Page } from "@playwright/test";

import { loginViaKeycloak, stubChatModelId, uniqueName } from "./helpers";

/// 「AI に聞く」の出力の形（llm.invoke の output_schema・effort・GAP-13）を、サーバの保存・検証・実行の経路で確かめる。
///
/// - 保存時の検証: 形の誤りは `/params/output_schema/...` の位置で返り、上限トークンが小さいと警告が出る。
/// - 実行: 形に合う出力は step の出力の data に載る。形に合わない出力は 1 回だけ直させて成功し、直しても合わなければ
///   llm_output_invalid で失敗する（stub の json: / jsonalways: の決まった応答）。
/// - 画面: 後ろのブロックの「差し込む」の候補に data が出る（カタログの output_fields から生成）。
/// 前提: compose の shiki-server が SHIKI__WORKFLOW__ENABLED=true・LLM は stub。実 LLM に送らないよう、モデルは
/// 組織で使える stub のモデルを引いて明示する（組織の既定が実 LLM でも使わない。id は環境で違うので決め打ちしない・
/// 手元は default・CI の compose は shiki-default・BUG-114）。

async function csrfHeader(page: Page): Promise<Record<string, string>> {
  const csrf = (await page.context().cookies()).find((c) => c.name === "shiki_csrf")?.value ?? "";
  return { "x-csrf-token": csrf, "content-type": "application/json" };
}

/// 問い合わせの種類と要約を決める出力の形。
const SCHEMA = {
  type: "object",
  required: ["category", "summary"],
  properties: {
    category: { type: "string", enum: ["billing", "bug", "howto"] },
    summary: { type: "string" },
  },
};

function structuredIr(name: string, model: string, params: Record<string, unknown> = {}) {
  return {
    ir_version: 1,
    name,
    display_name: name,
    triggers: [{ kind: "interactive" }],
    input_schema: { type: "object", properties: { body: { type: "string" } }, required: ["body"] },
    nodes: [
      {
        id: "triage",
        type: "llm.invoke",
        label: "種類を判定",
        params: {
          model,
          prompt: { $from: "input", path: "/body" },
          effort: "low",
          output_schema: SCHEMA,
          ...params,
        },
      },
      {
        id: "note",
        type: "llm.invoke",
        label: "結果を整える",
        params: { model, prompt: "判定の結果を短く書き出してください" },
      },
    ],
    edges: [{ from: "triage", from_port: "out", to: "note" }],
  };
}

async function createFlow(page: Page): Promise<string> {
  const name = uniqueName("wf-structured");
  const res = await page.request.post("/api/workflows", {
    headers: await csrfHeader(page),
    data: { ir: structuredIr(name, await stubChatModelId(page)) },
  });
  expect(res.ok(), await res.text()).toBeTruthy();
  return ((await res.json()) as { id: string }).id;
}

/// テストで作ったワークフローを片付ける（もう消えているものは飛ばす）。
async function cleanup(page: Page, ids: string[]) {
  const headers = await csrfHeader(page);
  for (const id of ids) {
    const res = await page.request.delete(`/api/artifacts/${id}`, { headers });
    expect([204, 404], `DELETE /artifacts/${id} → ${res.status()}`).toContain(res.status());
  }
}

type Step = { status: string; output: Record<string, unknown>; error: { code?: string; message?: string } | null };

/// 入力を渡して実行し、判定の step が終わるまで待つ。
async function runTriage(page: Page, id: string, body: string): Promise<Step> {
  const start = await page.request.post(`/api/workflows/${id}/runs`, {
    headers: await csrfHeader(page),
    data: { input: { body } },
  });
  expect(start.status(), await start.text()).toBe(202);
  const runId = ((await start.json()) as { run_id: string | null }).run_id;
  expect(runId).toBeTruthy();
  let step: Step | null = null;
  await expect
    .poll(
      async () => {
        const res = await page.request.get(`/api/workflows/${id}/runs/${runId}/steps?path=triage`);
        if (!res.ok()) return "pending";
        step = (await res.json()) as Step;
        return step.status;
      },
      { timeout: 60_000 },
    )
    .toMatch(/^(succeeded|failed)$/);
  return step!;
}

test("出力の形: 保存時に形を検査し、上限トークンが小さいと警告する", async ({ page }) => {
  test.setTimeout(120_000);
  await loginViaKeycloak(page);
  const model = await stubChatModelId(page);
  const validate = async (params: Record<string, unknown>) => {
    const res = await page.request.post("/api/workflows/validate", {
      headers: await csrfHeader(page),
      data: { ir: structuredIr(uniqueName("wf-structured-check"), model, params) },
    });
    expect(res.ok(), await res.text()).toBeTruthy();
    return (await res.json()) as {
      errors: { code: string; path?: string; message: string }[];
      warnings: { code: string; path?: string }[];
    };
  };
  const ok = await validate({});
  expect(ok.errors).toEqual([]);
  expect(ok.warnings).toEqual([]);

  const bad = await validate({
    output_schema: { type: "object", properties: { category: { type: ["string", "integer"] } } },
  });
  expect(bad.errors).toHaveLength(1);
  expect(bad.errors[0].code).toBe("ir.schema_violation");
  expect(bad.errors[0].path).toBe("/params/output_schema/properties/category/type");

  const open = await validate({ output_schema: { ...SCHEMA, additionalProperties: true } });
  expect(open.errors[0].path).toBe("/params/output_schema/additionalProperties");

  const few = await validate({ max_tokens: 256 });
  expect(few.errors).toEqual([]);
  expect(few.warnings.map((w) => w.code)).toEqual(["ir.output_schema_few_tokens"]);
  expect(few.warnings[0].path).toBe("/params/max_tokens");
});

test("出力の形: 形に合う出力は data に載り、合わない出力は 1 回直させ、直しても合わなければ失敗する", async ({
  page,
}) => {
  test.setTimeout(180_000);
  await loginViaKeycloak(page);
  const id = await createFlow(page);
  try {
    // stub は prompt に現れた選択肢を選ぶ。
    const ok = await runTriage(page, id, "アプリが落ちる bug の報告です");
    expect(ok.status, JSON.stringify(ok.error)).toBe("succeeded");
    expect(ok.output.model).toBe(await stubChatModelId(page));
    expect(ok.output.data).toEqual({ category: "bug", summary: "" });
    expect(JSON.parse(ok.output.text as string)).toEqual(ok.output.data);
    expect(ok.output).not.toHaveProperty("stop_reason");

    // 1 回目は JSON でない → 直させて形に合う値になる。
    const repaired = await runTriage(page, id, "json:申し訳ありません、分類できません");
    expect(repaired.status, JSON.stringify(repaired.error)).toBe("succeeded");
    expect((repaired.output.data as { category: string }).category).toBe("billing");

    // 直しても合わない → 再試行しない失敗（理由は日本語で、どの項目かが分かる）。
    const invalid = await runTriage(page, id, 'jsonalways:{"category":"refund"}');
    expect(invalid.status).toBe("failed");
    expect(invalid.error?.code).toBe("llm_output_invalid");
    expect(invalid.error?.message).toContain("category: 選択肢にない値です");
  } finally {
    await cleanup(page, [id]);
  }
});

test("出力の形: 後ろのブロックの「差し込む」の候補に data が出る", async ({ page }) => {
  test.setTimeout(120_000);
  await loginViaKeycloak(page);
  const id = await createFlow(page);
  try {
    await page.setViewportSize({ width: 1440, height: 900 });
    await page.goto(`/workflows/${id}`);
    const node = page.getByTestId("rf__node-note");
    await expect(node).toBeVisible({ timeout: 15_000 });
    await settleViewport(page);
    await node.click({ timeout: 15_000 });
    const panel = page.getByRole("complementary", { name: "ブロックの設定" });
    await panel
      .locator('[data-value-expr="AI への指示"]')
      .getByRole("button", { name: "差し込む" })
      .click({ timeout: 15_000 });
    for (const key of ["text", "model", "data"]) {
      await expect(page.getByRole("menuitem", { name: `「種類を判定」の ${key}`, exact: true })).toBeVisible();
    }
    await page.screenshot({ path: "test-results/structured-output-refs-pc.png" });
  } finally {
    await cleanup(page, [id]);
  }
});

/// スマホ幅（設定は下からのシート）でも、差し込む候補に data が出て、横にはみ出さない。
test("出力の形: スマホのシートでも差し込む候補に data が出る", async ({ browser, page }) => {
  test.setTimeout(180_000);
  const phone = await phonePage(browser, page);
  const id = await createFlow(page);
  try {
    await phone.goto(`/workflows/${id}`);
    const flow = phone.getByTestId("workflow-mobile-flow");
    await expect(flow).toBeVisible({ timeout: 15_000 });
    await flow
      .getByRole("button", { name: /結果を整える/ })
      .filter({ hasText: "結果を整える" })
      .click({ timeout: 15_000 });
    const sheet = phone.getByRole("dialog");
    await expect(sheet).toBeVisible();
    expect(await sheet.evaluate((el) => el.scrollWidth <= el.clientWidth + 1)).toBe(true);
    await sheet
      .locator('[data-value-expr="AI への指示"]')
      .getByRole("button", { name: "差し込む" })
      .click({ timeout: 15_000 });
    // メニューが開いている間はシートが支援技術から隠れる（モーダルのメニュー）ので、画面全体ではみ出しを見る。
    await expect(phone.getByRole("menuitem", { name: "「種類を判定」の data", exact: true })).toBeVisible();
    expect(await phone.evaluate(() => document.documentElement.scrollWidth <= window.innerWidth + 1)).toBe(true);
    await phone.screenshot({ path: "test-results/structured-output-refs-phone.png" });
  } finally {
    await cleanup(page, [id]);
    await phone.context().close();
  }
});

/// ログインは PC の画面で済ませ、そのセッションをタッチ端末の文脈へ持ち込む（workflow-model-default.spec と同じ）。
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

/// キャンバスの表示位置（fitView のアニメーション）が落ち着くまで待つ（workflows-editor.spec と同じ）。
async function settleViewport(page: Page) {
  const read = () => page.locator(".react-flow__viewport").evaluate((el) => (el as HTMLElement).style.transform);
  let prev = await read();
  await expect
    .poll(
      async () => {
        const cur = await read();
        const still = cur === prev;
        prev = cur;
        return still;
      },
      { intervals: [200] },
    )
    .toBe(true);
}
