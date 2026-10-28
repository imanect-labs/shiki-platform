import { expect, test, type Page } from "@playwright/test";

import { loginViaKeycloak, uniqueName } from "./helpers";

/// SQL で問い合わせる画面（#978）: 一覧から開いて、参照一覧から名前を入れ、結合・集計を実行する。
/// 部分集合外の SQL は理由と位置を出し、見えないテーブル名は存在しないテーブルと同じ表示になる。

async function csrf(page: Page): Promise<string> {
  return (await page.context().cookies()).find((c) => c.name === "shiki_csrf")?.value ?? "";
}

async function post(page: Page, path: string, data: unknown): Promise<{ id: string }> {
  const res = await page.request.post(path, { headers: { "X-CSRF-Token": await csrf(page) }, data });
  expect(res.status(), await res.text()).toBe(201);
  return (await res.json()) as { id: string };
}

test("SQL の画面で結合・集計を実行し、拒否は理由と位置を出す", async ({ page }) => {
  await loginViaKeycloak(page); // alice
  const cust = uniqueName("e2e_顧客");
  const deal = uniqueName("e2e_案件");
  const c = await post(page, "/api/data/tables", {
    name: cust,
    schema: { fields: [{ key: "name", display_name: "会社名", type: "text", indexed: true }], aggregate_min_rows: 1 },
  });
  const d = await post(page, "/api/data/tables", {
    name: deal,
    schema: {
      fields: [
        { key: "title", display_name: "件名", type: "text", indexed: true },
        {
          key: "customer",
          display_name: "顧客",
          type: "record_ref",
          ref_table: c.id,
          indexed: true,
          link: { cardinality: "many_to_one", symmetric_key: "deals" },
        },
        { key: "amount", display_name: "金額", type: "number", indexed: true },
      ],
      aggregate_min_rows: 1,
    },
  });
  const yamada = await post(page, `/api/data/tables/${c.id}/records`, { data: { name: "山田精工" } });
  const sakura = await post(page, `/api/data/tables/${c.id}/records`, { data: { name: "さくらマート" } });
  for (const [title, cust_id, amount] of [
    ["刷新", yamada.id, 500],
    ["POS", sakura.id, 300],
    ["保守", yamada.id, 80],
  ] as const) {
    await post(page, `/api/data/tables/${d.id}/records`, { data: { title, customer: [cust_id], amount } });
  }

  // 一覧の「SQL」から開く。
  await page.goto("/data");
  await page.getByTestId("open-sql").click();
  await expect(page).toHaveURL(/\/data\/sql$/);
  const editor = page.getByTestId("sql-editor");
  await expect(editor).toBeVisible();

  // 参照一覧から列名を入れられる（押すとカーソル位置に入る）。
  await editor.fill("SELECT ");
  const ref = page.getByTestId("sql-reference");
  await ref.getByPlaceholder("テーブル・列を探す").fill(deal);
  await ref.getByRole("button", { name: /^title/ }).click();
  await expect(editor).toHaveValue("SELECT title");

  // 結合と集計（Ctrl+Enter で実行）。
  await editor.fill(
    // 名前に記号（-）を含むので二重引用符で囲む。
    `SELECT c.name, SUM(d.amount) AS total FROM "${deal}" d JOIN "${cust}" c ON d.customer = c._id GROUP BY c.name ORDER BY total DESC`,
  );
  await editor.press("Control+Enter");
  const result = page.getByTestId("sql-result");
  await expect(result.locator("tbody tr")).toHaveCount(2, { timeout: 15_000 });
  await expect(result.locator("tbody tr").first()).toContainText("山田精工");
  await expect(result.locator("tbody tr").first()).toContainText("580");
  await expect(page.getByTestId("sql-result-count")).toHaveText(/2 行/);

  // 部分集合外（DML）: 理由と位置。
  await editor.fill(`DELETE FROM "${deal}"`);
  await page.getByTestId("sql-run").click();
  const err = page.getByTestId("sql-error");
  await expect(err).toContainText("使えない書き方です");
  await expect(err.getByTestId("sql-error-location")).toContainText("DELETE");

  // 存在しないテーブル名: 名前が見つからない（見えないテーブルと同じ表示）。
  await editor.fill("SELECT name FROM e2e_存在しないテーブル");
  await page.getByTestId("sql-run").click();
  await expect(err).toContainText("名前が見つかりません");
  await expect(err).toContainText("見つからないか、参照できません");
});
