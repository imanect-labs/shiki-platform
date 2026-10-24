import { expect, test, type Page } from "@playwright/test";

import { loginAs, loginViaKeycloak, uniqueName } from "./helpers";

/// /data の画面の細部（検証で見つかった不具合の回帰・A-2/A-4/A-7/A-8/A-9/D-13）。

const ROW_MARKER = 32;
const COL_WIDTH = 160;
const HEADER = 36;
const ROW = 34;

async function csrf(page: Page): Promise<string> {
  return (await page.context().cookies()).find((c) => c.name === "shiki_csrf")?.value ?? "";
}

async function seedTable(page: Page, name: string, rows: Record<string, unknown>[]) {
  const headers = { "X-CSRF-Token": await csrf(page) };
  const res = await page.request.post("/api/data/tables", {
    headers,
    data: {
      name,
      schema: {
        fields: [
          { key: "name", display_name: "件名", type: "text", indexed: true },
          { key: "amount", display_name: "金額", type: "number" },
          { key: "day", display_name: "日付", type: "date" },
          { key: "tags", display_name: "タグ", type: "multi_select", options: ["出張", "会議", "備品"] },
        ],
      },
    },
  });
  expect(res.status(), await res.text()).toBe(201);
  const tableId = ((await res.json()) as { id: string }).id;
  for (const data of rows) {
    const r = await page.request.post(`/api/data/tables/${tableId}/records`, { headers, data: { data } });
    expect(r.status(), await r.text()).toBe(201);
  }
  return tableId;
}

test("並べ替えできない列の見出しはクエリを送らず理由を出し、複数選択・型の合う値で絞り込める", async ({ page }) => {
  await loginViaKeycloak(page);
  const tableId = await seedTable(page, uniqueName("e2e-絞り込み"), [
    { name: "a", amount: 100, day: "2026-10-01", tags: ["出張"] },
    { name: "b", amount: 200, day: "2026-10-02", tags: ["会議", "備品"] },
  ]);
  const sorted: unknown[] = [];
  const failed: number[] = [];
  page.on("request", (r) => {
    if (r.url().includes(`/data/tables/${tableId}/query`) && r.method() === "POST") {
      const b = r.postDataJSON() as { sort?: unknown };
      if (b.sort) sorted.push(b.sort);
    }
  });
  page.on("response", (r) => {
    if (r.url().includes(`/data/tables/${tableId}/query`) && r.status() >= 400) failed.push(r.status());
  });
  await page.goto(`/data/${tableId}`);
  const grid = page.getByTestId("data-grid");
  await expect(grid).toHaveAttribute("data-loaded", "2");

  // 1. 複数選択（タグ）の見出しをクリック: 並べ替えを送らず、理由を出す（表は消えない・A-7）。
  const box = (await grid.boundingBox())!;
  await page.mouse.click(box.x + ROW_MARKER + 3 * COL_WIDTH + COL_WIDTH / 2, box.y + HEADER / 2);
  await expect(page.getByText("「タグ」では並べ替えできません").first()).toBeVisible();
  await expect(grid).toHaveAttribute("data-loaded", "2");
  expect(sorted).toHaveLength(0);

  // 2. 数値の列に文字を入れると、その場で理由を出して適用できない（A-9）。
  await page.getByTestId("data-filter-button").click();
  await page.getByRole("button", { name: /条件を追加/ }).click();
  await page.getByLabel("条件 1 の列").click();
  await page.getByRole("option", { name: "金額" }).click();
  await page.getByLabel("条件 1 の値").fill("abc");
  await expect(page.getByText("数値を入れてください")).toBeVisible();
  await expect(page.getByTestId("data-filter-apply")).toBeDisabled();

  // 3. 日付の列は日付の入力欄になる。
  await page.getByLabel("条件 1 の列").click();
  await page.getByRole("option", { name: "日付" }).click();
  await expect(page.getByLabel("条件 1 の値")).toHaveAttribute("type", "date");

  // 4. 複数選択の列で絞り込める（「含む」・A-8）。
  await page.getByLabel("条件 1 の列").click();
  await page.getByRole("option", { name: "タグ" }).click();
  await page.getByLabel("条件 1 の値").click();
  await page.getByRole("option", { name: "会議" }).click();
  await page.getByTestId("data-filter-apply").click();
  await expect(page.getByTestId("data-count")).toHaveText("1 件");
  await expect(grid).toHaveAttribute("data-loaded", "1");
  expect(failed).toHaveLength(0);
});

