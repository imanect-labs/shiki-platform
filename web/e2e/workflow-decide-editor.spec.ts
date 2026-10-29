import { expect, test, type Browser, type Locator, type Page } from "@playwright/test";

import { loginViaKeycloak, stubDecisionModel, uniqueName } from "./helpers";

/// 「AI で判定」（ai.decide・FEAT-1 の T6）のエディタの画面を確かめる。
///
/// - 出口のハンドルは検証 API の `node_ports`（サーバの導出）で描く。振り分けを変えると、検証の後にハンドルが
///   変わり、今の設定に無い出口から出るエッジは「無い出口」として描かれる（選んで消せる）。
/// - 「要確認」（uncertain）の出口から承認待ち（human.approval）へつないで保存できる（D-7）。
/// - パレットの「AI で判定」から置き、フォームだけで組んだ判定を保存して実行すると、stub の決定器で想定の出口へ進む。
/// - 確からしさが較正されていないこと（較正なし）を常に出す（D-52 の H4）。
/// 前提: 判定用のモデル（stub の決定器・`~/.config/shiki-dev/llm-stub.toml` の decide-stub）を持つカタログ・LLM は
/// stub・SHIKI__WORKFLOW__ENABLED=true。判定用のモデルを組織で公開し、finally で元に戻す。実 LLM は呼ばない
/// （元にする chat モデルを持たない stub の決定器だけを使う）。作った表・状態の流れ・ワークフローは最後に消す。

type AdminModel = { id: string; enabled: boolean; modality: "chat" | "decision"; provider_id: string; decision_via?: string | null };

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

type Made = { tables: string[]; artifacts: string[] };

async function cleanup(page: Page, made: Made) {
  const headers = await csrfHeader(page);
  for (const id of [...made.artifacts.reverse(), ...made.tables.reverse().map((t) => `table:${t}`)]) {
    const path = id.startsWith("table:") ? `/api/data/tables/${id.slice(6)}` : `/api/artifacts/${id}`;
    const res = await page.request.delete(path, { headers });
    expect.soft([200, 204, 404], `${path}: ${res.status()}`).toContain(res.status());
  }
}

/// 問い合わせの種類で振り分ける判定（答えで分ける）と、出口ごとの合流ノード。
function decideNode(model: string, route: Record<string, unknown> | undefined, state: unknown) {
  return {
    id: "triage",
    type: "ai.decide",
    label: "問い合わせを判定",
    params: {
      model,
      state,
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
      ...(route ? { route } : {}),
    },
  };
}

async function createWorkflow(page: Page, made: Made, ir: Record<string, unknown>): Promise<string> {
  const res = await page.request.post("/api/workflows", { headers: await csrfHeader(page), data: { ir } });
  expect(res.status(), await res.text()).toBe(201);
  const id = ((await res.json()) as { id: string }).id;
  made.artifacts.push(id);
  return id;
}

