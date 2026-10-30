import { expect, test, type Browser, type Locator, type Page } from "@playwright/test";

import {
  ANY_SHAPES,
  composeBlock,
  exprKind,
  fitsCompose,
  initialCompose,
  listItems,
  listOfItems,
  objectOfRows,
  objectRowIssue,
  objectRows,
  sameComposed,
  type ObjectRow,
} from "@/components/workflow/editor/config-panel/value-shape";

import { cleanupQueue, deleteTableApi, loginViaKeycloak, removeWorkflowApi, uniqueName } from "./helpers";

/// 値の欄の「項目を並べる」「リストを並べる」（BUG-118）。
///
/// 単体（画面を開かない）: IR の値を行に分けて作り直すと、同じ値に戻る（参照・差し込み・null・入れ子のリストを
/// 含む）。1 行だけ直しても、ほかの項目と並びは変わらない。組み立てられない形（入れ子が深い・読めない値の指定・
/// キーが空）は理由を返す。
///
/// e2e（LLM は使わない）:
/// - データを追加の「レコードの値」を、列を選んで組み立てる。「実行した人」（入力や結果から選ぶ）と直接入力の文字・
///   数を混ぜて保存すると、IR は項目ごとの参照のまま入り、実行するとテーブルのレコードに入る。開き直して 1 列だけ
///   直しても、ほかの列と参照はそのまま残る。スマホ幅のシートでも同じ行で出て、横にはみ出さない。
/// - 結果を画面に返すの「表にするリスト」は、1 件ずつの行（中は項目を並べる）で開き、1 件だけ直して保存しても、
///   ほかの件の参照は残る。
/// - 入れ子が深い値は、いまの JSON の欄のまま出し、理由を言う（「項目を並べる」は選べない）。保存しても値は変わらない。
/// 作ったテーブル・ワークフローは最後に消す。

const SHOTS = process.env.E2E_SHOTS_DIR;

// ── 単体 ──────────────────────────────────────────────────────────

const byPerson = { $from: "run", path: "/started_by" };
const title = { $from: "input", path: "/title" };

