import { expect, test, type Browser, type Locator, type Page } from "@playwright/test";

import { loginViaKeycloak, uniqueName } from "./helpers";

/// 「AI で判定」の答えの使い回し（決定キャッシュ・step 属性 `cache`・FEAT-1 の T10）。
///
/// - 既定は使わない。フォームの切り替えで `cache: { ttl_sec }` を書き、期間を選べる。切ると属性ごと外れる。
/// - 使い回すと、同じワークフローの 2 回目の run は前の答えを使い（出力の cached が true・同じ出口）、同じ IR を
///   別のワークフローとして保存したものでは使い回さない。
/// - ほかの種に cache を書いた IR は保存できない（ir.cache_unsupported）。
/// 前提: 判定用のモデル（stub の決定器・`~/.config/shiki-dev/llm-stub.toml` の decide-stub）を持つカタログ・LLM は
/// stub・SHIKI__WORKFLOW__ENABLED=true。判定用のモデルを組織で公開し、finally で元に戻す。実 LLM は呼ばない
/// （元にする chat モデルを持たない stub の決定器だけを使う）。作ったワークフローは最後に消す。

type AdminModel = { id: string; enabled: boolean; modality: "chat" | "decision"; decision_via?: string | null };

async function csrfHeader(page: Page): Promise<Record<string, string>> {
  const csrf = (await page.context().cookies()).find((c) => c.name === "shiki_csrf")?.value ?? "";
  return { "x-csrf-token": csrf, "content-type": "application/json" };
}

async function adminModels(page: Page): Promise<AdminModel[]> {
  const res = await page.request.get("/api/org-admin/ai/models");
  expect(res.ok(), await res.text()).toBeTruthy();
  return ((await res.json()) as { models: AdminModel[] }).models;
}

async function setEnabled(page: Page, models: { id: string; enabled: boolean }[]) {
  const res = await page.request.put("/api/org-admin/ai/settings", {
    headers: await csrfHeader(page),
    data: { models },
  });
  expect(res.ok(), await res.text()).toBeTruthy();
}

/// 問い合わせの種類で振り分ける判定（答えで分ける）と、出口ごとの合流ノード。
function decideIr(name: string, model: string, cache?: { ttl_sec: number }) {
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
        ...(cache ? { cache } : {}),
        params: {
          model,
          state: { $from: "input", path: "/body" },
          questions: [
            {
              key: "category",
              type: "choice",
              instructions: "問い合わせの種類はどれか",
              options: [
                { key: "billing", description: "請求・返金" },
                { key: "bug", description: "不具合" },
              ],
            },
          ],
          route: { question: "category", min_confidence: 0.8 },
        },
      },
      { id: "on_billing", type: "control.join", label: "請求の対応", params: {} },
      { id: "on_bug", type: "control.join", label: "不具合の対応", params: {} },
      { id: "on_uncertain", type: "control.join", label: "人が確認", params: {} },
    ],
    edges: [
      { from: "triage", from_port: "billing", to: "on_billing" },
      { from: "triage", from_port: "bug", to: "on_bug" },
      { from: "triage", from_port: "uncertain", to: "on_uncertain" },
    ],
  };
}

async function createWorkflow(page: Page, made: string[], ir: unknown): Promise<string> {
  const res = await page.request.post("/api/workflows", { headers: await csrfHeader(page), data: { ir } });
  expect(res.status(), await res.text()).toBe(201);
  const id = ((await res.json()) as { id: string }).id;
  made.push(id);
  return id;
}

async function cleanup(page: Page, made: string[]) {
  const headers = await csrfHeader(page);
  for (const id of made.reverse()) {
    const res = await page.request.delete(`/api/artifacts/${id}`, { headers });
    expect.soft([200, 204, 404], `/api/artifacts/${id}: ${res.status()}`).toContain(res.status());
  }
}

async function savedNode(page: Page, id: string) {
  const res = await page.request.get(`/api/workflows/${id}`);
  expect(res.ok(), await res.text()).toBeTruthy();
  const ir = (await res.json()) as { ir: { nodes: { id: string; cache?: { ttl_sec: number } | null }[] } };
  return ir.ir.nodes.find((n) => n.id === "triage")!;
}

type Run = { status: string; steps: { node_id: string; status: string; taken_ports: string[] }[] };