test("行番号で選んだレコードを確かめてから削除し、件数が減る（A-4）", async ({ page }) => {
  await loginViaKeycloak(page);
  const tableId = await seedTable(page, uniqueName("e2e-削除"), [{ name: "残す" }, { name: "消す" }]);
  await page.goto(`/data/${tableId}`);
  const grid = page.getByTestId("data-grid");
  await expect(grid).toHaveAttribute("data-loaded", "2");
  await expect(page.getByTestId("data-count")).toHaveText("2 件");
  // 既定の並び（更新の新しい順）の先頭は最後に作った「消す」。
  const box = (await grid.boundingBox())!;
  await page.mouse.click(box.x + ROW_MARKER / 2, box.y + HEADER + ROW / 2);
  await expect(page.getByTestId("data-grid-selection")).toContainText("1 件を選択中");
  const deleted = page.waitForResponse(
    (r) => r.url().includes(`/data/tables/${tableId}/records/`) && r.request().method() === "DELETE",
  );
  await page.getByTestId("data-grid-delete-rows").click();
  await page.getByRole("button", { name: "削除する" }).click();
  expect((await deleted).status()).toBe(204);
  await expect(grid).toHaveAttribute("data-loaded", "1");
  await expect(page.getByTestId("data-count")).toHaveText("1 件");
  const res = await page.request.post(`/api/data/tables/${tableId}/query`, {
    headers: { "X-CSRF-Token": await csrf(page) },
    data: { limit: 10 },
  });
  const items = ((await res.json()) as { items: { data: { name: string } }[] }).items;
  expect(items.map((i) => i.data.name)).toEqual(["残す"]);
});

test("テーブル: 同じ名前は名前の欄に理由を出し、空のテーブルは追加の導線を出し、一覧から削除できる（A-2・D-13・A-4）", async ({ page }) => {
  await loginViaKeycloak(page);
  const name = uniqueName("e2e-同名");
  const tableId = await seedTable(page, name, []);

  // 空のテーブル: 「条件に合う」ではなく「まだレコードがありません」と追加の導線。
  await page.goto(`/data/${tableId}`);
  await expect(page.getByTestId("data-grid-empty")).toContainText("まだレコードがありません");
  await page.getByRole("button", { name: "最初のレコードを追加" }).click();
  await expect(page.getByTestId("record-dialog")).toBeVisible();
  await page.keyboard.press("Escape");

  // 同じ名前で作る: 競合ではなく「同じ名前のテーブルがあります」。
  await page.goto("/data");
  await page.getByTestId("create-table").click();
  await page.getByLabel("テーブル名").fill(name);
  await page.getByTestId("create-table-submit").click();
  await expect(page.getByText("同じ名前のテーブルがあります。別の名前にしてください。")).toBeVisible();
  await page.keyboard.press("Escape");

  // 一覧から削除する（作成者だけ・確かめてから）。
  await page.getByRole("button", { name: `「${name}」を削除` }).click();
  await page.getByRole("button", { name: "削除する" }).click();
  await expect(page.getByText(`「${name}」を削除しました`).first()).toBeVisible();
  await expect(page.getByTestId("data-table-list").getByText(name, { exact: true })).toHaveCount(0);
  const gone = await page.request.get(`/api/data/tables/${tableId}`);
  expect([403, 404]).toContain(gone.status());
});

