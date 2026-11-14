import { expect, test, type Page } from "@playwright/test";

import { cleanupQueue, deleteTableApi, loginViaKeycloak, uniqueName } from "./helpers";

/// 会話の AI がテーブルを作る・直す道具（data_table_create / data_table_update・D-10 の T05）の承認カード
/// （前提: LLM=stub）。
///
/// - 作る承認カードに、テーブルの名前・列（型・必須・選択肢）・見られる人の決まり・状態の進め方（進められる人）・
///   「持ち主はあなただけ」が出る。承認するとテーブルができ、却下するとできない。
/// - 直す承認カードは今のテーブルを読んで、足す列・変える列を分けて出す。AI が読んだ後に変わっていれば言う。
///
/// チャットはツールの結果を描画しないので、結果はテーブルの本当の状態（API）で確かめる。
/// stub 駆動: `calltool:<ツール名> <入力 JSON>` でツールを決定的に呼ぶ。

const SHOTS = process.env.E2E_SHOTS_DIR;

const cleanup = cleanupQueue();
test.afterEach(() => cleanup.run());

type Table = { id: string; name: string; schema_version: number; schema: { fields: { key: string }[] } };

async function csrfHeaders(page: Page): Promise<Record<string, string>> {
  const csrf = (await page.context().cookies()).find((c) => c.name === "shiki_csrf")?.value ?? "";
  return { "x-csrf-token": csrf, "content-type": "application/json" };
}

async function findTable(page: Page, name: string): Promise<Table | null> {
  const res = await page.request.get(`/api/data/tables?q=${encodeURIComponent(name)}`);
  expect(res.ok(), await res.text()).toBeTruthy();
  return ((await res.json()) as { items: Table[] }).items.find((t) => t.name === name) ?? null;
}

/// 新しい会話で道具を呼ぶ（`/` から送る）。
async function ask(page: Page, tool: string, args: Record<string, unknown>) {
  await page.goto("/");
  const input = page.getByLabel("メッセージを入力");
  await input.fill(`calltool:${tool} ${JSON.stringify(args)}`);
  await page.getByRole("button", { name: "送信" }).click();
  await page.waitForURL(/\/c\/[0-9a-f-]+/i, { timeout: 20_000 });
}

async function waitAnswered(page: Page) {
  await expect(page.getByRole("button", { name: "生成を停止" })).toHaveCount(0, { timeout: 60_000 });
}

function createArgs(name: string) {
  return {
    name,
    fields: [
      { key: "title", display_name: "件名", type: "text", required: true },
      { key: "amount", display_name: "金額", type: "number" },
      { key: "applicant", display_name: "申請者", type: "created_by" },
      { key: "status", display_name: "状態", type: "select" },
    ],
    row_policy: { read: { any: [{ field_cmp: { field: "applicant", op: "eq", value: "user_id" } }, "is_owner"] } },
    fsm: {
      status_field: "status",
      states: ["下書き", "申請中", "承認済み"],
      transitions: [
        { from: "下書き", to: "申請中", actor: { field_cmp: { field: "applicant", op: "eq", value: "user_id" } } },
        { from: "申請中", to: "承認済み", actor: "is_owner" },
      ],
      applicant_write_lock: "applicant",
    },
  };
}

