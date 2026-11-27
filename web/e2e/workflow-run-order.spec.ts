import { expect, test, type Page } from "@playwright/test";

import { cleanupAll, loginViaKeycloak, uniqueName } from "./helpers";

/// 実行の詳細のブロックの並び（BUG-117）: 条件で分けたブロックの後ろで通らなかった側（実行せず）は、分けたブロックと
/// 同じ時刻に書かれる。その中を名前の辞書順にせず、線の順（分けたブロックが先）に並べる。
/// 名前と ID は、辞書順と実行の順が逆になるように付ける。前提: shiki-server が SHIKI__WORKFLOW__ENABLED=true。
/// 作ったワークフローは最後に消す（run はワークフローと一緒に消える）。

async function csrf(page: Page): Promise<Record<string, string>> {
  const token = (await page.context().cookies()).find((c) => c.name === "shiki_csrf")?.value ?? "";
  return { "x-csrf-token": token, "content-type": "application/json" };
}

test("実行の詳細: 通らなかった側のブロックは、条件で分けたブロックの後に並ぶ（BUG-117）", async ({ page }) => {
  test.setTimeout(120_000);
  await loginViaKeycloak(page);
  let id = "";
  try {
    const name = uniqueName("wf-run-order");
    const created = await page.request.post("/api/workflows", {
      headers: await csrf(page),
      data: {
        ir: {
          ir_version: 1,
          name,
          display_name: name,
          triggers: [{ kind: "interactive" }],
          input_schema: {
            type: "object",
            properties: { amount: { type: "number", title: "金額" } },
            required: ["amount"],
          },
          nodes: [
            { id: "a_small", type: "control.join", label: "あ: 少ないとき", params: {} },
            {
              id: "z_check",
              type: "control.branch",
              label: "ん: 金額を調べる",
              params: { condition: { cmp: { left: { $from: "input", path: "/amount" }, op: "gt", right: 100 } } },
            },
            { id: "y_large", type: "control.join", label: "わ: 多いとき", params: {} },
          ],
          edges: [
            { from: "z_check", from_port: "true", to: "y_large" },
            { from: "z_check", from_port: "false", to: "a_small" },
          ],
        },
      },
    });
    expect(created.status(), await created.text()).toBe(201);
    id = ((await created.json()) as { id: string }).id;
    const started = await page.request.post(`/api/workflows/${id}/runs`, {
      headers: await csrf(page),
      data: { input: { amount: 500 } },
    });
    expect(started.status(), await started.text()).toBe(202);
    const runId = ((await started.json()) as { run_id: string }).run_id;
    await expect
      .poll(async () => ((await (await page.request.get(`/api/workflows/${id}/runs/${runId}`)).json()) as { status: string }).status, {
        timeout: 60_000,
      })
      .toBe("succeeded");

    // API の並び: 分けたブロック → 通らなかった側 → 通った側。
    const detail = (await (await page.request.get(`/api/workflows/${id}/runs/${runId}`)).json()) as {
      steps: { step_path: string; status: string }[];
    };
    expect(detail.steps.map((s) => `${s.step_path}:${s.status}`)).toEqual([
      "z_check:succeeded",
      "a_small:skipped",
      "y_large:succeeded",
    ]);

    // 画面の並びも同じ。
    await page.setViewportSize({ width: 1440, height: 900 });
    await page.goto(`/workflows/${id}/runs?run=${runId}`);
    const sheet = page.locator('[role="dialog"]');
    await expect(sheet.getByText("ん: 金額を調べる")).toBeVisible({ timeout: 15_000 });
    const rows = await sheet.locator("li").allTextContents();
    const at = (label: string) => rows.findIndex((t) => t.includes(label));
    expect(at("ん: 金額を調べる")).toBeGreaterThanOrEqual(0);
    expect(at("ん: 金額を調べる")).toBeLessThan(at("あ: 少ないとき"));
    expect(at("あ: 少ないとき")).toBeLessThan(at("わ: 多いとき"));
    await page.screenshot({ path: "test-results/run-order-pc.png" });
  } finally {
    await cleanupAll(async () => {
      if (!id) return;
      const res = await page.request.delete(`/api/artifacts/${id}`, { headers: await csrf(page) });
      expect(res.status(), await res.text()).toBeLessThan(300);
    });
  }
});