test.describe("値の組み立て（単体・BUG-118）", () => {
  test("項目のまとまりを行に分けて作り直すと、参照・差し込み・null・入れ子を含めて同じ値に戻る", () => {
    const value = {
      name: "出張旅費",
      applicant: byPerson,
      amount: 12000,
      memo: null,
      paid: false,
      note: { $template: "{t} の申請", vars: { t: title } },
      tags: ["交通費", { $from: "input", path: "/tag" }],
      meta: { a: { b: { c: 1 } } },
      fallback: { $from: "nodes.ask.output", path: "/text", default: "未入力" },
    };
    expect(composeBlock(value, 0)).toBeNull();
    expect(initialCompose(value, ["object"], 0)).toBe("object");
    const rows = objectRows(value);
    expect(rows.map((r) => r.key)).toEqual(Object.keys(value));
    const rebuilt = objectOfRows(rows);
    expect(rebuilt).toEqual(value);
    // 保存（JSON）を通しても同じ。
    expect(JSON.parse(JSON.stringify(rebuilt))).toEqual(value);
    expect(sameComposed(rebuilt, value)).toBe(true);
  });

  test("1 行だけ直しても、ほかの項目の値と参照と並びは変わらない", () => {
    const value = { name: "出張旅費", applicant: byPerson, amount: 12000 };
    const rows = objectRows(value);
    const edited = rows.map((r) => (r.key === "name" ? { ...r, value: "出張旅費（大阪）" } : r));
    const out = objectOfRows(edited);
    expect(Object.keys(out)).toEqual(["name", "applicant", "amount"]);
    expect(out).toEqual({ name: "出張旅費（大阪）", applicant: byPerson, amount: 12000 });
    // 値を参照に替えても、ほかは変わらない。
    const toRef = objectOfRows(rows.map((r) => (r.key === "amount" ? { ...r, value: { $from: "input", path: "/amount" } } : r)));
    expect(toRef).toEqual({ name: "出張旅費", applicant: byPerson, amount: { $from: "input", path: "/amount" } });
  });

  test("リストを 1 件ずつに分けて作り直すと同じ値に戻る（中の項目のまとまりの参照も残る）", () => {
    const value = [{ name: title, amount: 1 }, { name: "固定", amount: null }, byPerson, "文字", 3];
    expect(composeBlock(value, 0)).toBeNull();
    expect(initialCompose(value, ["array"], 0)).toBe("array");
    expect(listOfItems(listItems(value))).toEqual(value);
    // 中の項目のまとまり（2 段目）も行に分けられる。
    expect(initialCompose(value[0], ANY_SHAPES, 1)).toBe("object");
    expect(objectOfRows(objectRows(value[0]))).toEqual(value[0]);
  });

  test("組み立てられない形は理由を返す（入れ子が深い・読めない値の指定・キーが空）", () => {
    // 3 段目の入れ子の中の参照は組み立てない（2 段まで）。参照の無い深い入れ子は 1 つの値として直接入力できる。
    expect(composeBlock({ a: { b: { c: title } } }, 0)).toBe("deep");
    expect(composeBlock([{ items: [title] }], 0)).toBe("deep");
    expect(composeBlock({ a: { b: { c: 1 } } }, 0)).toBeNull();
    expect(composeBlock({ a: { b: title } }, 0)).toBeNull();
    // エンジンが値式として読めないもの（知らないキー・$from が文字でない・$template が文字でない）。
    expect(composeBlock({ a: { $from: "input", extra: 1 } }, 0)).toBe("malformed");
    expect(composeBlock({ a: { $from: 1 } }, 0)).toBe("malformed");
    expect(composeBlock({ a: { b: { $template: 3 } } }, 0)).toBe("malformed");
    expect(composeBlock({ a: { $template: "x", vars: [] } }, 0)).toBe("malformed");
    expect(composeBlock({ "": 1, a: 2 }, 0)).toBe("blank_key");
    for (const v of [{ a: { b: { c: title } } }, { a: { $from: 1 } }, { "": 1 }]) {
      expect(initialCompose(v, ["object"], 0)).toBeNull();
      expect(fitsCompose(v, "object", 0)).toBe(false);
    }
    // 値式の形はエンジンの読み込みと同じ（path の null は省略と同じ）。
    expect(exprKind({ $from: "input", path: null })).toBe("from");
    expect(exprKind({ $template: "a" })).toBe("template");
  });

  test("欄を開いたときの形: 受けない形・2 段目より深い段・空の欄", () => {
    expect(initialCompose({ a: title }, ["array"], 0)).toBeNull();
    expect(initialCompose({ a: title }, ["object"], 2)).toBeNull();
    // 空の欄は、項目のまとまりに限る欄だけ「項目を並べる」で開く。空のリスト（CSV の編集の既定）は JSON のまま。
    expect(initialCompose(undefined, ["object"], 0, "object")).toBe("object");
    expect(initialCompose({}, ["object"], 0, "object")).toBe("object");
    expect(initialCompose(undefined, ["object"], 0)).toBeNull();
    expect(initialCompose([], ["array"], 0)).toBeNull();
    expect(initialCompose(title, ["object"], 0, "object")).toBeNull();
    expect(sameComposed({}, undefined)).toBe(true);
    expect(sameComposed({ a: 1, b: 2 }, { b: 2, a: 1 })).toBe(true);
    expect(sameComposed({ a: 1 }, { a: 2 })).toBe(false);
  });

  test("キーが空・同じキー・値の無い行は IR に入れず、理由を返す", () => {
    const rows: ObjectRow[] = [
      { id: 1, key: "name", value: "a" },
      { id: 2, key: "", value: "b" },
      { id: 3, key: "name", value: "c" },
      { id: 4, key: "amount", value: undefined },
      { id: 5, key: "memo", value: null },
    ];
    expect(objectOfRows(rows)).toEqual({ name: "a", memo: null });
    expect(rows.map((_, i) => objectRowIssue(rows, i))).toEqual([null, "blank_key", "duplicate", "no_value", null]);
    expect(listOfItems([{ id: 1, value: "a" }, { id: 2, value: undefined }, { id: 3, value: null }])).toEqual(["a", null]);
  });
});

// ── e2e ───────────────────────────────────────────────────────────

const cleanup = cleanupQueue();
test.afterEach(() => cleanup.run());

async function csrfHeaders(page: Page): Promise<Record<string, string>> {
  const csrf = (await page.context().cookies()).find((c) => c.name === "shiki_csrf")?.value ?? "";
  return { "x-csrf-token": csrf, "content-type": "application/json" };
}

