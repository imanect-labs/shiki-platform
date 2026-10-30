import { expect, test, type Page } from "@playwright/test";

import { cleanupAll, loginViaKeycloak, removeWorkflowApi, stubDecisionModel, stubLlmDecisionModel, uniqueName } from "./helpers";

/// 「AI で判定」（ai.decide）で AI が自信の低い答えを返したことを、実行履歴の詳細で作る人に見せる e2e（BUG-124・D-114）。
///
/// AI が自分の答えに、どの答えも同じくらいと見たときの値（はい／いいえなら 50%）より低い自信を付けると、判定は AI の
/// 自信をその値まで引き上げて進む（D-78 (3)）。引き上げた質問は結果の raised_questions に載り、実行履歴の詳細の
/// そのブロックに「AI が自信の低い答えを返しました（引き上げた質問: 2 件）」と質問ごとの値が出る。
/// stub の決定器（自分の確率を使う）は引き上げないので、同じ注意は出ない。
///
/// 前提: stub の chat モデルに判定させる判定用のモデル（`~/.config/shiki-dev/llm-stub.toml` の decide-llm-stub。
/// stub は AI の自信を 0 と答える）と、stub の決定器（decide-stub）を持つカタログ・LLM は stub。実 LLM は呼ばない。
/// 判定用のモデルと元の chat モデルを組織で公開し、finally で元に戻す。

const SHOTS = process.env.E2E_SHOTS_DIR;

type AdminModel = { id: string; enabled: boolean; modality: string; provider_id: string; decision_via?: string | null };

async function csrfHeaders(page: Page): Promise<Record<string, string>> {
  const csrf = (await page.context().cookies()).find((c) => c.name === "shiki_csrf")?.value ?? "";
  return { "x-csrf-token": csrf, "content-type": "application/json" };
}

async function adminModels(page: Page): Promise<AdminModel[]> {
  const res = await page.request.get("/api/org-admin/ai/models");
  expect(res.ok(), await res.text()).toBeTruthy();
  return ((await res.json()) as { models: AdminModel[] }).models;
}

async function setEnabled(page: Page, models: { id: string; enabled: boolean }[]) {
  if (models.length === 0) return;
  const res = await page.request.put("/api/org-admin/ai/settings", {
    headers: await csrfHeaders(page),
    data: { models },
  });
  expect(res.ok(), await res.text()).toBeTruthy();
}

const CATEGORY = "問い合わせの種類はどれか";
const ANGRY = "顧客は強い不満を示しているか";

/// 問い合わせの種類（3 択）と不満（はい／いいえ）を答えさせ、種類の AI の自信で分ける判定。
function decideIr(name: string, model: string) {
  return {
    ir_version: 1,
    name,
    display_name: name,
    triggers: [{ kind: "interactive" }],
    input_schema: { type: "object", properties: { body: { type: "string" } }, required: ["body"] },
    nodes: [
      {
        id: "triage",
        type: "ai.decide",
        label: "問い合わせを判定",
        params: {
          model,
          state: { $from: "input", path: "/body" },
          questions: [
            {
              key: "category",
              type: "choice",
              instructions: CATEGORY,
              options: [
                { key: "billing", label: "請求" },
                { key: "bug", label: "不具合" },
                { key: "howto", label: "使い方" },
              ],
            },
            { key: "angry", type: "boolean", instructions: ANGRY },
          ],
          route: { question: "category", by: "confidence", min_confidence: 0.8 },
        },
      },
      { id: "on_out", type: "control.join", label: "確かなとき", params: {} },
      { id: "on_uncertain", type: "control.join", label: "要確認のとき", params: {} },
    ],
    edges: [
      { from: "triage", from_port: "out", to: "on_out" },
      { from: "triage", from_port: "uncertain", to: "on_uncertain" },
    ],
  };
}

type Output = { raised_questions?: string[]; answers?: Record<string, { confidence?: number }>; provider?: string };

/// 作って実行し、終わるまで待つ。判定の step の出力を返す。
async function createAndRun(page: Page, model: string, ids: string[]): Promise<{ id: string; runId: string; output: Output }> {
  const created = await page.request.post("/api/workflows", {
    headers: await csrfHeaders(page),
    data: { ir: decideIr(uniqueName("wf-decide-raised"), model) },
  });
  expect(created.status(), await created.text()).toBe(201);
  const id = ((await created.json()) as { id: string }).id;
  ids.push(id);
  const started = await page.request.post(`/api/workflows/${id}/runs`, {
    headers: await csrfHeaders(page),
    data: { input: { body: "請求書の金額が違います。至急直してください。" } },
  });
  expect(started.status(), await started.text()).toBe(202);
  const runId = ((await started.json()) as { run_id: string }).run_id;
  const detail = async () =>
    (await (await page.request.get(`/api/workflows/${id}/runs/${runId}`)).json()) as { status: string; steps: unknown };
  await expect
    .poll(async () => (await detail()).status, { timeout: 60_000, intervals: [1_000] })
    .toMatch(/^(succeeded|failed|cancelled)$/);
  const finished = await detail();
  expect(finished.status, JSON.stringify(finished.steps)).toBe("succeeded");
  const step = await page.request.get(`/api/workflows/${id}/runs/${runId}/steps?path=triage`);
  expect(step.ok(), await step.text()).toBeTruthy();
  return { id, runId, output: ((await step.json()) as { output: Output }).output };
}

