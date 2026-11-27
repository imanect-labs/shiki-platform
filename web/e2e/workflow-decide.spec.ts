import { expect, test, type Page } from "@playwright/test";

import { loginViaKeycloak, stubDecisionModel, uniqueName } from "./helpers";

/// 判定のノード（ai.decide・FEAT-1）を、サーバの保存・検証・実行の経路で確かめる（エディタの画面は T6）。
///
/// - 検証 API: 各ノードの出口（node_ports）を保存時検証と同じ規則で返す。値で振り分けた判定から out へ張った
///   エッジは ir.unknown_port、文章を生成するモデルを判定に書くと ir.model_modality_mismatch。
/// - 実行: stub の決定器（材料に選択肢のキーがあれば確からしさ 0.9・無ければ先頭を 0.4）で、答えの出口へ進み、
///   しきい値に届かなければ uncertain へ進む。ほかの出口の先は skip される。
/// 前提: 判定用のモデル（stub の決定器・`~/.config/shiki-dev/llm-stub.toml` の decide-stub など）を持つカタログ・
/// LLM は stub・SHIKI__WORKFLOW__ENABLED=true。判定用のモデルを組織で公開して、finally で元に戻す。実 LLM は
/// 呼ばない（元にする chat モデルを持たない stub の決定器だけを使う）。

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

/// 問い合わせの種類で振り分ける判定と、出口ごとの合流ノード（どの出口へ進んだかを step の状態で見る）。
function decideIr(name: string, model: string, route: Record<string, unknown>, ports: string[]) {
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
              instructions: "問い合わせの種類はどれか",
              options: [
                { key: "billing", description: "請求・返金" },
                { key: "bug", description: "不具合" },
              ],
            },
            { key: "priority", type: "score", instructions: "対応の緊急度", levels: ["低", "中", "高"] },
          ],
          route,
        },
      },
      ...ports.map((p) => ({ id: `on_${p}`, type: "control.join", params: {} })),
    ],
    edges: ports.map((p) => ({ from: "triage", from_port: p, to: `on_${p}` })),
  };
}

type Validation = {
  errors: { code: string; node_id?: string; path?: string; message: string }[];
  node_ports: Record<string, string[]>;
};

async function validate(page: Page, ir: unknown): Promise<Validation> {
  const res = await page.request.post("/api/workflows/validate", {
    headers: await csrfHeader(page),
    data: { ir },
  });
  expect(res.ok(), await res.text()).toBeTruthy();
  return (await res.json()) as Validation;
}

type Run = { status: string; steps: { node_id: string; status: string; taken_ports: string[] }[] };

/// 判定の step の出力（カタログの output_fields の形）。
type DecideOutput = {
  answers: Record<string, { type: string; value: unknown; label?: string; confidence: number }>;
  model: string;
  resolved_model: string;
  calibrated: boolean;
  routed: { question: string; port: string } | null;
};

/// 入力を渡して実行し、run が終わるまで待つ。
async function runToEnd(page: Page, id: string, body: string): Promise<{ run: Run; runId: string }> {
  const start = await page.request.post(`/api/workflows/${id}/runs`, {
    headers: await csrfHeader(page),
    data: { input: { body } },
  });
  expect(start.status(), await start.text()).toBe(202);
  const runId = ((await start.json()) as { run_id: string | null }).run_id!;
  expect(runId).toBeTruthy();
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
  return { run: run!, runId };
}