test("ユーザーの列の絞り込みは候補から選び、表示名で見せて id で比べる（2-A-P2-3）", async ({ page }) => {
  await loginViaKeycloak(page);
  const headers = { "X-CSRF-Token": await csrf(page) };
  const res = await page.request.post("/api/data/tables", {
    headers,
    data: {
      name: uniqueName("e2e-ユーザー絞り込み"),
      schema: {
        fields: [
          { key: "name", display_name: "件名", type: "text", indexed: true },
          { key: "who", display_name: "申請者", type: "user_ref", indexed: true },
        ],
      },
    },
  });
  expect(res.status(), await res.text()).toBe(201);
  const tableId = ((await res.json()) as { id: string }).id;
  const BOB = "00000000-0000-0000-0000-000000000002";
  const ALICE = "00000000-0000-0000-0000-000000000001";
  for (const data of [{ name: "a", who: BOB }, { name: "b", who: ALICE }, { name: "c", who: BOB }]) {
    const r = await page.request.post(`/api/data/tables/${tableId}/records`, { headers, data: { data } });
    expect(r.status(), await r.text()).toBe(201);
  }
  const filters: unknown[] = [];
  page.on("request", (r) => {
    if (r.url().includes(`/data/tables/${tableId}/query`) && r.method() === "POST") {
      const b = r.postDataJSON() as { filter?: unknown };
      if (b.filter) filters.push(b.filter);
    }
  });
  await page.goto(`/data/${tableId}`);
  await expect(page.getByTestId("data-grid")).toHaveAttribute("data-loaded", "3");
  await page.getByTestId("data-filter-button").click();
  await page.getByRole("button", { name: /条件を追加/ }).click();
  await page.getByLabel("条件 1 の列").click();
  await page.getByRole("option", { name: "申請者" }).click();
  await page.getByRole("combobox", { name: "条件 1 の値" }).fill("bob");
  await page.getByRole("option", { name: /bob@a-corp/ }).click();
  // 選んだ値は表示名で出る（id は見せない）。
  const picked = page.getByTestId("filter-ref-value");
  await expect(picked).toContainText(/bob/i);
  await expect(picked).not.toContainText(BOB);
  await page.getByTestId("data-filter-apply").click();
  await expect(page.getByTestId("data-grid")).toHaveAttribute("data-loaded", "2");
  expect(JSON.stringify(filters.at(-1))).toContain(BOB);
});

test("FSM のテーブルの状態のセルは進める先のメニューで開き、遷移 API で変える（2-C-P2-4）", async ({ page }) => {
  await loginViaKeycloak(page);
  const headers = { "X-CSRF-Token": await csrf(page) };
  const res = await page.request.post("/api/data/tables", {
    headers,
    data: {
      name: uniqueName("e2e-状態"),
      schema: {
        fields: [
          { key: "name", display_name: "件名", type: "text", indexed: true },
          { key: "status", display_name: "状態", type: "select", options: ["下書き", "承認待ち", "承認", "却下"], indexed: true },
        ],
        status_field: "status",
      },
    },
  });
  expect(res.status(), await res.text()).toBe(201);
  const table = (await res.json()) as { id: string; schema: Record<string, unknown>; schema_version: number };
  const fsm = await page.request.post("/api/data/fsms", {
    headers,
    data: {
      name: uniqueName("e2e-flow"),
      table_id: table.id,
      body: {
        states: ["下書き", "承認待ち", "承認", "却下"],
        transitions: [
          { from: "下書き", to: "承認待ち", actor: "public" },
          { from: "承認待ち", to: "承認", actor: "public" },
          { from: "承認待ち", to: "却下", actor: "public" },
        ],
      },
    },
  });
  expect(fsm.status(), await fsm.text()).toBe(201);
  const f = (await fsm.json()) as { id: string; version?: number };
  const put = await page.request.put(`/api/data/tables/${table.id}/schema`, {
    headers,
    data: { schema: { ...table.schema, fsm_ref: { artifact_id: f.id, version: f.version ?? 1 } }, expected_schema_version: table.schema_version },
  });
  expect(put.status(), await put.text()).toBe(200);
  const rec = await page.request.post(`/api/data/tables/${table.id}/records`, { headers, data: { data: { name: "申請", status: "下書き" } } });
  expect(rec.status(), await rec.text()).toBe(201);
  const recordId = ((await rec.json()) as { id: string }).id;

  await page.goto(`/data/${table.id}`);
  const grid = page.getByTestId("data-grid");
  await expect(grid).toHaveAttribute("data-loaded", "1");
  const box = (await grid.boundingBox())!;
  await page.mouse.click(box.x + ROW_MARKER + COL_WIDTH + COL_WIDTH / 2, box.y + HEADER + ROW / 2);
  await page.keyboard.press("Enter");
  const menu = page.getByTestId("transition-editor");
  await expect(menu).toBeVisible();
  // 今の状態（下書き）から出る遷移の行き先だけ。
  await expect(menu.getByRole("option")).toHaveText(["承認待ち"]);
  const transitioned = page.waitForResponse((r) => r.url().includes(`/records/${recordId}/transition`));
  await menu.getByRole("option", { name: "承認待ち" }).click();
  expect((await transitioned).status()).toBe(200);
  await expect
    .poll(async () => ((await (await page.request.get(`/api/data/tables/${table.id}/records/${recordId}`)).json()) as { data: { status: string } }).data.status)
    .toBe("承認待ち");
  // 状態の列は Delete でも PATCH を送らない。
  let patched = false;
  page.on("request", (r) => {
    if (r.url().includes(`/records/${recordId}`) && r.method() === "PATCH") patched = true;
  });
  await page.mouse.click(box.x + ROW_MARKER + COL_WIDTH + COL_WIDTH / 2, box.y + HEADER + ROW / 2);
  // メニューを閉じた直後のクリックはダブルクリックとみなさない（メニューは開かない）。
  await expect(menu).toBeHidden();
  await page.keyboard.press("Delete");
  await expect(page.getByText("「状態」は直接書き換えられません", { exact: true })).toBeVisible();
  expect(patched).toBe(false);
});

