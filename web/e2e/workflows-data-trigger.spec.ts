import { expect, test, type Page } from "@playwright/test";

import { loginViaKeycloak, uniqueName } from "./helpers";

/// データ変更トリガの e2e（GAP-07・D-47）:
/// - きっかけの設定で「テーブルに行が追加されたとき」とテーブルを選んで保存すると、テーブルに束縛した
///   きっかけと data.read になる。値の欄の「差し込む」に、きっかけの内容の項目（行の ID など）が出る。
/// - 有効化したワークフローが、テーブル画面で追加した行で起動し、run の入力に行が載る。
/// 前提: shiki-server が SHIKI__WORKFLOW__ENABLED=true・LLM は stub。

async function csrfHeaders(page: Page): Promise<Record<string, string>> {
  const csrf = (await page.context().cookies()).find((c) => c.name === "shiki_csrf")?.value ?? "";
  return { "x-csrf-token": csrf, "content-type": "application/json" };
}

/// 件名だけの列を持つテーブルを作る（id と名前）。
async function createTable(page: Page): Promise<{ id: string; name: string; titleKey: string }> {
  const name = uniqueName("e2e-申請");
  const res = await page.request.post("/api/data/tables", {
    headers: await csrfHeaders(page),
    data: { name, schema: { fields: [{ key: "title", display_name: "件名", type: "text" }] } },
  });
  expect(res.status(), await res.text()).toBe(201);
  const table = (await res.json()) as { id: string; schema: { fields: { key: string; display_name: string }[] } };
  const titleKey = table.schema.fields.find((f) => f.display_name === "件名")?.key ?? "title";
  return { id: table.id, name, titleKey };
}

async function createFlow(page: Page, ir: Record<string, unknown>): Promise<string> {
  const res = await page.request.post("/api/workflows", { headers: await csrfHeaders(page), data: { ir } });
  expect(res.ok(), await res.text()).toBeTruthy();
  return ((await res.json()) as { id: string }).id;
}

type SavedIr = {
  declared_scopes?: string[];
  triggers: { kind: string; source?: string; scope?: unknown; filter?: unknown }[];
};

async function savedIr(page: Page, id: string): Promise<SavedIr> {
  return ((await (await page.request.get(`/api/workflows/${id}`)).json()) as { ir: SavedIr }).ir;
}

test("データ変更トリガ: テーブルを選んで保存すると table に束縛したきっかけになり、差し込むにきっかけの項目が出る（GAP-07・BUG-36）", async ({
  page,
}) => {
  test.setTimeout(120_000);
  await loginViaKeycloak(page);
  const table = await createTable(page);
  const name = uniqueName("data-trigger-edit");
  const id = await createFlow(page, {
    ir_version: 1,
    name,
    display_name: name,
    triggers: [{ kind: "interactive" }],
    nodes: [{ id: "ask", type: "llm.invoke", label: "要約する", params: { prompt: "" } }],
    edges: [],
  });
  await page.goto(`/workflows/${id}`);
  const canvas = page.locator(".react-flow");
  await expect(canvas.getByText("要約する").first()).toBeVisible({ timeout: 15_000 });

  // きっかけを「できごと」→「テーブルに行が追加されたとき」にし、テーブルを選ぶ。
  await canvas.getByText("手動で実行").first().click();
  const triggerPanel = page.getByRole("complementary", { name: "きっかけの設定" });
  await triggerPanel.getByRole("combobox").first().click();
  await page.getByRole("option", { name: "できごとが起きたとき" }).click();
  await triggerPanel.getByRole("combobox", { name: "できごと" }).click();
  await page.getByRole("option", { name: "テーブルに行が追加されたとき" }).click();
  await expect(triggerPanel.getByText("有効化した人に見える行・列の変更だけで動きます", { exact: false })).toBeVisible();
  await triggerPanel.getByRole("combobox", { name: "対象のテーブル" }).click();
  await page.getByRole("option", { name: table.name }).click();
  // キャンバスのきっかけにも、できごとの名前が出る。
  await expect(canvas.getByText("テーブルに行が追加されたとき").first()).toBeVisible();

  // 値の欄の「差し込む」に、きっかけの内容（追加された行の ID など）が出る（D-47・BUG-36）。
  await canvas.getByText("要約する").first().click();
  const panel = page.getByRole("complementary", { name: "ブロックの設定" });
  const prompt = panel.locator('[data-value-expr="AI への指示"]');
  await prompt.getByLabel("AI への指示").fill("次の行を確認してください: ");
  await prompt.getByRole("button", { name: "差し込む" }).click();
  await page.getByRole("menuitem", { name: "追加・更新された行の ID（record_id）" }).click();
  await expect(prompt.getByLabel("AI への指示")).toHaveValue("次の行を確認してください: {record_id}");

  await page.getByRole("button", { name: "保存", exact: true }).click();
  await expect(page.getByText(/保存しました/).first()).toBeVisible({ timeout: 15_000 });
  const ir = await savedIr(page, id);
  expect(ir.triggers).toEqual([{ kind: "event", source: "data.record.created", scope: { table: table.id } }]);
  expect(ir.declared_scopes).toContain("data.read");
});