/// 入力を渡して実行し、run が終わるまで待って、判定の step の出力を返す。
async function runDecide(page: Page, id: string, body: string): Promise<{ run: Run; cached: boolean }> {
  const start = await page.request.post(`/api/workflows/${id}/runs`, {
    headers: await csrfHeader(page),
    data: { input: { body } },
  });
  expect(start.status(), await start.text()).toBe(202);
  const runId = ((await start.json()) as { run_id: string | null }).run_id!;
  let run: Run | null = null;
  await expect
    .poll(
      async () => {
        const res = await page.request.get(`/api/workflows/${id}/runs/${runId}`);
        if (!res.ok()) return "pending";
        run = (await res.json()) as Run;
        return run.status;
      },
      { timeout: 60_000 },
    )
    .toMatch(/^(succeeded|failed|cancelled)$/);
  const step = await page.request.get(`/api/workflows/${id}/runs/${runId}/steps?path=triage`);
  expect(step.ok(), await step.text()).toBeTruthy();
  const output = ((await step.json()) as { output: { cached: boolean } }).output;
  return { run: run!, cached: output.cached };
}

async function choose(page: Page, trigger: Locator, option: string | RegExp) {
  await trigger.click();
  await page.getByRole("option", { name: option, exact: typeof option === "string" }).click();
}