test.describe("AI で判定（ai.decide）", () => {
  let decision: AdminModel | undefined;
  let chatModel: string | undefined;

  test.beforeEach(async ({ page }) => {
    await loginViaKeycloak(page);
    const models = await adminModels(page);
    // 元にする chat モデルを持たない判定用のモデル（stub の決定器）だけを使う（実 LLM を呼ばない）。
    decision = await stubDecisionModel(page, models);
    chatModel = models.find((m) => m.modality === "chat" && m.enabled)?.id;
    test.skip(!decision, "判定用のモデル（stub の決定器）がカタログに無い構成");
  });

  test("検証 API が出口を返し、発火しない出口と種類の違うモデルを拒否する", async ({ page }) => {
    const d = decision!;
    try {
      if (!d.enabled) await setEnabled(page, [{ id: d.id, enabled: true }]);
      const name = uniqueName("wf-decide-check");

      // 値で振り分けると、出口は選択肢のキーと uncertain（out は無い）。
      const byValue = await validate(
        page,
        decideIr(name, d.id, { question: "category", min_confidence: 0.8 }, ["billing", "bug", "uncertain"]),
      );
      expect(byValue.errors).toEqual([]);
      expect(byValue.node_ports.triage).toEqual(["billing", "bug", "uncertain"]);
      expect(byValue.node_ports.on_billing).toEqual(["out"]);

      // 値で振り分けた判定から out へ張ったエッジは発火しないので拒否する。
      const dead = await validate(
        page,
        decideIr(name, d.id, { question: "category", min_confidence: 0.8 }, ["out"]),
      );
      expect(dead.errors.map((e) => [e.code, e.node_id, e.path])).toEqual([
        ["ir.unknown_port", "triage", "/edges/0/from_port"],
      ]);

      // 確からしさで振り分けると out / uncertain（振り分けを変えると出口も変わる）。
      const byConfidence = await validate(
        page,
        decideIr(name, d.id, { question: "category", by: "confidence", min_confidence: 0.8 }, ["out"]),
      );
      expect(byConfidence.errors).toEqual([]);
      expect(byConfidence.node_ports.triage).toEqual(["out", "uncertain"]);

      // 段階の質問では振り分けられない（params の誤り・出口は返さない）。
      const bad = await validate(
        page,
        decideIr(name, d.id, { question: "priority", min_confidence: 0.8 }, []),
      );
      expect(bad.errors.some((e) => e.path === "/params/route/question")).toBeTruthy();
      expect(bad.node_ports.triage).toBeUndefined();

      // 文章を生成するモデルを判定に書くと、種類の違いとしてモデル欄の位置で返る。
      if (chatModel) {
        const wrong = await validate(
          page,
          decideIr(name, chatModel, { question: "category", min_confidence: 0.8 }, ["billing"]),
        );
        const e = wrong.errors.find((x) => x.code === "ir.model_modality_mismatch");
        expect(e, JSON.stringify(wrong.errors)).toBeTruthy();
        expect(e?.path).toBe("/params/model");
        expect(e?.message).toContain(d.id);
      }
    } finally {
      if (!d.enabled) await setEnabled(page, [{ id: d.id, enabled: false }]);
    }
  });

  test("stub の決定器で答えの出口へ進み、確かでなければ uncertain へ進む", async ({ page }) => {
    test.setTimeout(180_000);
    const d = decision!;
    const ids: string[] = [];
    try {
      if (!d.enabled) await setEnabled(page, [{ id: d.id, enabled: true }]);
      const res = await page.request.post("/api/workflows", {
        headers: await csrfHeader(page),
        data: {
          ir: decideIr(
            uniqueName("wf-decide-run"),
            d.id,
            { question: "category", min_confidence: 0.8 },
            ["billing", "bug", "uncertain"],
          ),
        },
      });
      expect(res.status(), await res.text()).toBe(201);
      const id = ((await res.json()) as { id: string }).id;
      ids.push(id);

      const status = (run: Run, node: string) => run.steps.find((s) => s.node_id === node)?.status;

      // 材料に選択肢のキーがある → 確からしさ 0.9 で billing の出口。
      const sure = await runToEnd(page, id, "billing の件で連絡しました");
      expect(sure.run.status).toBe("succeeded");
      expect(sure.run.steps.find((s) => s.node_id === "triage")?.taken_ports).toEqual(["billing"]);
      expect(status(sure.run, "on_billing")).toBe("succeeded");
      expect(status(sure.run, "on_bug")).toBe("skipped");
      expect(status(sure.run, "on_uncertain")).toBe("skipped");
      const step = await page.request.get(`/api/workflows/${id}/runs/${sure.runId}/steps?path=triage`);
      expect(step.ok(), await step.text()).toBeTruthy();
      const output = ((await step.json()) as { output: DecideOutput }).output;
      expect(output.answers.category).toMatchObject({ type: "choice", value: "billing", label: "請求・返金" });
      expect(output.answers.category.confidence).toBeCloseTo(0.9);
      expect(output.answers.priority.type).toBe("score");
      expect(output.routed).toEqual({ question: "category", port: "billing" });
      expect(output.model).toBe(d.id);
      expect(output.calibrated).toBe(false);

      // 手がかりが無い → 先頭を 0.4 で選ぶ → しきい値に届かず uncertain。
      const unsure = await runToEnd(page, id, "よく分からない内容です");
      expect(unsure.run.status).toBe("succeeded");
      expect(status(unsure.run, "on_uncertain")).toBe("succeeded");
      expect(status(unsure.run, "on_billing")).toBe("skipped");
    } finally {
      const headers = await csrfHeader(page);
      for (const id of ids) {
        const del = await page.request.delete(`/api/artifacts/${id}`, { headers });
        expect([204, 404], `DELETE /artifacts/${id} → ${del.status()}`).toContain(del.status());
      }
      if (!d.enabled) await setEnabled(page, [{ id: d.id, enabled: false }]);
    }
  });
});
