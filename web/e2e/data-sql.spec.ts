import { expect, test, type Page } from "@playwright/test";

import { loginAs, loginViaKeycloak, uniqueName } from "./helpers";

/// SQL で問い合わせる画面（#978）: 一覧から開いて、参照一覧から名前を入れ、結合・集計を実行する。
/// LIMIT はその件数で終わり、省くと続きを読み込める。拒否は理由と位置を出し、候補で誤った名前を置き換える。
/// 他人のテーブル（見えない）は存在しないテーブルと同じ表示になる。書きかけは再読込しても残る。

async function csrf(page: Page): Promise<string> {
  return (await page.context().cookies()).find((c) => c.name === "shiki_csrf")?.value ?? "";
}

async function post(page: Page, path: string, data: unknown): Promise<{ id: string }> {
  const res = await page.request.post(path, { headers: { "X-CSRF-Token": await csrf(page) }, data });
  expect(res.status(), await res.text()).toBe(201);
  return (await res.json()) as { id: string };
}

test("SQL の画面で結合・集計・続きの読み込みができ、拒否は理由と位置と候補を出す", async ({ page, browser }) => {
  // bob だけが見えるテーブル（alice からは存在しないテーブルと同じに見えるはず）。
  const bobCtx = await browser.newContext();
  const bob = await bobCtx.newPage();
  await loginAs(bob, "bob");
  const secret = uniqueName("e2e_bob専用");
  await post(bob, "/api/data/tables", {
    name: secret,
    schema: { fields: [{ key: "name", display_name: "名前", type: "text", indexed: true }] },
  });
  await bobCtx.close();

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
  for (const [title, custId, amount] of [
    ["刷新", yamada.id, 500],
    ["POS", sakura.id, 300],
    ["保守", yamada.id, 80],
  ] as const) {
    await post(page, `/api/data/tables/${d.id}/records`, { data: { title, customer: [custId], amount } });
  }
  // 続きの読み込み用に 1 ページ（50 件）を超える行を足す。
  for (let i = 0; i < 52; i += 13) {
    await Promise.all(
      Array.from({ length: Math.min(13, 52 - i) }, (_, k) =>
        post(page, `/api/data/tables/${d.id}/records`, { data: { title: `追加${i + k}`, amount: 1 } }),
      ),
    );
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

  // 結合と集計（Ctrl+Enter で実行）。名前に記号（-）を含むので二重引用符で囲む。
  await editor.fill(
    `SELECT c.name, SUM(d.amount) AS total FROM "${deal}" d JOIN "${cust}" c ON d.customer = c._id GROUP BY c.name ORDER BY total DESC`,
  );
  await editor.press("Control+Enter");
  const result = page.getByTestId("sql-result");
  await expect(result.locator("tbody tr")).toHaveCount(2, { timeout: 15_000 });
  await expect(result.locator("tbody tr").first()).toContainText("山田精工");
  await expect(result.locator("tbody tr").first()).toContainText("580");

  // LIMIT を書けばその件数で終わる（続きは出さない）。
  await editor.fill(`SELECT title FROM "${deal}" LIMIT 2`);
  await page.getByTestId("sql-run").click();
  await expect(page.getByTestId("sql-result-count")).toHaveText("2 行");
  await expect(page.getByTestId("sql-more")).toHaveCount(0);

  // LIMIT を省くと 50 件ずつ読み、続きを読み込める。
  await editor.fill(`SELECT title, amount FROM "${deal}"`);
  await page.getByTestId("sql-run").click();
  await expect(page.getByTestId("sql-result-count")).toHaveText("50 行を表示（続きあり）");
  await page.getByTestId("sql-more").click();
  await expect(page.getByTestId("sql-result-count")).toHaveText("55 行");
  await expect(result.locator("tbody tr")).toHaveCount(55);

  // 名前の誤り: 理由・位置・候補。一番近い候補を押すと誤った名前を置き換える。
  await editor.fill(`SELECT titel FROM "${deal}"`);
  await page.getByTestId("sql-run").click();
  const err = page.getByTestId("sql-error");
  await expect(err).toContainText("名前が見つかりません");
  await expect(err.getByTestId("sql-error-location")).toContainText("SELECT titel");
  const first = err.getByTestId("sql-error-expected").getByRole("button").first();
  await expect(first).toHaveText("title");
  await first.click();
  await expect(editor).toHaveValue(`SELECT title FROM "${deal}"`);

  // 部分集合外（DML）: 理由。
  await editor.fill(`DELETE FROM "${deal}"`);
  await page.getByTestId("sql-run").click();
  await expect(err).toContainText("使えない書き方です");

  // 見えないテーブル（bob のもの）と存在しないテーブルは、名前以外が同じ表示。候補（見えるテーブル）も
  // 同じ集合（並びは書いた名前に近い順なので比べない）。
  const shown = async (name: string) => {
    await editor.fill(`SELECT name FROM "${name}"`);
    await page.getByTestId("sql-run").click();
    await expect(err).toContainText(name);
    const message = (await err.locator("p").allInnerTexts()).join("\n").replaceAll(name, "X");
    const candidates = (await err.getByTestId("sql-error-expected").getByRole("button").allInnerTexts()).sort();
    return { message, candidates };
  };
  const hidden = await shown(secret);
  const absent = uniqueName("e2e_存在しない");
  expect(await shown(absent)).toEqual(hidden);
  expect(hidden.candidates).not.toContain(secret);

  // 書きかけは再読込しても残る（ユーザー単位の保存）。
  await page.waitForTimeout(600);
  await page.reload();
  await expect(page.getByTestId("sql-editor")).toHaveValue(`SELECT name FROM "${absent}"`);
});
