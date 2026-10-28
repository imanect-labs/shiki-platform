import { expect, test, type Page } from "@playwright/test";

import { cleanupAll, loginViaKeycloak, uniqueName } from "./helpers";

/// 状態の進め方（FSM）を使うブロックのテーブルの e2e（BUG-105）と、「データをまとめて登録」の注記（BUG-104）:
/// - 「状態を進める」と、状態の変化のきっかけのテーブルの一覧は、状態の進め方のあるテーブルだけを出す（承認を
///   待つと同じ）。状態の進め方の無いテーブルは名前で探しても出ない。
/// - 状態の進め方の無いテーブルを API で書いても、保存時の照合（V4）で止まる。
/// - 「データをまとめて登録」のスイッチは、形の合わないレコードだけで止まり、ブロックは失敗にならないことを言う。
/// 前提: shiki-server が SHIKI__WORKFLOW__ENABLED=true・LLM は stub（AI は呼ばない）。

const SHOTS = process.env.E2E_SHOTS_DIR;

async function csrfHeaders(page: Page): Promise<Record<string, string>> {
  const csrf = (await page.context().cookies()).find((c) => c.name === "shiki_csrf")?.value ?? "";
  return { "x-csrf-token": csrf, "content-type": "application/json" };
}

type Made = { tables: string[]; artifacts: string[] };

const STATES = ["申請中", "承認済み"];

/// 状態の列のあるテーブル（`fsm` なら状態の進め方も付ける）。
async function table(page: Page, made: Made, prefix: string, fsm: boolean): Promise<{ id: string; name: string }> {
  const headers = await csrfHeaders(page);
  const name = uniqueName(prefix);
  const created = await page.request.post("/api/data/tables", {
    headers,
    data: {
      name,
      schema: {
        fields: [
          { key: "title", display_name: "件名", type: "text", indexed: true },
          { key: "status", display_name: "状態", type: "select", options: STATES, indexed: true },
        ],
        status_field: "status",
      },
    },
  });
  expect(created.status(), await created.text()).toBe(201);
  const t = (await created.json()) as { id: string; schema: Record<string, unknown>; schema_version: number };
  made.tables.push(t.id);
  if (!fsm) return { id: t.id, name };
  const f = await page.request.post("/api/data/fsms", {
    headers,
    data: {
      name: uniqueName("e2e-進め方"),
      table_id: t.id,
      body: { states: STATES, transitions: [{ from: STATES[0], to: STATES[1], actor: "public" }] },
    },
  });
  expect(f.status(), await f.text()).toBe(201);
  const fsmArt = (await f.json()) as { id: string; version?: number };
  made.artifacts.push(fsmArt.id);
  const put = await page.request.put(`/api/data/tables/${t.id}/schema`, {
    headers,
    data: {
      schema: { ...t.schema, fsm_ref: { artifact_id: fsmArt.id, version: fsmArt.version ?? 1 } },
      expected_schema_version: t.schema_version,
    },
  });
  expect(put.status(), await put.text()).toBe(200);
  return { id: t.id, name };
}

function ir(name: string, tableId: string, triggerTable: string, extra: unknown[] = []) {
  return {
    ir_version: 1,
    name,
    display_name: name,
    triggers: [{ kind: "event", source: "data.transition", scope: { table: triggerTable } }],
    nodes: [
      {
        id: "advance",
        type: "data.transition",
        label: "承認済みにする",
        params: { table: tableId, record: { $from: "trigger", path: "/record_id" }, to: STATES[1] },
      },
      ...extra,
    ],
    edges: [],
  };
}

async function cleanup(page: Page, made: Made) {
  const headers = await csrfHeaders(page);
  await cleanupAll(
    ...[...made.artifacts].reverse().map((a) => async () => {
      const del = await page.request.delete(`/api/artifacts/${a}`, { headers });
      expect(del.status(), await del.text()).toBeLessThan(300);
    }),
    ...[...made.tables].reverse().map((t) => async () => {
      const del = await page.request.delete(`/api/data/tables/${t}`, { headers });
      expect(del.status(), await del.text()).toBeLessThan(300);
    }),
  );
}