const TITLE = "AI が自信の低い答えを返しました（引き上げた質問: 2 件）";
/// 引き上げた質問ごとの行（質問は「AI に尋ねること」の文・値は %）。
const RAISED_ITEMS = [
  `「${CATEGORY}」: AI の自信が 33.3% より低かったため、33.3% に引き上げました`,
  `「${ANGRY}」: AI の自信が 50% より低かったため、50% に引き上げました`,
];
const CALIBRATION = "AI の自信（較正なし）は、正解の確率ではありません。";

test("AI で判定: AI が自信の低い答えを返すと、実行履歴の詳細で引き上げた質問を言う（BUG-124・D-114）", async ({
  browser,
  page,
}) => {
  test.setTimeout(240_000);
  await page.setViewportSize({ width: 1440, height: 900 });
  await loginViaKeycloak(page);
  const models = await adminModels(page);
  const low = await stubLlmDecisionModel(page, models);
  const plain = await stubDecisionModel(page, models);
  test.skip(!low || !plain, "stub の chat モデルに判定させる判定用のモデル（decide-llm-stub）か stub の決定器がカタログに無い構成");
  const via = models.find((m) => m.id === low!.decision_via);
  expect(via, `元の chat モデル ${low!.decision_via} がカタログに無い`).toBeTruthy();
  // 公開していなかったものだけを公開し、finally でそれだけを戻す。
  const opened = [low!, via!, plain!].filter((m) => !m.enabled).map((m) => m.id);
  const ids: string[] = [];
  try {
    await setEnabled(page, opened.map((id) => ({ id, enabled: true })));

    // stub の AI は自信を 0 と答える → 3 択は 1/3、はい／いいえは 0.5 へ引き上げ、質問の順で結果に載る。
    const raised = await createAndRun(page, low!.id, ids);
    expect(raised.output.provider).toBe("llm_decider");
    expect(raised.output.raised_questions).toEqual(["category", "angry"]);
    expect(raised.output.answers?.category?.confidence).toBeCloseTo(1 / 3);
    expect(raised.output.answers?.angry?.confidence).toBeCloseTo(0.5);

    // 実行履歴の詳細で判定のブロックを開くと、注意と質問ごとの値が出る。
    await page.goto(`/workflows/${raised.id}/runs?run=${raised.runId}`);
    const sheet = page.getByRole("dialog");
    // 種類の AI の自信 0.33 はしきい値 0.80 に届かないので「要確認」へ進む（引き上げた答えでも同じ規則）。
    const row = sheet.getByRole("button", { name: /問い合わせを判定/ });
    await expect(row.getByText("要確認", { exact: true })).toBeVisible({ timeout: 20_000 });
    await row.click();
    const note = sheet.getByTestId("decide-raised");
    await expect(note).toBeVisible({ timeout: 20_000 });
    await expect(note.getByText(TITLE, { exact: true })).toBeVisible();
    await expect(note.getByRole("listitem")).toHaveText(RAISED_ITEMS);
    await expect(note.getByText(CALIBRATION, { exact: false })).toBeVisible();
    // 結果の全体にも項目として出る（後ろのブロックや条件で使える）。
    await expect(sheet.getByText(/"raised_questions": \[/)).toBeVisible();
    await note.scrollIntoViewIfNeeded();
    if (SHOTS) await page.screenshot({ path: `${SHOTS}/decide-raised-pc.png` });

    // stub の決定器は自分の確率を使い、引き上げない → 注意は出ない。
    const native = await createAndRun(page, plain!.id, ids);
    expect(native.output.raised_questions).toEqual([]);
    await page.goto(`/workflows/${native.id}/runs?run=${native.runId}`);
    await sheet.getByRole("button", { name: /問い合わせを判定/ }).click();
    await expect(sheet.getByText(/"raised_questions": \[\]/)).toBeVisible({ timeout: 20_000 });
    await expect(sheet.getByTestId("decide-raised")).toHaveCount(0);

    // スマホ幅でも、注意が横にはみ出さずに読める。
    const phoneCtx = await browser.newContext({
      storageState: await page.context().storageState(),
      viewport: { width: 390, height: 844 },
      hasTouch: true,
      isMobile: true,
      deviceScaleFactor: 3,
      ignoreHTTPSErrors: true,
    });
    try {
      const phone = await phoneCtx.newPage();
      await phone.goto(`/workflows/${raised.id}/runs?run=${raised.runId}`);
      const phoneSheet = phone.getByRole("dialog");
      await phoneSheet.getByRole("button", { name: /問い合わせを判定/ }).click();
      const phoneNote = phoneSheet.getByTestId("decide-raised");
      await expect(phoneNote.getByText(TITLE, { exact: true })).toBeVisible({ timeout: 20_000 });
      await phoneNote.scrollIntoViewIfNeeded();
      expect(await phone.evaluate(() => document.documentElement.scrollWidth)).toBeLessThanOrEqual(390);
      expect(await phoneNote.evaluate((el) => el.scrollWidth <= el.clientWidth + 1)).toBe(true);
      if (SHOTS) await phone.screenshot({ path: `${SHOTS}/decide-raised-phone.png` });
    } finally {
      await phoneCtx.close();
    }
  } finally {
    await cleanupAll(
      ...ids.map((id) => () => removeWorkflowApi(page, id)),
      () => setEnabled(page, opened.map((id) => ({ id, enabled: false }))),
    );
  }
});