/// FSM のテーブル（状態: 下書き → 承認待ち → 承認 / 却下）と 1 行（下書き）を作る。
async function fsmTable(page: Page): Promise<{ tableId: string; recordId: string }> {
  const headers = { "X-CSRF-Token": await csrf(page) };
  const res = await page.request.post("/api/data/tables", {
    headers,
    data: {
      name: uniqueName("e2e-状態の版"),
      schema: {
        fields: [
          { key: "name", display_name: "件名", type: "text", indexed: true },
          { key: "status", display_name: "状態", type: "select", options: ["下書き", "承認待ち", "承認", "却下"], indexed: true },
        ],
        status_field: "status",
      },
    },
  });
  expect(res.status(), await res.text()).toBe(201);
  const table = (await res.json()) as { id: string; schema: Record<string, unknown>; schema_version: number };
  const fsm = await page.request.post("/api/data/fsms", {
    headers,
    data: {
      name: uniqueName("e2e-flow"),
      table_id: table.id,
      body: {
        states: ["下書き", "承認待ち", "承認", "却下"],
        transitions: [
          { from: "下書き", to: "承認待ち", actor: "public" },
          { from: "承認待ち", to: "承認", actor: "public" },
          { from: "承認待ち", to: "却下", actor: "public" },
        ],
      },
    },
  });
  expect(fsm.status(), await fsm.text()).toBe(201);
  const f = (await fsm.json()) as { id: string; version?: number };
  const put = await page.request.put(`/api/data/tables/${table.id}/schema`, {
    headers,
    data: { schema: { ...table.schema, fsm_ref: { artifact_id: f.id, version: f.version ?? 1 } }, expected_schema_version: table.schema_version },
  });
  expect(put.status(), await put.text()).toBe(200);
  const rec = await page.request.post(`/api/data/tables/${table.id}/records`, { headers, data: { data: { name: "申請" } } });
  expect(rec.status(), await rec.text()).toBe(201);
  return { tableId: table.id, recordId: ((await rec.json()) as { id: string }).id };
}