test("作る承認カードに列・決まり・状態の進め方が出て、承認するとでき、却下するとできない", async ({ page }) => {
  test.setTimeout(180_000);
  await page.setViewportSize({ width: 1440, height: 900 });
  await loginViaKeycloak(page);

  // 却下するとテーブルはできない。
  const declined = uniqueName("e2e-却下した経費");
  await ask(page, "data_table_create", createArgs(declined));
  const card = page.getByTestId("approval-card");
  await expect(card.getByTestId("table-create-summary")).toBeVisible({ timeout: 30_000 });
  await card.getByRole("button", { name: "却下" }).click();
  await waitAnswered(page);
  expect(await findTable(page, declined)).toBeNull();

  // 承認するとできる。
  const name = uniqueName("e2e-経費");
  await ask(page, "data_table_create", createArgs(name));
  const summary = card.getByTestId("table-create-summary");
  await expect(summary).toBeVisible({ timeout: 30_000 });
  await expect(summary).toContainText(`テーブル「${name}」を作ります。`);
  await expect(summary).toContainText("列（4）");
  for (const s of ["件名", "金額", "申請者", "状態", "必須", "下書き", "申請中", "承認済み"]) {
    await expect(summary).toContainText(s);
  }
  await expect(summary).toContainText("見られる人の決まり");
  await expect(summary).toContainText("状態の進め方");
  await expect(summary).toContainText("申請者）はレコードを書き換えられません");
  await expect(summary).toContainText("持ち主はあなただけ");
  if (SHOTS) await card.screenshot({ path: `${SHOTS}/ai-table-create-card.png` });
  await card.getByRole("button", { name: "承認して続行" }).click();
  await waitAnswered(page);
  await expect.poll(() => findTable(page, name), { timeout: 30_000 }).not.toBeNull();
  const made = await findTable(page, name);
  if (!made) throw new Error("テーブルができていない");
  cleanup.add(() => deleteTableApi(page, made.id));
  expect(made.schema.fields.map((f) => f.key)).toEqual(["title", "amount", "applicant", "status"]);
});

test("直す承認カードは足す列と変える列を分けて出し、読んだ後に変わっていれば言う", async ({ page }) => {
  test.setTimeout(180_000);
  await page.setViewportSize({ width: 1440, height: 900 });
  await loginViaKeycloak(page);
  const name = uniqueName("e2e-備品");
  const res = await page.request.post("/api/data/tables", {
    headers: await csrfHeaders(page),
    data: {
      name,
      schema: {
        fields: [
          { key: "title", display_name: "件名", type: "text" },
          { key: "kind", display_name: "区分", type: "select", options: ["机"] },
        ],
      },
    },
  });
  expect(res.status(), await res.text()).toBe(201);
  const t = (await res.json()) as Table;
  cleanup.add(() => deleteTableApi(page, t.id));

  const args = {
    table_id: t.id,
    expected_schema_version: t.schema_version,
    fields: [
      { key: "kind", options: ["机", "椅子"] },
      { key: "price", display_name: "価格", type: "number", required: true },
    ],
  };
  // 読んだ版が古いと言う（承認しても保存されない）。
  await ask(page, "data_table_update", { ...args, expected_schema_version: t.schema_version + 5 });
  const card = page.getByTestId("approval-card");
  const stale = card.getByTestId("table-update-summary");
  await expect(stale).toBeVisible({ timeout: 30_000 });
  await expect(stale).toContainText("AI が読んだ後にテーブルが変わっています");
  await card.getByRole("button", { name: "却下" }).click();
  await waitAnswered(page);

  await ask(page, "data_table_update", args);
  const summary = card.getByTestId("table-update-summary");
  await expect(summary).toBeVisible({ timeout: 30_000 });
  await expect(summary).toContainText(`テーブル「${name}」を直します。`);
  await expect(summary).not.toContainText("AI が読んだ後に");
  await expect(summary.getByText("足す", { exact: true })).toHaveCount(1);
  await expect(summary.getByText("変える", { exact: true })).toHaveCount(1);
  await expect(summary).toContainText("価格");
  await expect(summary).toContainText("椅子");
  if (SHOTS) await card.screenshot({ path: `${SHOTS}/ai-table-update-card.png` });
  await card.getByRole("button", { name: "承認して続行" }).click();
  await waitAnswered(page);
  await expect
    .poll(async () => (await findTable(page, name))?.schema.fields.map((f) => f.key), { timeout: 30_000 })
    .toEqual(["title", "kind", "price"]);
});