/// ログインは PC の画面で済ませ、そのセッションをタッチ端末の文脈へ持ち込む（workflow-decide-editor.spec と同じ）。
async function phonePage(browser: Browser, page: Page): Promise<Page> {
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

test.describe("AI で判定の答えの使い回し（決定キャッシュ）", () => {
  let decision: AdminModel | undefined;

  test.beforeEach(async ({ page }) => {
    await loginViaKeycloak(page);
    // 元にする chat モデルを持たない判定用のモデル（stub の決定器）だけを使う（実 LLM を呼ばない）。
    decision = (await adminModels(page)).find((m) => m.modality === "decision" && !m.decision_via);
    test.skip(!decision, "判定用のモデル（stub の決定器）がカタログに無い構成");
  });

  test("フォームで使い回しを入れて保存すると、2 回目の run は前の答えを使う。切ると属性ごと外れる", async ({ page }) => {
    test.setTimeout(240_000);
    const d = decision!;
    const made: string[] = [];
    try {
      if (!d.enabled) await setEnabled(page, [{ id: d.id, enabled: true }]);
      const id = await createWorkflow(page, made, decideIr(uniqueName("wf-decide-cache"), d.id));
      await page.setViewportSize({ width: 1440, height: 1000 });
      await page.goto(`/workflows/${id}`);
      const node = page.getByTestId("rf__node-triage");
      await expect(node).toBeVisible({ timeout: 15_000 });
      await node.click();
      const panel = page.getByRole("complementary", { name: "ブロックの設定" });
      const section = panel.getByTestId("decide-cache");
      await section.scrollIntoViewIfNeeded();
      const toggle = section.getByRole("switch", { name: "同じ内容なら答えを使い回す" });
      // 既定は使わない（期間の欄も出さない）。
      await expect(toggle).toHaveAttribute("aria-checked", "false");
      await expect(section.getByTestId("decide-cache-ttl")).toHaveCount(0);
      await expect(section).toContainText("毎回 AI に判定させます");

      // キーボードで入れる（既定の 1 日）→ 7 日へ。
      await toggle.focus();
      await page.keyboard.press("Space");
      await expect(toggle).toHaveAttribute("aria-checked", "true");
      const ttl = section.getByRole("combobox", { name: "使い回す期間" });
      await expect(ttl).toContainText("1 日");
      await choose(page, ttl, "7 日");
      await expect(ttl).toContainText("7 日");
      await expect(section).toContainText("組織のモデルの許可は毎回確かめます");
      await page.screenshot({ path: "test-results/decide-cache-pc.png" });
      await page.getByRole("button", { name: "保存", exact: true }).click();
      await expect(page.getByText(/保存しました/).first()).toBeVisible({ timeout: 15_000 });
      expect((await savedNode(page, id)).cache).toEqual({ ttl_sec: 7 * 86_400 });

      // 開き直しても残る。
      await page.reload();
      await page.getByTestId("rf__node-triage").click();
      const again = page.getByRole("complementary", { name: "ブロックの設定" }).getByTestId("decide-cache");
      await expect(again.getByRole("switch", { name: "同じ内容なら答えを使い回す" })).toHaveAttribute(
        "aria-checked",
        "true",
      );
      await expect(again.getByRole("combobox", { name: "使い回す期間" })).toContainText("7 日");

      // 1 回目は判定し、2 回目は前の答えを使う（同じ出口）。
      const first = await runDecide(page, id, "billing の件で連絡しました");
      expect(first.run.status).toBe("succeeded");
      expect(first.cached).toBe(false);
      const second = await runDecide(page, id, "billing の件で連絡しました");
      expect(second.run.status).toBe("succeeded");
      expect(second.cached).toBe(true);
      expect(second.run.steps.find((s) => s.node_id === "triage")?.taken_ports).toEqual(["billing"]);
      // 材料が違えば使い回さない。
      expect((await runDecide(page, id, "不具合です bug")).cached).toBe(false);

      // 同じ IR を別のワークフローとして保存したものでは使い回さない（範囲はワークフローごと）。
      const other = await createWorkflow(
        page,
        made,
        decideIr(uniqueName("wf-decide-cache-other"), d.id, { ttl_sec: 7 * 86_400 }),
      );
      expect((await runDecide(page, other, "billing の件で連絡しました")).cached).toBe(false);

      // 切ると属性ごと外れる（既定に戻る）。
      await again.getByRole("switch", { name: "同じ内容なら答えを使い回す" }).click();
      await expect(again.getByTestId("decide-cache-ttl")).toHaveCount(0);
      await page.getByRole("button", { name: "保存", exact: true }).click();
      await expect(page.getByText(/保存しました/).first()).toBeVisible({ timeout: 15_000 });
      expect((await savedNode(page, id)).cache ?? null).toBeNull();
      expect((await runDecide(page, id, "billing の件で連絡しました")).cached).toBe(false);
    } finally {
      await cleanup(page, made);
      if (!d.enabled) await setEnabled(page, [{ id: d.id, enabled: false }]);
    }
  });

  test("判定でないブロックに cache を書いた IR は保存できない", async ({ page }) => {
    const res = await page.request.post("/api/workflows/validate", {
      headers: await csrfHeader(page),
      data: {
        ir: {
          ir_version: 1,
          name: uniqueName("wf-cache-bad"),
          nodes: [{ id: "wait", type: "control.join", params: {}, cache: { ttl_sec: 3600 } }],
          edges: [],
        },
      },
    });
    expect(res.ok(), await res.text()).toBeTruthy();
    const body = (await res.json()) as { errors: { code: string; node_id?: string; path?: string }[] };
    expect(body.errors.map((e) => [e.code, e.node_id, e.path])).toEqual([["ir.cache_unsupported", "wait", "/cache"]]);
  });

  test("スマホ: 設定のシートで使い回しを入れられ、横にはみ出さない", async ({ browser, page }) => {
    test.setTimeout(180_000);
    const d = decision!;
    const made: string[] = [];
    const phone = await phonePage(browser, page);
    try {
      if (!d.enabled) await setEnabled(page, [{ id: d.id, enabled: true }]);
      const id = await createWorkflow(page, made, decideIr(uniqueName("wf-decide-cache-phone"), d.id, { ttl_sec: 3600 }));
      await phone.goto(`/workflows/${id}`);
      const flow = phone.getByTestId("workflow-mobile-flow");
      await expect(flow).toBeVisible({ timeout: 15_000 });
      await flow.getByRole("button", { name: /問い合わせを判定/ }).filter({ hasText: "問い合わせを判定" }).click();
      const sheet = phone.getByRole("dialog");
      await expect(sheet).toBeVisible();
      const section = sheet.getByTestId("decide-cache");
      await section.scrollIntoViewIfNeeded();
      await expect(section.getByRole("switch", { name: "同じ内容なら答えを使い回す" })).toHaveAttribute(
        "aria-checked",
        "true",
      );
      await expect(section.getByRole("combobox", { name: "使い回す期間" })).toContainText("1 時間");
      expect(await sheet.evaluate((el) => el.scrollWidth <= el.clientWidth + 1)).toBe(true);
      expect(await phone.evaluate(() => document.documentElement.scrollWidth <= window.innerWidth + 1)).toBe(true);
      await phone.screenshot({ path: "test-results/decide-cache-phone.png" });
    } finally {
      await cleanup(page, made);
      if (!d.enabled) await setEnabled(page, [{ id: d.id, enabled: false }]);
      await phone.context().close();
    }
  });
});
