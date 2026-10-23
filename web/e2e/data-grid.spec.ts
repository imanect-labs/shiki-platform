import { expect, test, type Page } from "@playwright/test";

import { loginViaKeycloak, uniqueName } from "./helpers";

/// 業務データのグリッド（Task 15.5）: テーブル作成 → 列追加 → レコード編集 → ビュー保存。
///
/// グリッドは canvas（glide-data-grid）なのでセルは座標で触り、値の反映は API（閲覧者本人の権限の
/// `run_query`）で確かめる。

/// 行番号の列・既定の列幅・見出し・行の高さ（data-grid.tsx / data-view.ts の既定値）。
const ROW_MARKER = 32;
const COL_WIDTH = 160;
const HEADER = 36;
const ROW = 34;

async function cellPoint(page: Page, col: number, row: number) {
  const box = await page.getByTestId("data-grid").boundingBox();
  if (!box) throw new Error("grid not visible");
  return {
    x: box.x + ROW_MARKER + col * COL_WIDTH + COL_WIDTH / 2,
    y: box.y + HEADER + row * ROW + ROW / 2,
  };
}

async function queryRows(page: Page, tableId: string) {
  const res = await page.request.post(`/api/data/tables/${tableId}/query`, {
    data: { limit: 50 },
    headers: { "X-CSRF-Token": (await page.context().cookies()).find((c) => c.name === "shiki_csrf")?.value ?? "" },
  });
  expect(res.ok()).toBeTruthy();
  return ((await res.json()) as { items: { id: string; rev: number; data: Record<string, unknown> }[] }).items;
}

test("テーブル作成 → 列追加 → レコード編集 → ビュー保存", async ({ page }) => {
  await loginViaKeycloak(page);
  const tableName = uniqueName("e2e-経費");

  // 1. テーブルを作る（最初の列「名前」は索引付き）。
  await page.goto("/data");
  await page.getByTestId("create-table").click();
  await page.getByLabel("テーブル名").fill(tableName);
  await page.getByTestId("create-table-submit").click();
  await page.waitForURL(/\/data\/[0-9a-f-]{36}$/);
  const tableId = new URL(page.url()).pathname.split("/").pop()!;
  await expect(page.getByTestId("data-grid")).toBeVisible();

  // 2. 列を追加する（数値の「金額」）。
  await page.getByTestId("schema-editor-open").click();
  await page.getByTestId("schema-add-field").click();
  await page.getByLabel("新しい列の表示名").fill("金額");
  await page.getByLabel("新しい列の型").click();
  await page.getByRole("option", { name: "数値" }).click();
  await page.getByTestId("schema-add-confirm").click();
  await page.getByTestId("schema-save").click();
  await expect(page.getByTestId("schema-editor")).toBeHidden();
  // 参照名（key）はサーバが決めた不変の名前（日本語の表示名からは連番になる）。
  const table = (await (await page.request.get(`/api/data/tables/${tableId}`)).json()) as {
    schema: { fields: { key: string; display_name: string }[] };
  };
  const keyOf = (label: string) => table.schema.fields.find((f) => f.display_name === label)!.key;
  const nameKey = keyOf("名前");
  const amountKey = keyOf("金額");

  // 3. レコードを追加する。
  await page.getByTestId("add-record").click();
  const dialog = page.getByTestId("record-dialog");
  await dialog.getByLabel("名前").fill("出張旅費");
  await dialog.getByLabel("金額").fill("12000");
  await page.getByTestId("record-dialog-submit").click();
  await expect(dialog).toBeHidden();
  await expect(page.getByTestId("data-count")).toHaveText("1 件");
  const [created] = await queryRows(page, tableId);
  expect(created.data).toMatchObject({ [amountKey]: 12000 });

  // 4. セルを編集する（rev の楽観ロックで保存される）。
  await expect(page.getByTestId("data-grid")).toHaveAttribute("data-loaded", "1");
  const amountCell = await cellPoint(page, 1, 0);
  // セルを選んで Enter で編集を始め、入力欄に書いて Enter で確定する。
  await page.mouse.click(amountCell.x, amountCell.y);
  await page.keyboard.press("Enter");
  const editor = page.locator("#portal textarea");
  await expect(editor).toBeVisible();
  await editor.fill("9500");
  const patched = page.waitForResponse(
    (r) => r.url().includes(`/data/tables/${tableId}/records/`) && r.request().method() === "PATCH",
  );
  await page.keyboard.press("Enter");
  expect((await patched).status()).toBe(200);
  const [edited] = await queryRows(page, tableId);
  expect(edited.data).toMatchObject({ [nameKey]: "出張旅費", [amountKey]: 9500 });
  expect(edited.rev).toBe(created.rev + 1);

  // 5. 列の見出しを変え、列を隠してビューとして保存する（ビューごとの設定）。
  await page.getByTestId("data-columns-button").click();
  await page.getByLabel("「名前」の見出し").fill("件名");
  await page.getByRole("button", { name: "「金額」を隠す" }).click();
  await page.keyboard.press("Escape");
  await page.getByTestId("view-save-as").click();
  const viewName = uniqueName("自分用");
  await page.getByLabel("ビューの名前").fill(viewName);
  await page.getByTestId("view-save-confirm").click();
  await page.waitForURL(/\?view=[0-9a-f-]{36}$/);
  const viewId = new URL(page.url()).searchParams.get("view")!;

  // 保存した表示設定は型付きの ViewSpec としてサーバに残る。
  const view = await (await page.request.get(`/api/data/views/${viewId}`)).json();
  expect(view.display.kind).toBe("grid");
  expect(view.display.columns).toEqual(
    expect.arrayContaining([
      expect.objectContaining({ field: nameKey, label: "件名" }),
      expect.objectContaining({ field: amountKey, hidden: true }),
    ]),
  );

  // 開き直しても同じ表示（見出し・非表示）になり、既定のビューは変わらない。
  await page.reload();
  await expect(page.getByTestId("view-switcher")).toContainText(viewName);
  await page.getByTestId("data-columns-button").click();
  await expect(page.getByLabel("「名前」の見出し")).toHaveValue("件名");
  await expect(page.getByRole("button", { name: "「金額」を表示" })).toBeVisible();
  await page.keyboard.press("Escape");
  await page.goto(`/data/${tableId}`);
  await expect(page.getByTestId("view-switcher")).toContainText("既定のビュー");
  await page.getByTestId("data-columns-button").click();
  await expect(page.getByLabel("「名前」の見出し")).toHaveValue("名前");
});