type IrNode = { id: string; type: string; params: Record<string, unknown> };

async function savedParams(page: Page, id: string, node: string): Promise<Record<string, unknown>> {
  const res = await page.request.get(`/api/workflows/${id}`);
  expect(res.ok(), await res.text()).toBeTruthy();
  const ir = ((await res.json()) as { ir: { nodes: IrNode[] } }).ir;
  return ir.nodes.find((n) => n.id === node)?.params ?? {};
}

async function createWorkflow(page: Page, nodes: unknown[], edges: unknown[] = []): Promise<string> {
  const name = uniqueName("wf-value-compose");
  const res = await page.request.post("/api/workflows", {
    headers: await csrfHeaders(page),
    data: { ir: { ir_version: 1, name, display_name: name, triggers: [{ kind: "interactive" }], nodes, edges } },
  });
  expect(res.ok(), await res.text()).toBeTruthy();
  const id = ((await res.json()) as { id: string }).id;
  cleanup.add(() => removeWorkflowApi(page, id));
  return id;
}

/// 保存のボタンを押し、保存できたことを待つ。
async function save(page: Page) {
  const done = page.waitForResponse((r) => r.request().method() === "PUT" && /\/api\/workflows\/[^/]+$/.test(r.url()));
  await page.getByRole("button", { name: "保存", exact: true }).click();
  const res = await done;
  expect(res.ok(), await res.text()).toBeTruthy();
}

/// 欄の中の「値の入れ方」で `mode` を選ぶ（入れ子の行の中では、その行のものだけを見る）。
async function chooseMode(page: Page, scope: Locator, mode: string) {
  await scope.getByRole("combobox", { name: "値の入れ方" }).first().click();
  await page.getByRole("option", { name: mode, exact: true }).click();
}

/// 「列を追加」で行を足し、列を選ぶ。選んだ列の行（読み上げの名前が列の名前になる）を返す。
async function addColumn(page: Page, field: Locator, column: string): Promise<Locator> {
  await field.getByRole("button", { name: "列を追加" }).click();
  const row = field.getByRole("group").last();
  await expect(row.getByRole("combobox", { name: "列" })).toBeFocused();
  await row.getByRole("combobox", { name: "列" }).click();
  await page.getByRole("option", { name: column, exact: true }).click();
  const named = field.getByRole("group", { name: column, exact: true });
  await expect(named).toBeVisible();
  return named;
}

async function phonePage(browser: Browser, page: Page): Promise<Page> {
  const ctx = await browser.newContext({
    storageState: await page.context().storageState(),
    viewport: { width: 390, height: 844 },
    hasTouch: true,
    isMobile: true,
    deviceScaleFactor: 2,
    ignoreHTTPSErrors: true,
  });
  cleanup.add(() => ctx.close());
  return ctx.newPage();
}