async function savedIr(page: Page, id: string) {
  const res = await page.request.get(`/api/workflows/${id}`);
  expect(res.ok(), await res.text()).toBeTruthy();
  return ((await res.json()) as {
    ir: {
      nodes: { id: string; type: string; params: Record<string, unknown> }[];
      edges: { from: string; from_port?: string | null; to: string }[];
    };
  }).ir;
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

/// ノードの出口のハンドルの名前（描いた順）。
function handleIds(node: Locator) {
  return node
    .locator(".react-flow__handle.source")
    .evaluateAll((els) => els.map((e) => e.getAttribute("data-handleid") ?? ""));
}

/// 出口のハンドルが、その出口の名前の行の横にある（縦の中心が行の中に入る）。ずれると別の出口からつないでしまう。
async function expectHandlesBesideLabels(node: Locator) {
  const misplaced = await node.locator("[data-port]").evaluateAll((rows) =>
    rows.flatMap((row) => {
      const port = row.getAttribute("data-port") ?? "";
      const handle = row.ownerDocument.querySelector(
        `[data-id="${row.closest(".react-flow__node")?.getAttribute("data-id")}"] .react-flow__handle.source[data-handleid="${port}"]`,
      );
      if (!handle) return [`${port}: ハンドルが無い`];
      const r = row.getBoundingClientRect();
      const h = handle.getBoundingClientRect();
      const cy = h.top + h.height / 2;
      return cy >= r.top && cy <= r.bottom ? [] : [`${port}: 行 ${r.top}-${r.bottom}・ハンドル ${cy}`];
    }),
  );
  expect(misplaced).toEqual([]);
}

async function choose(page: Page, trigger: Locator, option: string | RegExp) {
  await trigger.click();
  await page.getByRole("option", { name: option, exact: typeof option === "string" }).click();
}

test.describe("AI で判定（エディタ）", () => {
  let decision: AdminModel | undefined;

  test.beforeEach(async ({ page }) => {
    await loginViaKeycloak(page);
    // 元にする chat モデルを持たない判定用のモデル（stub の決定器）だけを使う（実 LLM を呼ばない）。
    decision = await stubDecisionModel(page, await adminModels(page));
    test.skip(!decision, "判定用のモデル（stub の決定器）がカタログに無い構成");
  });

  test("振り分けを変えると、検証の後にハンドルが変わり、無くなった出口のエッジを無い出口として描く", async ({ page }) => {
    test.setTimeout(180_000);
    const d = decision!;
    const made: Made = { tables: [], artifacts: [] };
    try {
      if (!d.enabled) await setEnabled(page, [{ id: d.id, enabled: true }]);
      const name = uniqueName("wf-decide-ports");
      const id = await createWorkflow(page, made, {
        ir_version: 1,
        name,
        display_name: name,
        triggers: [{ kind: "interactive" }],
        input_schema: { type: "object", properties: { body: { type: "string" } }, required: ["body"] },
        nodes: [
          decideNode(d.id, { question: "category", min_confidence: 0.8 }, { $from: "input", path: "/body" }),
          { id: "on_billing", type: "control.join", label: "請求の対応", params: {} },
          { id: "on_bug", type: "control.join", label: "不具合の対応", params: {} },
          { id: "on_uncertain", type: "control.join", label: "人が確認", params: {} },
        ],
        edges: [
          { from: "triage", from_port: "billing", to: "on_billing" },
          { from: "triage", from_port: "bug", to: "on_bug" },
          { from: "triage", from_port: "uncertain", to: "on_uncertain" },
        ],
      });
      await page.setViewportSize({ width: 1440, height: 1000 });
      await page.goto(`/workflows/${id}`);
      const node = page.getByTestId("rf__node-triage");
      await expect(node).toBeVisible({ timeout: 15_000 });
      // 答えで分ける: 選択肢のキーと要確認（out は無い）。サーバの導出（node_ports）の順で描く。
      await expect.poll(() => handleIds(node), { timeout: 10_000 }).toEqual(["billing", "bug", "uncertain"]);
      await expect(node.locator('[data-port="uncertain"]')).toHaveText("要確認");
      await settleViewport(page);
      await expectHandlesBesideLabels(node);
      await node.click();
      const panel = page.getByRole("complementary", { name: "ブロックの設定" });
      await expect(panel.getByTestId("decide-calibration")).toContainText("較正なし");
      await expect(panel.getByTestId("decide-exits")).toContainText("要確認");

      // 確かなときだけ進める: 検証の後に out / uncertain へ変わる。選択肢の出口から出ていたエッジは
      // 「無い出口」として描く（線を選んで消せる）。
      await choose(page, panel.getByRole("combobox", { name: "振り分け", exact: true }), "確かなときだけ進める");
      await expect.poll(() => handleIds(node), { timeout: 10_000 }).toEqual(["out", "uncertain", "billing", "bug"]);
      await expect(node.locator('[data-port="billing"]')).toHaveText("billing（無い出口）");
      await expectHandlesBesideLabels(node);
      await expect(panel.getByTestId("decide-exits")).toContainText("次へ");
      // 保存は無い出口のエッジで止まる（ir.unknown_port）。
      await page.getByRole("button", { name: "保存", exact: true }).click();
      await expect(page.getByText("保存できません（検証エラー）").first()).toBeVisible({ timeout: 15_000 });
      await page.screenshot({ path: "test-results/decide-ports-confidence-pc.png" });

      // 答えで分けるに戻すと、元の出口に戻り、無い出口は消える。
      await choose(page, panel.getByRole("combobox", { name: "振り分け", exact: true }), "答えで分ける");
      await expect.poll(() => handleIds(node), { timeout: 10_000 }).toEqual(["billing", "bug", "uncertain"]);
      await expect(node.getByText("（無い出口）")).toHaveCount(0);

      // 振り分けに使う質問の選択肢のキーを変えると、その出口から出るエッジも付け替わる（保存できる）。
      const question = panel.getByRole("group", { name: /^質問 1/ });
      await question.getByRole("textbox", { name: "選択肢 1 のキー" }).fill("invoice");
      await expect.poll(() => handleIds(node), { timeout: 10_000 }).toEqual(["invoice", "bug", "uncertain"]);
      await page.getByRole("button", { name: "保存", exact: true }).click();
      await expect(page.getByText(/保存しました/).first()).toBeVisible({ timeout: 15_000 });
      const ir = await savedIr(page, id);
      expect(ir.edges).toContainEqual({ from: "triage", from_port: "invoice", to: "on_billing" });
      await page.screenshot({ path: "test-results/decide-ports-value-pc.png" });
    } finally {
      await cleanup(page, made);
      if (!d.enabled) await setEnabled(page, [{ id: d.id, enabled: false }]);
    }
  });

  test("要確認の出口から承認待ちへつないで保存できる（D-7）", async ({ page }) => {
    test.setTimeout(180_000);
    const d = decision!;
    const made: Made = { tables: [], artifacts: [] };
    try {
      if (!d.enabled) await setEnabled(page, [{ id: d.id, enabled: true }]);
      const table = await fsmTable(page, made);
      const name = uniqueName("wf-decide-approval");
      const id = await createWorkflow(page, made, {
        ir_version: 1,
        name,
        display_name: name,
        triggers: [{ kind: "event", source: "data.transition", scope: { table } }],
        nodes: [
          decideNode(d.id, { question: "category", by: "confidence", min_confidence: 0.8 }, { $from: "trigger" }),
          { id: "auto", type: "control.join", label: "自動で処理", params: {} },
          {
            id: "review",
            type: "human.approval",
            label: "担当者が確認",
            params: {
              title: "判定の確認",
              record: { table, id: { $from: "trigger", path: "/record_id" } },
              assignees: [{ field: "approver" }],
              approve: { to: "承認" },
            },
          },
        ],
        edges: [{ from: "triage", from_port: "out", to: "auto" }],
        policies: { run_timeout_sec: 1209600 },
      });
      await page.setViewportSize({ width: 1440, height: 1000 });
      await page.goto(`/workflows/${id}`);
      const node = page.getByTestId("rf__node-triage");
      await expect(node).toBeVisible({ timeout: 15_000 });
      await expect.poll(() => handleIds(node), { timeout: 10_000 }).toEqual(["out", "uncertain"]);
      await settleViewport(page);

      // 要確認の出口のハンドルから、承認待ちのブロックの入口へドラッグしてつなぐ。
      const from = node.locator('.react-flow__handle.source[data-handleid="uncertain"]');
      const to = page.getByTestId("rf__node-review").locator(".react-flow__handle.target");
      await from.dragTo(to);
      await expect(page.locator('[data-testid="rf__edge-e:triage|uncertain|review"]')).toHaveCount(1);
      await page.getByRole("button", { name: "保存", exact: true }).click();
      await expect(page.getByText(/保存しました/).first()).toBeVisible({ timeout: 15_000 });
      const ir = await savedIr(page, id);
      expect(ir.edges).toContainEqual({ from: "triage", from_port: "uncertain", to: "review" });
      await page.screenshot({ path: "test-results/decide-uncertain-approval-pc.png" });
    } finally {
      await cleanup(page, made);
      if (!d.enabled) await setEnabled(page, [{ id: d.id, enabled: false }]);
    }
  });

  test("パレットから置いた判定をフォームで組んで保存し、stub の決定器で想定の出口へ進む", async ({ page }) => {
    test.setTimeout(240_000);
    const d = decision!;
    const made: Made = { tables: [], artifacts: [] };
    try {
      if (!d.enabled) await setEnabled(page, [{ id: d.id, enabled: true }]);
      const name = uniqueName("wf-decide-form");
      const id = await createWorkflow(page, made, {
        ir_version: 1,
        name,
        display_name: name,
        triggers: [{ kind: "interactive" }],
        input_schema: {
          type: "object",
          properties: { body: { type: "string", title: "本文" } },
          required: ["body"],
        },
        nodes: [],
        edges: [],
      });
      await page.setViewportSize({ width: 1440, height: 1000 });
      await page.goto(`/workflows/${id}`);
      await page.getByRole("button", { name: "最初のブロック" }).click();
      await page.getByRole("button", { name: /^AI で判定/ }).click();
      const node = page.getByTestId("rf__node-decide_1");
      await expect(node).toBeVisible();
      const panel = page.getByRole("complementary", { name: "ブロックの設定" });
      // 置いたばかり（モデル未選択・問いが空）でも、振り分けが無ければ出口は out に決まる（出口は振り分けに関わる
      // 項目だけから導く）。
      await expect.poll(() => handleIds(node), { timeout: 10_000 }).toEqual(["out"]);
      await expect(node.getByTestId("node-ports-pending")).toHaveCount(0);

      // 判定用のモデル（判定用のモデルだけが一覧に出る・既定の行は無い）。
      await panel.getByRole("button", { name: /^判定用のモデル: 選んでください/ }).click();
      const picker = page.getByTestId("workflow-model-picker");
      await expect(picker.getByTestId("model-default-option")).toHaveCount(0);
      await picker.getByTestId(`model-option-${d.id}`).click();

      // 判定の材料: 入力の本文を差し込む。
      await panel.locator('[data-value-expr="判定の材料"]').getByRole("button", { name: "差し込む" }).click();
      await page.getByRole("menuitem", { name: "本文（body）", exact: true }).click();

      // 質問: キー・問い・選択肢（名前・AI への説明・キーを分けて書く・D-87）。
      const question = panel.getByRole("group", { name: /^質問 1/ });
      await question.getByRole("textbox", { name: "キー", exact: true }).fill("category");
      await question.getByRole("textbox", { name: "問い（何を判定するか）" }).fill("問い合わせの種類はどれか");
      await question.getByRole("textbox", { name: "選択肢 1 の名前" }).fill("請求");
      await question.getByRole("textbox", { name: "選択肢 1 の AI への説明" }).fill("請求書や返金についての問い合わせ");
      await question.getByRole("textbox", { name: "選択肢 1 のキー" }).fill("billing");
      await question.getByRole("textbox", { name: "選択肢 2 の名前" }).fill("不具合");
      await question.getByRole("textbox", { name: "選択肢 2 のキー" }).fill("bug");
      // 予約名はサーバが知らせる（欄の下）。
      await question.getByRole("textbox", { name: "選択肢 2 のキー" }).fill("error");
      await expect(question.getByText(/選択肢のキーに「error」は使えません/)).toBeVisible({ timeout: 10_000 });
      await question.getByRole("textbox", { name: "選択肢 2 のキー" }).fill("bug");

      // 振り分け: 答えで分ける（使う質問は選択肢の質問）。しきい値は既定を置かず空のまま（D-88・PIT-80）で、
      // 空のままでは実行できないことを欄の下で言う。「0.80 から始める」で仮の値を入れ、範囲の外は欄の下で知らせる。
      await choose(page, panel.getByRole("combobox", { name: "振り分け", exact: true }), "答えで分ける");
      await expect(panel.getByRole("combobox", { name: "振り分けに使う質問" })).toContainText("category");
      const threshold = panel.getByRole("spinbutton", { name: "しきい値（AI の自信）" });
      await expect(threshold).toHaveValue("");
      await expect(panel.getByText("「しきい値（AI の自信）」を入れてください。")).toBeVisible({ timeout: 10_000 });
      await page.screenshot({ path: "test-results/decide-threshold-blank-pc.png" });
      // 空のしきい値は保存を止めない（実行と自動実行のオンの前に止める）。保存した定義にしきい値は無い。
      await page.getByRole("button", { name: "保存", exact: true }).click();
      await expect(page.getByText(/保存しました/).first()).toBeVisible({ timeout: 15_000 });
      const blankSaved = (await savedIr(page, id)).nodes.find((n) => n.id === "decide_1")!;
      expect(blankSaved.params.route).toEqual({ question: "category", by: "value" });
      await panel.getByRole("button", { name: "0.80 から始める" }).click();
      await expect(threshold).toHaveValue("0.8");
      await expect(panel.getByRole("button", { name: "0.80 から始める" })).toHaveCount(0);
      await expect(panel.getByText("AI の自信が 0.80（80%）以上なら答えに従い、届かないときは「要確認」へ進みます。")).toBeVisible();
      await threshold.fill("1.2");
      await expect(panel.getByText("しきい値は 0.50〜1.00 にしてください。")).toBeVisible();
      await threshold.fill("0.8");
      await expect(panel.getByText("しきい値は 0.50〜1.00 にしてください。")).toHaveCount(0);
      // 入れた後は、検証が追いつく前でも、空の知らせを振り分けやしきい値の下に残さない。
      await expect(panel.getByTestId("decide-route").getByText("入力してください")).toHaveCount(0);
      await expect(panel.getByText("「しきい値（AI の自信）」を入れてください。")).toHaveCount(0);
      await expect(panel.getByTestId("decide-calibration")).toContainText("較正なし");

      // 検証の後に、選択肢ごとの出口と要確認のハンドルが出る。出口は選択肢の名前で出す（D-87）。
      await expect.poll(() => handleIds(node), { timeout: 10_000 }).toEqual(["billing", "bug", "uncertain"]);
      await expect(node.locator('[data-port="billing"]')).toHaveText("請求");
      await expect(node.locator('[data-port="bug"]')).toHaveText("不具合");
      await expect(panel.getByTestId("decide-exits").locator('[data-port="billing"]')).toContainText("請求");
      await expect(page.locator('[aria-label^="検証エラー"]')).toHaveCount(0);
      await page.screenshot({ path: "test-results/decide-form-pc.png" });

      await page.getByRole("button", { name: "保存", exact: true }).click();
      await expect(page.getByText(/保存しました/).first()).toBeVisible({ timeout: 15_000 });
      const ir = await savedIr(page, id);
      const saved = ir.nodes.find((n) => n.id === "decide_1")!;
      expect(saved.params).toEqual({
        model: d.id,
        state: { $template: "{body}", vars: { body: { $from: "input", path: "/body" } } },
        questions: [
          {
            key: "category",
            type: "choice",
            instructions: "問い合わせの種類はどれか",
            options: [
              { key: "billing", label: "請求", description: "請求書や返金についての問い合わせ" },
              { key: "bug", label: "不具合" },
            ],
          },
        ],
        route: { question: "category", by: "value", min_confidence: 0.8 },
      });

      // 実行: 材料に選択肢のキーがあれば確からしさ 0.9 でその出口、無ければ 0.4 で要確認（stub の決定器）。
      const sure = await runToEnd(page, id, "billing の件で連絡しました");
      expect(sure.status).toBe("succeeded");
      expect(sure.steps.find((s) => s.node_id === "decide_1")?.taken_ports).toEqual(["billing"]);
      // 答えの名前（label）は選択肢の名前（テーブルの選択肢の列に書き込む値）。答えの値はキーのまま。
      const step = await page.request.get(`/api/workflows/${id}/runs/${sure.run_id}/steps?path=decide_1`);
      expect(step.ok(), await step.text()).toBeTruthy();
      const answer = ((await step.json()) as { output: { answers: Record<string, { value: string; label: string }> } })
        .output.answers.category;
      expect(answer).toMatchObject({ value: "billing", label: "請求" });
      const unsure = await runToEnd(page, id, "よく分からない内容です");
      expect(unsure.status).toBe("succeeded");
      expect(unsure.steps.find((s) => s.node_id === "decide_1")?.taken_ports).toEqual(["uncertain"]);
    } finally {
      await cleanup(page, made);
      if (!d.enabled) await setEnabled(page, [{ id: d.id, enabled: false }]);
    }
  });

  /// スマホ幅: 縦の一覧に判定の出口（選択肢・要確認）の行が出て、設定のシートが横にはみ出さない。
  test("スマホ: 縦の一覧に出口の行が出て、設定のシートで質問と振り分けを編集できる", async ({ browser, page }) => {
    test.setTimeout(180_000);
    const d = decision!;
    const made: Made = { tables: [], artifacts: [] };
    const phone = await phonePage(browser, page);
    try {
      if (!d.enabled) await setEnabled(page, [{ id: d.id, enabled: true }]);
      const name = uniqueName("wf-decide-phone");
      const id = await createWorkflow(page, made, {
        ir_version: 1,
        name,
        display_name: name,
        triggers: [{ kind: "interactive" }],
        input_schema: { type: "object", properties: { body: { type: "string" } }, required: ["body"] },
        nodes: [
          decideNode(d.id, { question: "category", min_confidence: 0.8 }, { $from: "input", path: "/body" }),
          { id: "on_billing", type: "control.join", label: "請求の対応", params: {} },
        ],
        edges: [{ from: "triage", from_port: "billing", to: "on_billing" }],
      });
      await phone.goto(`/workflows/${id}`);
      const flow = phone.getByTestId("workflow-mobile-flow");
      await expect(flow).toBeVisible({ timeout: 15_000 });
      // 判定の後ろのブロックはつながったものとして並ぶ（つながっていない一覧に落ちない）。
      await expect(flow.getByText("要確認", { exact: true })).toBeVisible({ timeout: 10_000 });
      await expect(flow.getByText("billing", { exact: true })).toBeVisible();
      await expect(flow.getByTestId("mobile-flow-cycle")).toHaveCount(0);
      await phone.screenshot({ path: "test-results/decide-mobile-flow-phone.png" });

      await flow.getByRole("button", { name: /問い合わせを判定/ }).filter({ hasText: "問い合わせを判定" }).click();
      const sheet = phone.getByRole("dialog");
      await expect(sheet).toBeVisible();
      await expect(sheet.getByTestId("decide-calibration")).toContainText("較正なし");
      const question = sheet.getByRole("group", { name: /^質問 1/ });
      await question.getByRole("button", { name: "選択肢を追加" }).scrollIntoViewIfNeeded();
      await question.getByRole("button", { name: "選択肢を追加" }).click();
      // 足した選択肢は、最初に決める名前の欄へフォーカスが移る（D-87）。
      await expect(question.getByRole("textbox", { name: "選択肢 3 の名前" })).toBeFocused();
      expect(await sheet.evaluate((el) => el.scrollWidth <= el.clientWidth + 1)).toBe(true);
      expect(await phone.evaluate(() => document.documentElement.scrollWidth <= window.innerWidth + 1)).toBe(true);
      await phone.screenshot({ path: "test-results/decide-sheet-phone.png" });
      await sheet.getByTestId("decide-route").scrollIntoViewIfNeeded();
      await phone.screenshot({ path: "test-results/decide-sheet-route-phone.png" });
    } finally {
      await cleanup(page, made);
      if (!d.enabled) await setEnabled(page, [{ id: d.id, enabled: false }]);
      await phone.context().close();
    }
  });
});

type Run = { run_id: string; status: string; steps: { node_id: string; status: string; taken_ports: string[] }[] };

/// 入力を渡して実行し、run が終わるまで待つ。
async function runToEnd(page: Page, id: string, body: string): Promise<Run> {
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
  return run!;
}

/// 承認の状態の流れつきの表（受付 → 承認 は承認者の列の人）。
async function fsmTable(page: Page, made: Made): Promise<string> {
  const headers = await csrfHeader(page);
  const states = ["受付", "承認", "却下"];
  const created = await page.request.post("/api/data/tables", {
    headers,
    data: {
      name: uniqueName("e2e-判定の確認"),
      schema: {
        fields: [
          { key: "title", display_name: "件名", type: "text", indexed: true },
          { key: "approver", display_name: "承認者", type: "user_ref", indexed: true },
          { key: "status", display_name: "状態", type: "select", options: states, indexed: true },
        ],
        status_field: "status",
      },
    },
  });
  expect(created.status(), await created.text()).toBe(201);
  const table = (await created.json()) as { id: string; schema: Record<string, unknown>; schema_version: number };
  made.tables.push(table.id);
  const approverOnly = { field_cmp: { field: "approver", op: "eq", value: "user_id" } };
  const fsm = await page.request.post("/api/data/fsms", {
    headers,
    data: {
      name: uniqueName("e2e-判定の確認の流れ"),
      table_id: table.id,
      body: {
        states,
        transitions: [
          { from: "受付", to: "承認", actor: approverOnly },
          { from: "受付", to: "却下", actor: approverOnly },
        ],
      },
    },
  });
  expect(fsm.status(), await fsm.text()).toBe(201);
  const f = (await fsm.json()) as { id: string; version?: number };
  made.artifacts.push(f.id);
  const put = await page.request.put(`/api/data/tables/${table.id}/schema`, {
    headers,
    data: {
      schema: { ...table.schema, fsm_ref: { artifact_id: f.id, version: f.version ?? 1 } },
      expected_schema_version: table.schema_version,
    },
  });
  expect(put.status(), await put.text()).toBe(200);
  return table.id;
}

/// ログインは PC の画面で済ませ、そのセッションをタッチ端末の文脈へ持ち込む（workflow-model-default.spec と同じ）。
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

/// レビューの指摘の直し（打ち変えの途中の重なり・出口が決まる前のエラー時の出口・保存の直後の出口・書きかけの質問）。
test.describe("AI で判定（エディタ・編集の途中）", () => {
  let decision: AdminModel | undefined;

  test.beforeEach(async ({ page }) => {
    await loginViaKeycloak(page);
    decision = await stubDecisionModel(page, await adminModels(page));
    test.skip(!decision, "判定用のモデル（stub の決定器）がカタログに無い構成");
  });

  test("キーを 1 文字ずつ打ち変えて、ほかの選択肢・質問のキーを通っても、ほかのエッジと振り分けを移さない", async ({ page }) => {
    test.setTimeout(180_000);
    const d = decision!;
    const made: Made = { tables: [], artifacts: [] };
    try {
      if (!d.enabled) await setEnabled(page, [{ id: d.id, enabled: true }]);
      const name = uniqueName("wf-decide-rename");
      const base = decideNode(d.id, { question: "category", min_confidence: 0.8 }, { $from: "input", path: "/body" });
      const node = {
        ...base,
        params: {
          ...base.params,
          questions: [...base.params.questions, { key: "angry", type: "boolean", instructions: "強い不満を示しているか" }],
        },
      };
      const id = await createWorkflow(page, made, {
        ir_version: 1,
        name,
        display_name: name,
        triggers: [{ kind: "interactive" }],
        input_schema: { type: "object", properties: { body: { type: "string" } }, required: ["body"] },
        nodes: [
          node,
          { id: "on_billing", type: "control.join", label: "請求の対応", params: {} },
          { id: "on_bug", type: "control.join", label: "不具合の対応", params: {} },
          { id: "on_uncertain", type: "control.join", label: "人が確認", params: {} },
          { id: "reply", type: "llm.invoke", label: "返信を書く", params: { prompt: "" } },
        ],
        edges: [
          { from: "triage", from_port: "billing", to: "on_billing" },
          { from: "triage", from_port: "bug", to: "on_bug" },
          { from: "triage", from_port: "uncertain", to: "on_uncertain" },
          { from: "on_billing", to: "reply" },
        ],
      });
      await page.setViewportSize({ width: 1440, height: 1000 });
      await page.goto(`/workflows/${id}`);
      const card = page.getByTestId("rf__node-triage");
      await expect.poll(() => handleIds(card), { timeout: 15_000 }).toEqual(["billing", "bug", "uncertain"]);
      await card.click();
      const panel = page.getByRole("complementary", { name: "ブロックの設定" });

      // 選択肢 2（bug）のキーを billing_issue へ 1 文字ずつ打ち変える。途中で選択肢 1 と同じ billing を通る。
      const question = panel.getByRole("group", { name: /^質問 1/ });
      const key2 = question.getByRole("textbox", { name: "選択肢 2 のキー" });
      await key2.fill("");
      await key2.pressSequentially("billing_issue");
      await expect(key2).toHaveValue("billing_issue");
      await expect.poll(() => handleIds(card), { timeout: 10_000 }).toEqual(["billing", "billing_issue", "uncertain"]);
      await expect(card.getByText("（無い出口）")).toHaveCount(0);

      // 振り分けに使っていない質問（angry）のキーを打ち変えて、振り分けの質問のキー（category）を通っても、
      // 振り分けは category のまま。
      const q2 = panel.getByRole("group", { name: /^質問 2/ });
      const q2key = q2.getByRole("textbox", { name: "キー", exact: true });
      await q2key.fill("");
      await q2key.pressSequentially("category_x");
      await expect(panel.getByRole("combobox", { name: "振り分けに使う質問" })).toContainText("category");
      await expect(panel.getByRole("combobox", { name: "振り分けに使う質問" })).not.toContainText("category_x");

      // 後ろのブロックの「差し込む」に、質問ごとの答えが出る（ai.decide の出力の下位の項目）。
      await page.getByTestId("rf__node-reply").click();
      await panel
        .locator('[data-value-expr="AI への指示"]')
        .getByRole("button", { name: "差し込む" })
        .click();
      for (const label of ["category の答え", "category の確からしさ", "category_x の答え"]) {
        await expect(page.getByRole("menuitem", { name: `「問い合わせを判定」の ${label}`, exact: true })).toBeVisible();
      }
      await page.keyboard.press("Escape");

      await page.getByRole("button", { name: "保存", exact: true }).click();
      await expect(page.getByText(/保存しました/).first()).toBeVisible({ timeout: 15_000 });
      const ir = await savedIr(page, id);
      expect(ir.edges).toEqual(
        expect.arrayContaining([
          { from: "triage", from_port: "billing", to: "on_billing" },
          { from: "triage", from_port: "billing_issue", to: "on_bug" },
          { from: "triage", from_port: "uncertain", to: "on_uncertain" },
        ]),
      );
      const saved = ir.nodes.find((n) => n.id === "triage")!;
      expect((saved.params.route as { question: string }).question).toBe("category");
      expect((saved.params.questions as { key: string }[]).map((q) => q.key)).toEqual(["category", "category_x"]);
    } finally {
      await cleanup(page, made);
      if (!d.enabled) await setEnabled(page, [{ id: d.id, enabled: false }]);
    }
  });

  test("出口が決まる前に失敗したときを続けるにしても、尻尾の＋はエラー時の出口につながない", async ({ page }) => {
    test.setTimeout(180_000);
    const made: Made = { tables: [], artifacts: [] };
    try {
      const name = uniqueName("wf-decide-tail");
      const id = await createWorkflow(page, made, {
        ir_version: 1,
        name,
        display_name: name,
        triggers: [{ kind: "interactive" }],
        nodes: [],
        edges: [],
      });
      // サーバがまだ出口を導いていない間（検証の応答の前・設定が契約に合わない間）を保つため、検証の応答から
      // 判定のノードの出口を外す。
      await page.route("**/api/workflows/validate", async (route) => {
        const res = await route.fetch();
        const body = (await res.json()) as { node_ports?: Record<string, string[]> };
        delete body.node_ports?.decide_1;
        await route.fulfill({ response: res, json: body });
      });
      await page.setViewportSize({ width: 1440, height: 1000 });
      await page.goto(`/workflows/${id}`);
      await page.getByRole("button", { name: "最初のブロック" }).click();
      await page.getByRole("button", { name: /^AI で判定/ }).click();
      const card = page.getByTestId("rf__node-decide_1");
      await expect(card).toBeVisible();
      const panel = page.getByRole("complementary", { name: "ブロックの設定" });
      await choose(page, panel.getByRole("combobox", { name: "失敗したとき" }), "エラー用の出口に流す");
      await page.getByRole("button", { name: "パネルを閉じる" }).click();
      // 出口（続きの処理の出口）が決まるまでは、エラー時の出口があってもその旨を出し、エラー時は名前付きの行で描く。
      await expect(card.getByTestId("node-ports-pending")).toBeVisible({ timeout: 10_000 });
      await expect(card.locator('[data-port="error"]')).toHaveText("エラー時");
      await settleViewport(page);
      await expectHandlesBesideLabels(card);
      await page.screenshot({ path: "test-results/decide-pending-error-pc.png" });

      // 尻尾の＋から足したブロックは、既定の出口（out）につながる（error ではない）。
      await card.getByRole("button", { name: "次のブロックを追加" }).click();
      await page.getByRole("button", { name: /^合流/ }).first().click();
      await expect(page.locator('[data-testid^="rf__edge-e:decide_1|out|"]')).toHaveCount(1);
      await expect(page.locator('[data-testid^="rf__edge-e:decide_1|error|"]')).toHaveCount(0);
      await expect(card.locator('[data-port="out"]')).toHaveText("次へ");

      // スマホ幅の縦の一覧も、出口が決まる前は「次へ」と「エラー時」を分けて出す（error の後ろに足さない）。
      await page.screenshot({ path: "test-results/decide-pending-error-tail-pc.png" });
      // 設定は保存できない（書きかけ）ので、同じ画面の幅を変えて手元の定義のまま見る（設定のパネルは閉じておく）。
      const close = page.getByRole("button", { name: "パネルを閉じる" });
      if (await close.isVisible()) await close.click();
      await page.setViewportSize({ width: 390, height: 844 });
      const flow = page.getByTestId("workflow-mobile-flow");
      await expect(flow).toBeVisible({ timeout: 15_000 });
      await expect(flow.getByText("次へ", { exact: true })).toBeVisible();
      await expect(flow.getByText("エラー時", { exact: true })).toBeVisible();
      await expect(flow.getByRole("button", { name: "「AI で判定」の「エラー時」の後ろにブロックを追加" })).toBeVisible();
      await page.screenshot({ path: "test-results/decide-pending-error-phone.png" });
    } finally {
      // 検証の差し替えが応答を待っている間にテストが終わると、閉じたページへの応答で落ちる（後片付けの前に外す）。
      await page.unrouteAll({ behavior: "ignoreErrors" });
      await cleanup(page, made);
    }
  });

  test("振り分けを変えてすぐ保存しても、書きかけの質問があっても、出口は今の設定のものになる", async ({ page }) => {
    test.setTimeout(180_000);
    const d = decision!;
    const made: Made = { tables: [], artifacts: [] };
    try {
      if (!d.enabled) await setEnabled(page, [{ id: d.id, enabled: true }]);
      const name = uniqueName("wf-decide-fresh");
      const id = await createWorkflow(page, made, {
        ir_version: 1,
        name,
        display_name: name,
        triggers: [{ kind: "interactive" }],
        input_schema: { type: "object", properties: { body: { type: "string" } }, required: ["body"] },
        nodes: [
          decideNode(d.id, undefined, { $from: "input", path: "/body" }),
          { id: "auto", type: "control.join", label: "自動で処理", params: {} },
        ],
        edges: [{ from: "triage", from_port: "out", to: "auto" }],
      });
      await page.setViewportSize({ width: 1440, height: 1000 });
      await page.goto(`/workflows/${id}`);
      const card = page.getByTestId("rf__node-triage");
      await expect.poll(() => handleIds(card), { timeout: 15_000 }).toEqual(["out"]);
      await card.click();
      const panel = page.getByRole("complementary", { name: "ブロックの設定" });

      // 振り分けを変えてすぐ（ライブ検証の前に）保存する。保存の後に出口を取り直す。
      await choose(page, panel.getByRole("combobox", { name: "振り分け", exact: true }), "確かなときだけ進める");
      await page.keyboard.press("Control+s");
      await expect(page.getByText(/保存しました/).first()).toBeVisible({ timeout: 15_000 });
      await expect.poll(() => handleIds(card), { timeout: 10_000 }).toEqual(["out", "uncertain"]);
      await expect(panel.getByTestId("decide-exits")).toHaveAttribute("data-state", "fresh");

      // 質問を足して（問いは空・書きかけ）、答えで分けるに変える。出口は振り分けの質問から決まる。
      await panel.getByRole("button", { name: "質問を追加" }).click();
      await choose(page, panel.getByRole("combobox", { name: "振り分け", exact: true }), "答えで分ける");
      await expect.poll(() => handleIds(card), { timeout: 10_000 }).toEqual(["billing", "bug", "uncertain", "out"]);
      await expect(card.locator('[data-port="out"]')).toHaveText("次へ（無い出口）");
      await expect(panel.getByTestId("decide-exits")).toHaveAttribute("data-state", "fresh");
      await expect(panel.getByTestId("decide-exits")).toContainText("billing");
      await page.screenshot({ path: "test-results/decide-draft-question-pc.png" });
    } finally {
      await cleanup(page, made);
      if (!d.enabled) await setEnabled(page, [{ id: d.id, enabled: false }]);
    }
  });
});
