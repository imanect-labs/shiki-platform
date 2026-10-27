import { expect, test, type Page } from "@playwright/test";

import { cleanupAll, loginViaKeycloak, uniqueName } from "./helpers";

/// 値で振り分ける（control.switch）の行き先の値の e2e（BUG-100）:
/// 「この値のとき」に打った文字は、数の書き方そのもの（1・0.5）だけを数として保存する。頭に 0 のある番号
/// （001）・1.0・前後に空白のある値は文字のまま保存し、テキストの「001」が「001」の行き先へ進む。
/// 注記は付録 D の N3.1.2（値がないと失敗する・どれにも一致しないと「その他」）。

const SHOTS = process.env.E2E_SHOTS_DIR;

async function csrf(page: Page): Promise<string> {
  return (await page.context().cookies()).find((c) => c.name === "shiki_csrf")?.value ?? "";
}

type SwitchCase = { port: string; equals: unknown };

test("振り分け: 001 は文字のまま保存し、テキストの「001」が 001 の行き先へ進む（BUG-100）", async ({ page }) => {
  test.setTimeout(150_000);
  await page.setViewportSize({ width: 1440, height: 900 });
  await loginViaKeycloak(page);
  const name = uniqueName("switch-case");
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
          input_schema: {
            type: "object",
            properties: { code: { type: "string", title: "コード" } },
          },
          nodes: [
            {
              id: "sw",
              type: "control.switch",
              label: "コードで振り分け",
              params: {
                value: { $from: "input", path: "/code" },
                cases: ["zero_one", "one", "spaced", "one_point_zero"].map((port) => ({ port, equals: "" })),
              },
            },
            {
              id: "t",
              type: "script.run",
              label: "001 の処理",
              params: { source: { inline: "function main(input) { return input; }" } },
            },
          ],
          edges: [{ from: "sw", from_port: "zero_one", to: "t" }],
        },
      },
    });
    expect(res.ok(), await res.text()).toBeTruthy();
    id = ((await res.json()) as { id: string }).id;
    await page.goto(`/workflows/${id}`);
    await page.getByTestId("rf__node-sw").click();
    const panel = page.getByRole("complementary", { name: "ブロックの設定" });
    await expect(panel.getByText("この値と同じ値の行き先へ進みます。値がないと、このブロックは失敗します。")).toBeVisible();
    await expect(
      panel.getByText("上から順に比べて、最初に一致した行き先へ進みます。どれにも一致しないときは「その他」へ進みます。"),
    ).toBeVisible();

    const values = panel.getByRole("textbox", { name: "一致する値" });
    await expect(values).toHaveCount(4);
    for (const [i, text] of ["001", "1", " 1", "1.0"].entries()) {
      await values.nth(i).fill(text);
      // 打った文字はそのまま欄に残る（数に直して書き戻さない）。
      await expect(values.nth(i)).toHaveValue(text);
    }
    if (SHOTS) await page.screenshot({ path: `${SHOTS}/switch-case-pc.png` });
    await page.getByRole("button", { name: "保存", exact: true }).click();
    await expect(page.getByText(/保存しました/).first()).toBeVisible({ timeout: 15_000 });

    const saved = (await (await page.request.get(`/api/workflows/${id}`)).json()) as {
      ir: { nodes: { id: string; params: { cases?: SwitchCase[] } }[] };
    };
    const cases = saved.ir.nodes.find((n) => n.id === "sw")?.params.cases ?? [];
    expect(cases.map((c) => c.equals)).toEqual(["001", 1, " 1", "1.0"]);

    // テキストの「001」で実行すると、001 の行き先へ進む（1 の行き先にも「その他」にも行かない）。
    const run = await page.request.post(`/api/workflows/${id}/runs`, {
      headers: { "x-csrf-token": await csrf(page), "content-type": "application/json" },
      data: { input: { code: "001" } },
    });
    expect(run.ok(), await run.text()).toBeTruthy();
    const runId = ((await run.json()) as { run_id: string }).run_id;
    await expect
      .poll(
        async () => {
          const detail = (await (await page.request.get(`/api/workflows/${id}/runs/${runId}`)).json()) as {
            steps: { node_id: string; status: string; taken_ports: string[] }[];
          };
          const sw = detail.steps.find((s) => s.node_id === "sw");
          return sw?.status === "succeeded" ? sw.taken_ports : null;
        },
        { timeout: 60_000 },
      )
      .toEqual(["zero_one"]);
  } finally {
    await cleanupAll(async () => {
      if (!id) return;
      const del = await page.request.delete(`/api/artifacts/${id}`, { headers: { "x-csrf-token": await csrf(page) } });
      expect(del.status(), await del.text()).toBeLessThan(300);
    });
  }
});