test("データを追加: 列ごとに「実行した人」と直接入力を混ぜて組み、保存して実行するとレコードに入り、開き直して 1 列直しても参照が残る（BUG-118）", async ({
  page,
  browser,
}) => {
  test.setTimeout(180_000);
  await page.setViewportSize({ width: 1440, height: 900 });
  await loginViaKeycloak(page);
  const me = (await (await page.request.get("/api/me")).json()) as { id: string };

  const tableName = uniqueName("e2e-経費の申請");
  const t = await page.request.post("/api/data/tables", {
    headers: await csrfHeaders(page),
    data: {
      name: tableName,
      schema: {
        fields: [
          { key: "name", display_name: "件名", type: "text" },
          { key: "applicant", display_name: "申請者", type: "user_ref" },
          { key: "amount", display_name: "金額", type: "number" },
          { key: "memo", display_name: "メモ", type: "text" },
        ],
      },
    },
  });
  expect(t.status(), await t.text()).toBe(201);
  const tableId = ((await t.json()) as { id: string }).id;
  cleanup.add(() => deleteTableApi(page, tableId));
  const id = await createWorkflow(page, [
    { id: "add", type: "data.record.create", label: "申請を記録", params: { table: tableId, data: {} } },
  ]);

  await page.goto(`/workflows/${id}`);
  const panel = page.getByRole("complementary", { name: "ブロックの設定" });
  await page.locator(".react-flow").getByText("申請を記録").first().click();
  // テーブルを読んでから列の候補が出る（手元の開発用の DB はテーブルが多く、読むのに時間がかかることがある）。
  await expect(panel.getByRole("combobox", { name: "対象のテーブル" })).toContainText(tableName, { timeout: 30_000 });
  const field = panel.locator('[data-value-expr="レコードの値"]');
  // 空の項目のまとまりは「項目を並べる」で開き、テーブルの列から選ばせる。
  await expect(field.getByRole("combobox", { name: "値の入れ方" }).first()).toHaveText("項目を並べる");
  await expect(field).toContainText("値を入れる列を「列を追加」で選びます。");

  // 件名: 直接入力。
  const name = await addColumn(page, field, "件名（name）");
  await name.getByRole("textbox", { name: "「件名（name）」の値" }).fill("出張旅費");
  // 申請者: 入力や結果から選ぶ → 実行した人。
  const applicant = await addColumn(page, field, "申請者（applicant）");
  // 値を入れるまでは保存しないことを、その行で言う。
  await expect(applicant.getByTestId("compose-row-issue")).toHaveText("値を入れるまで、この列は保存しません。");
  await chooseMode(page, applicant, "入力や結果から選ぶ");
  await applicant.getByRole("combobox", { name: "参照先" }).click();
  await page.getByRole("option", { name: "実行した人（started_by）", exact: true }).click();
  await expect(applicant.getByTestId("compose-row-issue")).toHaveCount(0);
  // 金額: 数値の列は、打った数字を数として入れる。
  const amount = await addColumn(page, field, "金額（amount）");
  await amount.getByRole("textbox", { name: "「金額（amount）」の値" }).fill("12,000");
  // ほかの行で選んだ列は、もう選べない。
  await field.getByRole("button", { name: "列を追加" }).click();
  await field.getByRole("group").last().getByRole("combobox", { name: "列" }).click();
  await expect(page.getByRole("option", { name: "件名（name）", exact: true })).toHaveAttribute("aria-disabled", "true");
  await page.keyboard.press("Escape");
  await field.getByRole("group").last().getByRole("button", { name: /を外す$/ }).click();
  await expect(field.getByRole("group")).toHaveCount(3);
  await field.scrollIntoViewIfNeeded();
  if (SHOTS) await page.screenshot({ path: `${SHOTS}/value-compose-record-pc.png` });

  await save(page);
  expect((await savedParams(page, id, "add")).data).toEqual({ name: "出張旅費", applicant: byPerson, amount: 12000 });

  // 開き直しても同じ行で出る。1 列だけ直して保存しても、ほかの列と参照はそのまま残る。
  await page.reload();
  await page.locator(".react-flow").getByText("申請を記録").first().click();
  await expect(panel.getByRole("combobox", { name: "対象のテーブル" })).toContainText(tableName, { timeout: 30_000 });
  await expect(field.getByRole("combobox", { name: "値の入れ方" }).first()).toHaveText("項目を並べる");
  const applicant2 = field.getByRole("group", { name: "申請者（applicant）", exact: true });
  await expect(applicant2.getByRole("combobox", { name: "値の入れ方" })).toHaveText("入力や結果から選ぶ");
  await expect(applicant2.getByRole("combobox", { name: "参照先" })).toHaveText("実行した人（started_by）");
  await expect(field.getByRole("textbox", { name: "「金額（amount）」の値" })).toHaveValue("12000");
  await field.getByRole("textbox", { name: "「件名（name）」の値" }).fill("出張旅費（大阪）");
  // 「直接入力」に切り替えると、組み立てた値を JSON で見せる（IR は変えない）。戻すと同じ行に戻る。
  // 行はテーブルの列の並びで出す（保存した値のキーの並びによらない）。
  await expect(field.getByRole("combobox", { name: "列" })).toHaveText(["件名（name）", "申請者（applicant）", "金額（amount）"]);
  await chooseMode(page, field, "直接入力");
  const json = await field.getByRole("textbox", { name: "レコードの値" }).inputValue();
  expect(JSON.parse(json)).toEqual({ name: "出張旅費（大阪）", applicant: byPerson, amount: 12000 });
  await chooseMode(page, field, "項目を並べる");
  await expect(field.getByRole("group")).toHaveCount(3);
  await save(page);
  expect((await savedParams(page, id, "add")).data).toEqual({ name: "出張旅費（大阪）", applicant: byPerson, amount: 12000 });

  // 実行すると、実行した人（alice）の ID がレコードの申請者に入る。
  const started = await page.request.post(`/api/workflows/${id}/runs`, { headers: await csrfHeaders(page), data: { input: {} } });
  expect(started.status(), await started.text()).toBe(202);
  const runId = ((await started.json()) as { run_id: string }).run_id;
  await expect
    .poll(
      async () => ((await (await page.request.get(`/api/workflows/${id}/runs/${runId}`)).json()) as { status: string }).status,
      { timeout: 60_000 },
    )
    .toBe("succeeded");
  const records = await page.request.get(`/api/data/tables/${tableId}/records`);
  expect(records.ok(), await records.text()).toBeTruthy();
  const rows = ((await records.json()) as { items: { data: Record<string, unknown> }[] }).items;
  expect(rows).toHaveLength(1);
  expect(rows[0].data).toMatchObject({ name: "出張旅費（大阪）", applicant: me.id, amount: 12000 });

  // スマホ幅のシートでも同じ行で出て、横にはみ出さない。
  const phone = await phonePage(browser, page);
  await phone.goto(`/workflows/${id}`);
  const flow = phone.getByTestId("workflow-mobile-flow");
  await expect(flow).toBeVisible({ timeout: 15_000 });
  await flow.getByRole("button", { name: /申請を記録/ }).filter({ hasText: "申請を記録" }).first().click();
  const sheet = phone.getByRole("dialog");
  await expect(sheet.getByRole("combobox", { name: "対象のテーブル" })).toContainText(tableName, { timeout: 30_000 });
  const phoneField = sheet.locator('[data-value-expr="レコードの値"]');
  await expect(phoneField.getByRole("group")).toHaveCount(3);
  await expect(phoneField.getByRole("group", { name: "申請者（applicant）", exact: true }).getByRole("combobox", { name: "参照先" })).toHaveText(
    "実行した人（started_by）",
  );
  // 数値の列の数は、テーブルを読む前に開いても「JSON で入れる」をオフで見せる（はじめから数の列のときと同じ）。
  await expect(phoneField.getByRole("group", { name: "金額（amount）", exact: true }).getByRole("switch", { name: "JSON で入れる" })).not.toBeChecked();
  await phoneField.scrollIntoViewIfNeeded();
  expect(await sheet.evaluate((el) => el.scrollWidth <= el.clientWidth + 1)).toBe(true);
  if (SHOTS) await phone.screenshot({ path: `${SHOTS}/value-compose-record-phone.png` });
});