test("状態を進める・状態のきっかけ: 状態の進め方のあるテーブルだけを選べ、無いテーブルは保存で止まる（BUG-105）", async ({
  page,
}) => {
  test.setTimeout(180_000);
  await page.setViewportSize({ width: 1440, height: 900 });
  await loginViaKeycloak(page);
  page.on("dialog", (d) => void d.accept());
  const made: Made = { tables: [], artifacts: [] };
  try {
    const withFlow = await table(page, made, "e2e-進め方あり", true);
    const plain = await table(page, made, "e2e-進め方なし", false);
    const headers = await csrfHeaders(page);

    // 状態の進め方の無いテーブルは、ブロックでもきっかけでも、保存時の照合で止まる。
    for (const [node, trigger, path] of [
      [plain.id, withFlow.id, "/params/table"],
      [withFlow.id, plain.id, "/triggers/0/scope/table"],
    ]) {
      const res = await page.request.post("/api/workflows", { headers, data: { ir: ir(uniqueName("transition-ng"), node, trigger) } });
      expect(res.status(), await res.text()).toBe(400);
      const body = (await res.json()) as { errors?: { code: string; path?: string }[] };
      expect(body.errors).toContainEqual(expect.objectContaining({ code: "ir.unknown_table", path }));
    }

    const ok = await page.request.post("/api/workflows", {
      headers,
      data: { ir: ir(uniqueName("transition-ok"), withFlow.id, withFlow.id) },
    });
    expect(ok.ok(), await ok.text()).toBeTruthy();
    const id = ((await ok.json()) as { id: string }).id;
    made.artifacts.push(id);

    await page.goto(`/workflows/${id}`);
    const panel = page.getByRole("complementary", { name: "ブロックの設定" });
    // 「状態を進める」のテーブルの一覧: 進め方のあるテーブルは出て、無いテーブルは名前で探しても出ない。
    await page.getByTestId("rf__node-advance").click();
    const select = panel.getByRole("combobox", { name: "対象のテーブル" });
    await expect(select).toContainText(withFlow.name, { timeout: 15_000 });
    await expect(panel.getByText(/進められるかは、テーブルの状態の進め方と、実行する人/)).toBeVisible();
    await select.click();
    const picker = page.getByTestId("workflow-table-picker");
    await picker.getByTestId("workflow-table-search").fill(withFlow.name);
    await expect(picker.getByRole("option", { name: withFlow.name })).toBeVisible({ timeout: 15_000 });
    // 名前で当たっても、状態の進め方の無いテーブルは「無い」とは言わずに、選べないと言う。
    await picker.getByTestId("workflow-table-search").fill(plain.name);
    await expect(picker.getByText(`「${plain.name}」に当たるテーブルのうち、ここで選べるものはありません`)).toBeVisible({
      timeout: 15_000,
    });
    await expect(picker.getByRole("option", { name: plain.name })).toHaveCount(0);
    if (SHOTS) await page.screenshot({ path: `${SHOTS}/transition-table-pc.png` });
    await page.keyboard.press("Escape");

    // 状態のきっかけのテーブルの一覧も同じ。状態の選択肢は進め方のあるテーブルの状態。
    await page.locator(".react-flow").getByText("テーブルの行の状態が変わったとき").first().click();
    const triggerPanel = page.getByRole("complementary", { name: "きっかけの設定" });
    const trigger = triggerPanel.getByRole("combobox", { name: "対象のテーブル" });
    await expect(trigger).toContainText(withFlow.name, { timeout: 15_000 });
    await trigger.click();
    await picker.getByTestId("workflow-table-search").fill(plain.name);
    await expect(picker.getByText(`「${plain.name}」に当たるテーブルのうち、ここで選べるものはありません`)).toBeVisible({
      timeout: 15_000,
    });
    await page.keyboard.press("Escape");
    await expect(picker).toHaveCount(0);
    await expect(triggerPanel.getByText("「どの状態でも」なら、どの状態に変わっても実行します。")).toBeVisible();
    if (SHOTS) await page.screenshot({ path: `${SHOTS}/transition-trigger-pc.png` });
  } finally {
    await cleanup(page, made);
  }
});

test("データをまとめて登録: スイッチの注記が実際の動き（形の合わないときだけ止まり、ブロックは失敗しない）を言う（BUG-104）", async ({
  page,
}) => {
  test.setTimeout(120_000);
  await page.setViewportSize({ width: 1440, height: 900 });
  await loginViaKeycloak(page);
  const made: Made = { tables: [], artifacts: [] };
  try {
    const t = await table(page, made, "e2e-まとめて登録", false);
    const name = uniqueName("bulk-note");
    const res = await page.request.post("/api/workflows", {
      headers: await csrfHeaders(page),
      data: {
        ir: {
          ir_version: 1,
          name,
          display_name: name,
          triggers: [{ kind: "interactive" }],
          nodes: [
            {
              id: "bulk",
              type: "data.bulk_upsert",
              label: "まとめて登録",
              params: { table: t.id, rows: { $from: "input", path: "/rows" }, abort_on_invalid: true },
            },
          ],
          edges: [],
        },
      },
    });
    expect(res.ok(), await res.text()).toBeTruthy();
    const id = ((await res.json()) as { id: string }).id;
    made.artifacts.push(id);

    await page.goto(`/workflows/${id}`);
    await page.getByTestId("rf__node-bulk").click();
    const panel = page.getByRole("complementary", { name: "ブロックの設定" });
    await expect(panel.getByRole("combobox", { name: "対象のテーブル" })).toContainText(t.name, { timeout: 15_000 });
    const toggle = panel.getByRole("switch", { name: "1 件でも合わなければ、どれも登録しない" });
    await expect(toggle).toHaveAttribute("aria-checked", "true");
    await expect(panel.getByTestId("bulk-abort-note")).toHaveText(
      "オフなら、形の合うレコードだけを登録します。どちらでもブロックは失敗にならないので、結果の件数で確かめてください。",
    );
    await expect(panel.getByText("不合格の行があれば何も登録しない")).toHaveCount(0);
    await panel.getByTestId("bulk-abort-more").getByRole("button", { name: "詳しく" }).click();
    await expect(panel.getByTestId("bulk-abort-more")).toContainText(
      "ほかの書き込みと重なって登録できなかったレコード（同じ値を入れられない列など）があっても、残りは登録します。",
    );
    if (SHOTS) await panel.screenshot({ path: `${SHOTS}/bulk-abort-note-pc.png` });
  } finally {
    await cleanup(page, made);
  }
});