test("状態のメニューはサーバの今の状態と版で進め、進め先を読めなければ理由を出す（AllowedTransitions）", async ({ page }) => {
  await loginViaKeycloak(page);
  const { tableId, recordId } = await fsmTable(page);
  // 変更の購読を止めて、グリッドの行を古いままにする（他の人の更新が届いていない状態）。
  await page.route(new RegExp(`/api/data/tables/${tableId}/stream`), (route) => route.abort());
  await page.goto(`/data/${tableId}`);
  const grid = page.getByTestId("data-grid");
  await expect(grid).toHaveAttribute("data-loaded", "1");
  // 他の人が別の列を更新した（rev 1 → 2）。グリッドはまだ rev 1 の行を持っている。
  const patched = await page.request.patch(`/api/data/tables/${tableId}/records/${recordId}`, {
    headers: { "X-CSRF-Token": await csrf(page) },
    data: { patch: { name: "申請（改）" }, expected_rev: 1 },
  });
  expect(patched.status(), await patched.text()).toBe(200);

  const box = (await grid.boundingBox())!;
  const statusCell = { x: box.x + ROW_MARKER + COL_WIDTH + COL_WIDTH / 2, y: box.y + HEADER + ROW / 2 };
  await page.mouse.click(statusCell.x, statusCell.y);
  await page.keyboard.press("Enter");
  const menu = page.getByTestId("transition-editor");
  await expect(menu).toBeVisible();
  await expect(menu).toContainText("今: 下書き");
  await expect(menu.getByRole("option")).toHaveText(["承認待ち"]);
  // 遷移はメニューを開いたときにサーバが返した版（rev 2）で送る（手元の古い rev 1 で 409 にしない）。
  const sent = page.waitForRequest((r) => r.url().includes(`/records/${recordId}/transition`) && r.method() === "POST");
  const done = page.waitForResponse((r) => r.url().includes(`/records/${recordId}/transition`) && r.request().method() === "POST");
  await menu.getByRole("option", { name: "承認待ち" }).click();
  expect(((await sent).postDataJSON() as { expected_rev: number }).expected_rev).toBe(2);
  expect((await done).status()).toBe(200);
  await expect(page.getByText(/に進められませんでした/)).toHaveCount(0);

  // 進め先を読めない（状態の列が読めない 403 field_unavailable）: 候補を推測で出さず、理由を出す。
  await page.route(new RegExp(`/api/data/tables/${tableId}/records/${recordId}/transitions$`), (route) =>
    route.fulfill({
      status: 403,
      contentType: "application/json",
      body: JSON.stringify({ status: 403, title: "Forbidden", code: "field_unavailable", message: "指定した列は存在しないか、参照できません" }),
    }),
  );
  await expect(menu).toBeHidden();
  await page.mouse.click(statusCell.x, statusCell.y + 1);
  await page.keyboard.press("Enter");
  await expect(menu).toBeVisible();
  await expect(menu.getByRole("alert")).toContainText("状態の列を見る権限がないため");
  await expect(menu.getByRole("option")).toHaveCount(0);
  await page.keyboard.press("Escape");
  await expect(menu).toBeHidden();
});

test("見えないテーブル・削除したテーブル・存在しないテーブルは同じ「見つかりません」になる（404）", async ({ page }) => {
  await loginViaKeycloak(page);
  const headers = { "X-CSRF-Token": await csrf(page) };
  const res = await page.request.post("/api/data/tables", {
    headers,
    data: { name: uniqueName("e2e-消す"), schema: { fields: [{ key: "name", display_name: "件名", type: "text", indexed: true }] } },
  });
  expect(res.status(), await res.text()).toBe(201);
  const gone = ((await res.json()) as { id: string }).id;
  expect((await page.request.delete(`/api/data/tables/${gone}`, { headers })).status()).toBeLessThan(300);
  const NOT_FOUND = "テーブルが見つかりません。削除されたか、見る権限がありません。";
  for (const id of [gone, "8f2b3c1e-0000-4000-8000-000000000000"]) {
    const r = await page.request.get(`/api/data/tables/${id}`);
    expect(r.status()).toBe(404);
    await page.goto(`/data/${id}`);
    await expect(page.getByText(NOT_FOUND)).toBeVisible();
  }
  // 他の人のテーブル（共有されていない）も同じ。
  const page2 = await page.context().browser()!.newPage();
  await loginAs(page2, "bob");
  const mine = await page.request.post("/api/data/tables", {
    headers,
    data: { name: uniqueName("e2e-見せない"), schema: { fields: [{ key: "name", display_name: "件名", type: "text", indexed: true }] } },
  });
  const hidden = ((await mine.json()) as { id: string }).id;
  expect((await page2.request.get(`/api/data/tables/${hidden}`)).status()).toBe(404);
  await page2.goto(`/data/${hidden}`);
  await expect(page2.getByText(NOT_FOUND)).toBeVisible();
  await page2.close();
});