test("結果を画面に返す: 表にするリストを 1 件ずつの行で開き、1 件だけ直しても、ほかの件の参照は残る（BUG-118）", async ({
  page,
  browser,
}) => {
  test.setTimeout(120_000);
  await page.setViewportSize({ width: 1440, height: 900 });
  await loginViaKeycloak(page);
  const value = [
    { item: title, amount: 1200 },
    { item: "宿泊費", amount: 9800 },
  ];
  const id = await createWorkflow(page, [
    { id: "reply", type: "workflow.reply", label: "明細を返す", params: { format: "table", value } },
  ]);
  await page.goto(`/workflows/${id}`);
  const panel = page.getByRole("complementary", { name: "ブロックの設定" });
  await page.locator(".react-flow").getByText("明細を返す").first().click();
  const field = panel.locator('[data-value-expr="表にするリスト"]');
  await expect(field.getByRole("combobox", { name: "値の入れ方" }).first()).toHaveText("リストを並べる");
  const first = field.getByRole("group", { name: "1 件目", exact: true });
  const second = field.getByRole("group", { name: "2 件目", exact: true });
  await expect(first.getByRole("combobox", { name: "値の入れ方" }).first()).toHaveText("項目を並べる");
  // 入力項目の無いワークフローの参照は、場所を手で入れた形で出す（どこの値かと場所がそのまま見える）。
  const item = first.getByRole("group", { name: "item", exact: true });
  await expect(item.getByRole("combobox", { name: "値の入れ方" })).toHaveText("入力や結果から選ぶ");
  await expect(item.getByRole("textbox", { name: "取り出す場所" })).toHaveValue("/title");
  // 2 段目の中の値は、組み立てを選べない（入れ子は 2 段まで）。
  await second.getByRole("group", { name: "amount", exact: true }).getByRole("combobox", { name: "値の入れ方" }).click();
  await expect(page.getByRole("option", { name: "項目を並べる" })).toHaveCount(0);
  await page.keyboard.press("Escape");
  await second.getByRole("textbox", { name: "「amount」の値" }).fill("10500");
  // 1 件足して、キーと値を入れる。
  await field.getByRole("button", { name: "1 件追加" }).click();
  const third = field.getByRole("group", { name: "3 件目", exact: true });
  await chooseMode(page, third, "項目を並べる");
  await third.getByRole("button", { name: "項目を追加" }).click();
  await expect(third.getByRole("textbox", { name: "キー" })).toBeFocused();
  await third.getByRole("textbox", { name: "キー" }).fill("item");
  await third.getByRole("textbox", { name: "「item」の値" }).fill("日当");
  await field.scrollIntoViewIfNeeded();
  if (SHOTS) await page.screenshot({ path: `${SHOTS}/value-compose-list-pc.png` });
  await save(page);
  // 数として書いてあった値は、JSON で入れたまま数で残る（直接入力の JSON のスイッチがオン）。
  expect((await savedParams(page, id, "reply")).value).toEqual([
    { item: title, amount: 1200 },
    { item: "宿泊費", amount: 10500 },
    { item: "日当" },
  ]);

  const phone = await phonePage(browser, page);
  await phone.goto(`/workflows/${id}`);
  const flow = phone.getByTestId("workflow-mobile-flow");
  await expect(flow).toBeVisible({ timeout: 15_000 });
  await flow.getByRole("button", { name: /明細を返す/ }).filter({ hasText: "明細を返す" }).first().click();
  const sheet = phone.getByRole("dialog");
  const phoneField = sheet.locator('[data-value-expr="表にするリスト"]');
  await expect(phoneField.getByRole("group", { name: /^\d 件目$/ })).toHaveCount(3);
  await phoneField.scrollIntoViewIfNeeded();
  expect(await sheet.evaluate((el) => el.scrollWidth <= el.clientWidth + 1)).toBe(true);
  if (SHOTS) await phone.screenshot({ path: `${SHOTS}/value-compose-list-phone.png` });
});

