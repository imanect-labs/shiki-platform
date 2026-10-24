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

/// セルを 1 回クリックして Enter で編集を始める（クリック直後の Enter で開くこと自体を確かめるので、
/// 選び直し・待ちは入れない。グリッドはクリックの確定でフォーカスを同期的に受け取る）。
async function openCellEditor(page: Page, col: number, row: number) {
  const p = await cellPoint(page, col, row);
  await page.mouse.click(p.x, p.y);
  await page.keyboard.press("Enter");
  const editor = page.locator("#portal textarea");
  await expect(editor).toBeVisible();
  return editor;
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
  // セルを選んで Enter で編集を始め、入力欄に書いて Enter で確定する。
  const editor = await openCellEditor(page, 1, 0);
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

async function csrfOf(page: Page): Promise<string> {
  return (await page.context().cookies()).find((c) => c.name === "shiki_csrf")?.value ?? "";
}

/// API でテーブルと行を作る（列: コード（unique・索引）・金額）。
async function seedTable(page: Page, codes: string[]) {
  const headers = { "X-CSRF-Token": await csrfOf(page) };
  const res = await page.request.post("/api/data/tables", {
    headers,
    data: {
      name: uniqueName("e2e-ライブ"),
      schema: {
        fields: [
          { key: "code", display_name: "コード", type: "text", indexed: true, unique: true },
          { key: "amount", display_name: "金額", type: "number" },
        ],
      },
    },
  });
  expect(res.status(), await res.text()).toBe(201);
  const tableId = ((await res.json()) as { id: string }).id;
  for (const [i, code] of codes.entries()) {
    const r = await page.request.post(`/api/data/tables/${tableId}/records`, {
      headers,
      data: { data: { code, amount: (i + 1) * 100 } },
    });
    expect(r.status(), await r.text()).toBe(201);
  }
  return tableId;
}

test("unique の列に重複する値を入れると、競合ではなく重複の文言を出して元に戻す", async ({ page }) => {
  await loginViaKeycloak(page);
  const tableId = await seedTable(page, ["A-1", "B-2"]);
  await page.goto(`/data/${tableId}`);
  await expect(page.getByTestId("data-grid")).toHaveAttribute("data-loaded", "2");
  // グリッドの 1 行目と同じ並び（既定の並び）で、もう一方の行の値を入れる。
  const rows = await queryRows(page, tableId);
  const other = String(rows[1].data.code);

  const editor = await openCellEditor(page, 0, 0);
  await editor.fill(other);
  const patched = page.waitForResponse(
    (r) => r.url().includes(`/data/tables/${tableId}/records/`) && r.request().method() === "PATCH",
  );
  await page.keyboard.press("Enter");
  const res = await patched;
  expect(res.status()).toBe(409);
  const body = (await res.json()) as { code?: string; field?: string; message?: string };
  expect(body.code).toBe("unique_violation");
  // サーバは違反した列を名指しする（参照名と、表示名つきの文言・値は含めない）。
  expect(body.field).toBe("code");
  expect(body.message).toContain("コード");
  expect(body.message).not.toContain(other);
  await expect(page.getByText("「コード」に同じ値のレコードが既にあります").first()).toBeVisible();
  // 他の人の更新（rev 競合）の文言は出さない。
  await expect(page.getByText("他の人が先に更新しました")).toHaveCount(0);
  const after = await queryRows(page, tableId);
  expect(after[0].data.code).toBe(rows[0].data.code);
});

test("他の人のカーソルを表示し、権限の変更で読み直し、購読が拒否されたら止めて案内する", async ({ page }) => {
  await loginViaKeycloak(page);
  const tableId = await seedTable(page, ["A-1", "B-2"]);
  const rows = await queryRows(page, tableId);

  // 変更の購読（SSE）を差し替えて、サーバが送る形のイベントを決め打ちで流す（EventSource は本物）。
  let mode: "awareness" | "reset" | "forbidden" = "awareness";
  const bob = "00000000-0000-0000-0000-000000000002";
  await page.route(new RegExp(`/api/data/tables/${tableId}/stream`), async (route) => {
    if (mode === "forbidden") return route.fulfill({ status: 403, body: "" });
    const events =
      mode === "reset"
        ? [`event: stream.reset\ndata: ${JSON.stringify({ reason: "authz_changed" })}`]
        : [
            `event: stream.ready\ndata: ${JSON.stringify({ position: 0 })}`,
            `event: awareness\ndata: ${JSON.stringify({ principal: bob, record_id: rows[0].id, field: "amount", at: new Date().toISOString() })}`,
          ];
    await route.fulfill({
      status: 200,
      headers: { "content-type": "text/event-stream", "cache-control": "no-cache" },
      body: `retry: 500\n\n${events.join("\n\n")}\n\n`,
    });
  });

  await page.goto(`/data/${tableId}`);
  await expect(page.getByTestId("data-grid")).toHaveAttribute("data-loaded", "2");

  // 1. 他の人の位置（データを含まない）がツールバーの顔ぶれに出る。
  const peers = page.getByTestId("data-peers");
  await expect(peers).toBeVisible();
  await expect(peers.locator("[aria-label*='「金額」を選択中']")).toHaveCount(1);

  // 2. 自分がセルを選ぶと、位置（行 id と列の参照名だけ）を送る。
  const sent = page.waitForRequest(
    (r) => r.url().includes(`/data/tables/${tableId}/awareness`) && r.method() === "POST",
  );
  const cell = await cellPoint(page, 1, 1);
  await page.mouse.click(cell.x, cell.y);
  const body = (await sent).postDataJSON() as { record_id: string; field: string | null };
  expect(body).toEqual({ record_id: rows[1].id, field: "amount" });

  // 3. 権限が変わった（stream.reset authz_changed）: 直前の行を残さず読み直す。
  const reread = page.waitForRequest(
    (r) => r.url().includes(`/data/tables/${tableId}/query`) && r.method() === "POST",
  );
  mode = "reset";
  await reread;
  // 読み直したあとの購読は拒否される（共有が外れた等）。
  mode = "forbidden";

  // 4. 購読が 403 で繋ぎ直せない: 繋ぎ直しを繰り返さずに止め、案内と「読み直す」を出す
  //    （テーブル自体は読めるので、行は見えたまま）。
  const banner = page.getByTestId("data-grid-live-problem");
  await expect(banner).toBeVisible({ timeout: 15_000 });
  await expect(banner).toContainText("自動反映が止まりました");
  await expect(page.getByTestId("data-grid")).toHaveAttribute("data-loaded", "2");
});

test("編集中に他の人が同じ行を更新したら、編集を始めた時点の rev で送って黙って上書きしない（D-1）", async ({ page, browser }) => {
  await loginViaKeycloak(page);
  const tableId = await seedTable(page, ["A-1"]);
  await page.goto(`/data/${tableId}`);
  await expect(page.getByTestId("data-grid")).toHaveAttribute("data-loaded", "1");

  // 別のブラウザ（別セッション）で同じ行を更新する人。
  const otherCtx = await browser.newContext();
  const other = await otherCtx.newPage();
  await loginViaKeycloak(other);

  // 1. 自分が「金額」の編集を始め、入力したまま確定しない。
  const editor = await openCellEditor(page, 1, 0);
  await editor.fill("777");

  // 2. その間に、もう一人が同じ行の同じ列を更新する（SSE で自分のグリッドにも届く）。
  const [row] = await queryRows(other, tableId);
  const upd = await other.request.patch(`/api/data/tables/${tableId}/records/${row.id}`, {
    headers: { "X-CSRF-Token": await csrfOf(other) },
    data: { patch: { amount: 555 }, expected_rev: row.rev },
  });
  expect(upd.status(), await upd.text()).toBe(200);
  // 編集中の行が他の人に更新されたことを編集中のまま知らせる（SSE の到達の確認も兼ねる）。
  await expect(page.getByTestId("data-grid-edit-stale")).toBeVisible({ timeout: 10_000 });

  // 3. 自分が確定する: 編集を始めた時点の rev で送るので 409 になり、他の人の値は残る。
  const patched = page.waitForResponse(
    (r) => r.url().includes(`/data/tables/${tableId}/records/`) && r.request().method() === "PATCH",
  );
  await page.keyboard.press("Enter");
  const res = await patched;
  expect(res.request().postDataJSON()).toMatchObject({ expected_rev: row.rev });
  expect(res.status()).toBe(409);
  await expect(page.getByText("他の人が先に更新しました").first()).toBeVisible();
  const [after] = await queryRows(other, tableId);
  expect(after.data.amount).toBe(555);
  await otherCtx.close();
});

test("選択中のセルのクリック → Enter → 入力は同じ行に保存し、Delete で空にし、Esc の後も Enter で編集できる（D-10）", async ({ page }) => {
  await loginViaKeycloak(page);
  const tableId = await seedTable(page, ["A-1", "B-2", "C-3"]);
  await page.goto(`/data/${tableId}`);
  await expect(page.getByTestId("data-grid")).toHaveAttribute("data-loaded", "3");
  const rows = await queryRows(page, tableId);
  const patchOf = () =>
    page.waitForResponse(
      (r) => r.url().includes(`/data/tables/${tableId}/records/`) && r.request().method() === "PATCH",
    );

  // 1. 選んでいるセルをもう一度クリックしてから Enter → 入力 → Enter は、その行に保存する。
  const p = await cellPoint(page, 1, 0);
  await page.mouse.click(p.x, p.y);
  // ダブルクリック（＝編集の開始）と区別される間を空けて、選択中のセルをもう一度クリックする。
  await page.waitForTimeout(700);
  await page.mouse.click(p.x, p.y);
  await page.keyboard.press("Enter");
  await expect(page.locator("#portal textarea")).toBeVisible();
  await page.keyboard.type("4242");
  let patched = patchOf();
  await page.keyboard.press("Enter");
  let res = await patched;
  expect(res.url()).toContain(rows[0].id);
  expect(res.request().postDataJSON()).toMatchObject({ patch: { amount: 4242 } });

  // 2. Esc で編集をやめた直後でも Enter で編集を始められる。
  await page.mouse.click(p.x, p.y);
  await page.keyboard.press("Enter");
  await expect(page.locator("#portal textarea")).toBeVisible();
  await page.keyboard.press("Escape");
  await expect(page.locator("#portal textarea")).toBeHidden();
  await page.keyboard.press("Enter");
  await expect(page.locator("#portal textarea")).toBeVisible();
  await page.keyboard.press("Escape");
  await expect(page.locator("#portal textarea")).toBeHidden();

  // 3. Delete で選んだセルを空にする。
  patched = patchOf();
  await page.keyboard.press("Delete");
  res = await patched;
  expect(res.url()).toContain(rows[0].id);
  expect(res.request().postDataJSON()).toMatchObject({ patch: { amount: null } });
  expect(res.status()).toBe(200);

  // 4. 選んだセルで打鍵を始めると、先頭の文字も欠けずに入る（編集欄が開く前の打鍵を取りこぼさない）。
  const codeCell = await cellPoint(page, 0, 1);
  await page.mouse.click(codeCell.x, codeCell.y);
  await page.waitForTimeout(100);
  patched = patchOf();
  await page.keyboard.type("abcdef");
  await page.keyboard.press("Enter");
  res = await patched;
  expect(res.url()).toContain(rows[1].id);
  expect(res.request().postDataJSON()).toMatchObject({ patch: { code: "abcdef" } });

  // 5. Enter で選択が 1 つ下へ移った直後（前のクリックから 500ms 以内）にそのセルをクリックしても、編集は
  //    始まらない（ダブルクリックとみなさない・2-D-P3-3）。本当のダブルクリックでは始まる。
  const top = await cellPoint(page, 0, 0);
  await page.mouse.click(top.x, top.y);
  await page.keyboard.press("Enter");
  await expect(page.locator("#portal textarea")).toBeVisible();
  await page.keyboard.type("Z");
  patched = patchOf();
  await page.keyboard.press("Enter");
  await patched;
  const below = await cellPoint(page, 0, 1);
  await page.mouse.click(below.x, below.y);
  await page.waitForTimeout(200);
  await expect(page.locator("#portal textarea")).toBeHidden();
  await page.waitForTimeout(700);
  await page.mouse.dblclick(below.x, below.y);
  await expect(page.locator("#portal textarea")).toBeVisible();
  await page.keyboard.press("Escape");
  await expect(page.locator("#portal textarea")).toBeHidden();

  // 6. F2 で編集を始め、Backspace で選んだセルを空にする（表計算と同じ）。
  await page.keyboard.press("F2");
  await expect(page.locator("#portal textarea")).toBeVisible();
  await page.keyboard.press("Escape");
  await expect(page.locator("#portal textarea")).toBeHidden();
  patched = patchOf();
  await page.keyboard.press("Backspace");
  res = await patched;
  expect(res.request().postDataJSON()).toMatchObject({ patch: { code: null } });
});