test("データ変更トリガ: 有効化したワークフローが、テーブル画面で追加した行で起動する（GAP-07）", async ({ page }) => {
  test.setTimeout(180_000);
  await loginViaKeycloak(page);
  const table = await createTable(page);
  const name = uniqueName("data-trigger-run");
  const id = await createFlow(page, {
    ir_version: 1,
    name,
    display_name: name,
    triggers: [{ kind: "event", source: "data.record.created", scope: { table: table.id } }],
    nodes: [
      {
        id: "pick",
        type: "script.run",
        label: "行を受け取る",
        params: { source: { inline: "function main(input) { return { id: input.record_id }; }" } },
      },
    ],
    edges: [],
  });
  const enabled = await page.request.post(`/api/workflows/${id}/enable`, {
    headers: await csrfHeaders(page),
    data: {
      version: 1,
      grants: [{ scope: "data.read", object_type: "data_table", object_id: table.id, relation: "viewer" }],
    },
  });
  expect(enabled.ok(), await enabled.text()).toBeTruthy();

  // テーブル画面から行を追加する（人の操作）。
  await page.goto(`/data/${table.id}`);
  await expect(page.getByTestId("data-grid")).toBeVisible();
  await page.getByTestId("add-record").click();
  const dialog = page.getByTestId("record-dialog");
  await dialog.getByLabel("件名").fill("備品の購入");
  await page.getByTestId("record-dialog-submit").click();
  await expect(dialog).toBeHidden();
  const rows = await page.request.post(`/api/data/tables/${table.id}/query`, {
    headers: await csrfHeaders(page),
    data: { limit: 10 },
  });
  const [row] = ((await rows.json()) as { items: { id: string }[] }).items;
  expect(row?.id).toBeTruthy();

  // relay（tick 5 秒）で run が起き、入力に追加した行が載る。
  let runId = "";
  await expect
    .poll(
      async () => {
        const list = (await (await page.request.get(`/api/workflows/${id}/runs`)).json()) as {
          items: { run_id: string; trigger_kind: string }[];
        };
        runId = list.items.find((r) => r.trigger_kind === "event")?.run_id ?? "";
        return runId;
      },
      { timeout: 60_000, intervals: [1_000] },
    )
    .not.toBe("");
  const detail = (await (await page.request.get(`/api/workflows/${id}/runs/${runId}`)).json()) as {
    input: { source: string; record_id: string; by_workflow: boolean; record: { data: Record<string, unknown> } };
  };
  expect(detail.input.source).toBe("data.record.created");
  expect(detail.input.record_id).toBe(row.id);
  expect(detail.input.by_workflow).toBe(false);
  expect(detail.input.record.data[table.titleKey]).toBe("備品の購入");
});