test("入れ子が深い値は JSON の欄のまま出して理由を言い、保存しても値は変わらない（BUG-118）", async ({ page }) => {
  test.setTimeout(90_000);
  await page.setViewportSize({ width: 1440, height: 900 });
  await loginViaKeycloak(page);
  const deep = { order: { lines: { first: title } }, note: "そのまま" };
  const id = await createWorkflow(page, [
    { id: "s", type: "script.run", label: "受付", params: { source: { inline: "function main(input) { return input; }" }, input: deep } },
  ]);
  await page.goto(`/workflows/${id}`);
  const panel = page.getByRole("complementary", { name: "ブロックの設定" });
  await page.locator(".react-flow").getByText("受付").first().click();
  const field = panel.locator('[data-value-expr="渡す入力（省略可）"]');
  await expect(field.getByRole("combobox", { name: "値の入れ方" })).toHaveText("直接入力");
  await expect(field.getByTestId("compose-blocked")).toHaveText(
    "入れ子が 3 段以上あるため、「項目を並べる」では組み立てられません（組み立てられるのは 2 段まで）。JSON のまま直してください。",
  );
  expect(JSON.parse(await field.getByRole("textbox", { name: "渡す入力（省略可）" }).inputValue())).toEqual(deep);
  if (SHOTS) await page.screenshot({ path: `${SHOTS}/value-compose-deep-pc.png` });
  await field.getByRole("combobox", { name: "値の入れ方" }).click();
  await expect(page.getByRole("option", { name: "項目を並べる" })).toHaveAttribute("aria-disabled", "true");
  if (SHOTS) await page.screenshot({ path: `${SHOTS}/value-compose-deep-menu-pc.png` });
  await page.keyboard.press("Escape");
  await expect(page.getByRole("listbox")).toHaveCount(0);
  // ブロック名を変えて保存しても（この欄に触れずに）、深い値はそのまま残る。
  await panel.getByRole("textbox", { name: "ブロック名" }).fill("受付（深い入力）");
  await save(page);
  expect((await savedParams(page, id, "s")).input).toEqual(deep);
});
