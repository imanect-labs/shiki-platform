import { expect, test, type Page } from "@playwright/test";

import { cleanupAll, loginViaKeycloak, uniqueName } from "./helpers";

/// 「データを調べる」（data.query）の読む件数の上限の e2e（BUG-108）:
/// 201 以上（と 0）は保存時の誤りになり、誤りは「読む件数の上限」の欄の下に出る（探す条件の下に重ねない）。
/// 以前は保存でき、実行で黙って 200 件に切り詰められていた。

const SHOTS = process.env.E2E_SHOTS_DIR;

async function csrf(page: Page): Promise<string> {
  return (await page.context().cookies()).find((c) => c.name === "shiki_csrf")?.value ?? "";
}

test("データを調べる: 読む件数の上限は 1〜200 件で、範囲の外は欄の下に誤りが出て保存できない（BUG-108）", async ({ page }) => {
  test.setTimeout(120_000);
  await page.setViewportSize({ width: 1440, height: 900 });
  await loginViaKeycloak(page);
  const name = uniqueName("data-query-limit");
  let id = "";
  try {
    const res = await page.request.post("/api/workflows", {
      headers: { "x-csrf-token": await csrf(page), "content-type": "application/json" },
      data: {
        ir: {
          ir_version: 1,
          name,
          display_name: name,
          triggers: [{ kind: "interactive" }],
          nodes: [
            {
              id: "find",
              type: "data.query",
              label: "申請を探す",
              params: { table: "8c8a6f6e-2ab7-4a44-a815-9a2b53c4e9a1", query: { limit: 50 } },
            },
          ],
          edges: [],
        },
      },
    });
    expect(res.ok(), await res.text()).toBeTruthy();
    id = ((await res.json()) as { id: string }).id;

    // API から 201 以上を保存しようとしても、直す欄を指した誤りで断る。
    const bad = await page.request.put(`/api/workflows/${id}`, {
      headers: { "x-csrf-token": await csrf(page), "content-type": "application/json" },
      data: {
        expected_version: 1,
        ir: {
          ir_version: 1,
          name,
          display_name: name,
          triggers: [{ kind: "interactive" }],
          nodes: [
            {
              id: "find",
              type: "data.query",
              params: { table: "8c8a6f6e-2ab7-4a44-a815-9a2b53c4e9a1", query: { limit: 500 } },
            },
          ],
          edges: [],
        },
      },
    });
    expect(bad.status()).toBe(400);
    const body = (await bad.json()) as { errors: { path?: string; message: string }[] };
    expect(body.errors).toEqual(
      expect.arrayContaining([
        expect.objectContaining({
          path: "/params/query/limit",
          message: expect.stringContaining("「読む件数の上限」は 1〜200 件にしてください。"),
        }),
      ]),
    );

    await page.goto(`/workflows/${id}`);
    await page.getByTestId("rf__node-find").click();
    const panel = page.getByRole("complementary", { name: "ブロックの設定" });
    const limit = panel.getByRole("spinbutton", { name: /読む件数の上限/ });
    await expect(limit).toHaveValue("50");
    await limit.fill("500");
    // ライブ検証の誤りが欄の下に 1 回だけ出る。
    await expect(panel.getByText("「読む件数の上限」は 1〜200 件にしてください。")).toHaveCount(1, { timeout: 10_000 });
    if (SHOTS) await page.screenshot({ path: `${SHOTS}/data-query-limit-pc.png` });
    await limit.fill("200");
    await expect(panel.getByText("「読む件数の上限」は 1〜200 件にしてください。")).toHaveCount(0, { timeout: 10_000 });
    await page.getByRole("button", { name: "保存", exact: true }).click();
    await expect(page.getByText(/保存しました/).first()).toBeVisible({ timeout: 15_000 });
    const saved = (await (await page.request.get(`/api/workflows/${id}`)).json()) as {
      ir: { nodes: { id: string; params: { query?: { limit?: number } } }[] };
    };
    expect(saved.ir.nodes.find((n) => n.id === "find")?.params.query?.limit).toBe(200);
  } finally {
    await cleanupAll(async () => {
      if (!id) return;
      const del = await page.request.delete(`/api/artifacts/${id}`, { headers: { "x-csrf-token": await csrf(page) } });
      expect(del.status(), await del.text()).toBeLessThan(300);
    });
  }
});
