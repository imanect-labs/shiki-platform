import { expect, test, type Page } from "@playwright/test";

import { CSV_COLUMNS, ITEM, MISSING_SKU, OLD_PO, app06World, seenText } from "./campaign/apps/app06";
import { expectDenied, expectOutsiderSeesNothing, expectStatus } from "./campaign/checks";
import { call, type Json } from "./campaign/http";
import { people, phoneOf, type People, type PersonName } from "./campaign/people";
import { gridRows, pressApi, pressApiDone, pressInApp, replyText, replyTones, waitAppRun, type AppRun } from "./campaign/press";
import { noSideScroll, shot } from "./campaign/shots";
import { addDays, runAt, withMinuteCron } from "./campaign/time";
import { cmp, files, rows, runs, type DataRecord, type NodeItem, type RunDetail } from "./campaign/truth";
import { buildWorld, cleanupWorld, type World } from "./campaign/world";

/// APP-06 在庫の発注点アラートと発注書の下書き（台帳 §7・§7.13 の APP-06）を組んで、倉庫（alice）と購買（bob）として
/// 使い、成功判定をデータで確かめる。LLM は使わない。長い待ちは毎分の cron の 1 つだけ。
/// (1) 発注点を割った品目だけが下書きになる: 不足 > 0・不足 0・足りている・発注点なし・発注済の発注あり・取消の発注
///     あり の品目を置き、本物のスケジュール（毎分の写し・委譲の経路）で 1 回、手動（予定の日時）で 1 回動かす。
///     発注の SKU の集合がちょうど {不足 > 0 かつ未完了の発注なし} で、数量 = ceil(不足/ロット)×ロット・下書き・
///     発注のキー = SKU:日付。発注書.csv が同じ SKU と数量を宣言の列の順で持つ。
/// (2) 二重に発注しない: (a) 続けて流しても増えず、発注書も増えない (b) 2 本同時に流しても SKU ごとの未完了の発注は
///     1 件まで (c) 発注済に進めた SKU は作らない (d) 入荷済の後も足りなければ、翌日の実行で 1 件できる。
/// (3) 同時に出庫しても在庫が壊れない: 不変条件（最終の在庫 = 初めの在庫 + 成功した増減の合計・0 未満にならない）で
///     判定する。本番の IR（足す数）でも流す。rev の競合の節は、待ちを挟んだ試験用の版でだけ決まって通す。
/// （追加）結果は押した本人だけに返り、画面の表が読み直される・状態の進め方の権限（購買は 下書き→発注済、倉庫は
/// 発注済→入荷済）・charlie にはどれも見えない。
/// `E2E_SHOTS_DIR` を渡すと、PC とスマホの画面を撮る。

test.describe.configure({ mode: "serial" });

let P: People;
let W: World;
/// 本物のスケジュールの run が数えた「今日」（transform.date の出力・以後の手動の実行もこの日で流す）。
let day = "";

test.beforeAll(async ({ browser }) => {
  test.setTimeout(300_000);
  P = await people(browser);
  W = await buildWorld(P, app06World());
  // 前からある発注を進める（購買の bob: S-106 は発注済、S-107 は取消）。
  for (const [po, to] of [
    [OLD_PO.s106, "発注済"],
    [OLD_PO.s107, "取消"],
  ] as const) {
    const rec = await orderOf(po.po_key);
    expect(await transition(P.bob.page, rec, to), `bob が ${po.sku} の発注を ${to} に進める`).toBe(200);
  }
});

test.afterAll(async () => {
  test.setTimeout(180_000);
  try {
    if (W) await cleanupWorld(W);
  } finally {
    await P?.close();
  }
});

const alice = () => P.alice.page;

async function orders(): Promise<DataRecord[]> {
  return rows.query(alice(), W.tables.orders.id);
}

async function orderOf(poKey: string): Promise<DataRecord> {
  const found = await rows.query(alice(), W.tables.orders.id, cmp("po_key", "eq", poKey));
  expect(found.length, `発注 ${poKey} はちょうど 1 行`).toBe(1);
  return found[0];
}

async function itemOf(sku: string): Promise<DataRecord> {
  const found = await rows.query(alice(), W.tables.items.id, cmp("sku", "eq", sku));
  expect(found.length, `品目 ${sku} はちょうど 1 行`).toBe(1);
  return found[0];
}

/// 発注の状態を進める（`POST /data/tables/{id}/records/{rid}/transition`）。状態コードを返す。
async function transition(page: Page, rec: DataRecord, to: string): Promise<number> {
  const fresh = await rows.get(alice(), W.tables.orders.id, rec.id);
  const r = await call(page, "POST", `/data/tables/${W.tables.orders.id}/records/${rec.id}/transition`, { to, expected_rev: fresh.rev });
  return r.status;
}

const OPEN = ["下書き", "発注済"];

/// 未完了（下書き・発注済）の発注の SKU ごとの件数。
function openCount(all: DataRecord[]): Record<string, number> {
  const out: Record<string, number> = {};
  for (const r of all) if (OPEN.includes(String(r.data.status))) out[String(r.data.sku)] = (out[String(r.data.sku)] ?? 0) + 1;
  return out;
}

/// 手計算: 不足（発注点 − 在庫）が 0 より大きく、未完了の発注が無い品目 → 数量 = ceil(不足/ロット)×ロット。
type Want = { sku: string; qty: number; name: string; supplier: string; due: string };
function expectedDrafts(stock: Record<string, number>, openSkus: Set<string>, on: string): Want[] {
  const out: Want[] = [];
  for (const it of Object.values(ITEM)) {
    const rp = "reorder_point" in it ? it.reorder_point : undefined;
    if (rp === undefined) continue;
    const shortage = rp - (stock[it.sku] ?? it.stock);
    if (shortage <= 0 || openSkus.has(it.sku)) continue;
    out.push({
      sku: it.sku,
      qty: Math.ceil(shortage / it.lot_size) * it.lot_size,
      name: it.name,
      supplier: it.supplier,
      due: addDays(on, it.lead_time_days),
    });
  }
  return out.sort((a, b) => a.sku.localeCompare(b.sku));
}

/// 発注書（`発注書-<日付>.csv`・番号つきも含む）。
async function sheetsOf(on: string): Promise<NodeItem[]> {
  const re = new RegExp(`^発注書-${on}( \\(\\d+\\))?\\.csv$`);
  return (await files.list(alice(), W.folders.po.id)).filter((n) => n.kind === "file" && re.test(n.name));
}

/// 発注書の中身（表のファイルの行・`GET /files/{id}/tabular/rows`）。
async function sheetRows(fileId: string): Promise<{ columns: string[]; rows: (string | null)[][] }> {
  return (await files.tabularRows(alice(), fileId)) as unknown as { columns: string[]; rows: (string | null)[][] };
}

/// 予定の日時を指定して発注点チェックを流し、終わるまで待つ。
async function checkAt(on: string): Promise<RunDetail> {
  const id = await runAt(alice(), W.workflows.check.id, `${on}T08:00`);
  return runs.waitEnd(alice(), W.workflows.check.id, id);
}

/// 発注点チェックの run が新しく作った発注の件数（save を通らなかったら 0）。
async function createdBy(d: RunDetail): Promise<number> {
  if (runs.stepStatus(d, "made") !== "succeeded") return 0;
  return (await runs.stepOutput<{ created: number }>(alice(), W.workflows.check.id, d.run_id, "made")).created;
}

test("(1) 本物のスケジュール（委譲）で、発注点を割って未完了の発注が無い品目だけが下書きになり、発注書.csv が同じ数を持つ", async () => {
  test.setTimeout(300_000);
  const before = await orders();
  expect(before.length, "前からある発注は 2 行").toBe(2);
  expect(openCount(before)).toEqual({ "S-106": 1 });
  await withMinuteCron(alice(), W.workflows.check, async ({ workflowId, run, detail }) => {
    expect(run.trigger_kind).toBe("schedule");
    expect(detail.status, JSON.stringify(detail.failure)).toBe("succeeded");
    day = (await runs.stepOutput<{ date: string }>(alice(), workflowId, run.run_id, "day")).date;
    expect(day).toMatch(/^\d{4}-\d{2}-\d{2}$/);
    expect(runs.takenPorts(detail, "any")).toEqual(["true"]);
    expect(runs.takenPorts(detail, "fresh")).toEqual(["true"]);
    expect(runs.stepStatus(detail, "sheet")).toBe("succeeded");
    const made = await runs.stepOutput<{ created: number }>(alice(), workflowId, run.run_id, "made");
    const save = await runs.stepOutput<Json>(alice(), workflowId, run.run_id, "save");

    // 手計算の期待: 不足 > 0 で未完了の発注が無い品目（S-106 は発注済の発注あり・S-107 の前の発注は取消）。
    const want = expectedDrafts({}, new Set(["S-106"]), day);
    expect(want.map((w) => w.sku)).toEqual(["S-101", "S-102", "S-107"]);
    expect(made.created, `作った件数（save: ${JSON.stringify(save)}）`).toBe(want.length);

    // 行: 新しい発注の SKU の集合・数量・状態・発注のキー・発注番号・納期。前からある発注は変わらない。
    const after = await orders();
    const fresh = after.filter((r) => !before.some((b) => b.id === r.id)).sort((a, b) => String(a.data.sku).localeCompare(String(b.data.sku)));
    expect(fresh.map((r) => r.data.sku)).toEqual(want.map((w) => w.sku));
    for (const [i, w] of want.entries()) {
      expect(fresh[i].data, `発注 ${w.sku}`).toMatchObject({
        po_key: `${w.sku}:${day}`,
        po_no: `PO-${day.replace(/-/g, "")}-${w.sku}`,
        qty: w.qty,
        item_name: w.name,
        supplier: w.supplier,
        ordered_on: day,
        due_on: w.due,
        status: "下書き",
      });
    }
    for (const b of before) expect(after.find((r) => r.id === b.id)?.rev, `前からある発注 ${String(b.data.sku)} は変わらない`).toBe(b.rev);
    expect(Object.values(openCount(after)).every((n) => n === 1), JSON.stringify(openCount(after))).toBe(true);
    // 自動実行が作った行の作成者（GAP-58）: いまの動きは委譲元（有効化した alice）。docs はワークフローと書いている。
    for (const r of fresh) expect(r.created_by ?? (r as unknown as { owner?: string }).owner, `${String(r.data.sku)} の作成者`).toBe(P.alice.id);

    // 発注書: 今日の発注書がちょうど 1 つ（2 回目の分の run があっても作った 0 件なら書かない）。宣言の列の順で同じ数。
    const sheets = await sheetsOf(day);
    expect(sheets.map((s) => s.name)).toEqual([`発注書-${day}.csv`]);
    const sheet = await sheetRows(sheets[0].id);
    expect(sheet.columns).toEqual(CSV_COLUMNS);
    const lines = [...sheet.rows].sort((a, b) => String(a[1]).localeCompare(String(b[1])));
    expect(lines).toEqual(want.map((w) => [`PO-${day.replace(/-/g, "")}-${w.sku}`, w.sku, w.name, String(w.qty), w.supplier, w.due]));
  });
});

test("(1)(2)(a) 手動でもう一度流しても、発注は増えず、発注書も増えない", async () => {
  test.setTimeout(120_000);
  expect(day, "前の確かめで本物のスケジュールが動いた").not.toBe("");
  const before = await orders();
  const sheetsBefore = await sheetsOf(day);
  const d = await checkAt(day);
  expect(d.status, JSON.stringify(d.failure)).toBe("succeeded");
  // 未完了の発注のある SKU は組まないので、登録（save）まで進まない（作った件数 0）。
  expect((await runs.stepOutput<{ count: number }>(alice(), W.workflows.check.id, d.run_id, "plan")).count).toBe(0);
  expect(runs.takenPorts(d, "any")).toEqual(["false"]);
  expect(runs.stepStatus(d, "save")).toBe("skipped");
  expect(await createdBy(d)).toBe(0);
  const after = await orders();
  expect(after.length).toBe(before.length);
  for (const b of before) expect(after.find((r) => r.id === b.id)?.rev).toBe(b.rev);
  expect((await sheetsOf(day)).map((s) => s.id)).toEqual(sheetsBefore.map((s) => s.id));
});

/// 画面から入出庫を登録する（区分は切り替えのボタン・SKU と数量は欄に入れる）。
async function moveInApp(page: Page, appId: string, v: { sku: string; qty: number; kind: "入庫" | "出庫" }) {
  await page.goto(`/apps/${appId}`);
  const figure = page.getByTestId("ui-figure").first();
  await expect(figure.getByRole("button", { name: "登録", exact: true })).toBeEnabled({ timeout: 30_000 });
  await figure.getByLabel("SKU").fill(v.sku);
  await figure.getByLabel("数量").fill(String(v.qty));
  await figure.getByRole("radio", { name: v.kind }).click();
  return pressInApp(page, appId, { button: "登録", open: false });
}

function gridCaption(page: Page, alias: string) {
  return page.getByTestId(`miniapp-data-grid-${alias}`).locator("figcaption");
}

test("（追加）出庫の結果は押した本人だけに返り、画面の表が読み直されて、発注点割れの品目が /query と一致する", async ({ browser }) => {
  test.setTimeout(240_000);
  const app = W.apps.main;
  const bob = P.bob.page;
  const shortBefore = await rows.query(alice(), W.tables.items.id, cmp("shortage", "gt", 0));
  await bob.goto(`/apps/${app.id}`);
  await expect(gridCaption(bob, "発注点割れの品目")).toContainText(`${shortBefore.length} 件`, { timeout: 30_000 });

  // bob が画面から U-201 を 5 個出庫: 12 → 7（発注点 10 を下回る）。
  const pressed = await moveInApp(bob, app.id, { sku: ITEM.u201.sku, qty: 5, kind: "出庫" });
  expect(pressed.actionId).toBe(app.actions["登録"]);
  expect(pressed.params).toEqual({ sku: ITEM.u201.sku, qty: 5, kind: "out" });
  expect(pressed.run.status).toBe("succeeded");
  expect(replyTones(pressed.run)).toEqual(["warning"]);
  expect(replyText(pressed.run)).toBe(
    `${ITEM.u201.name}（${ITEM.u201.sku}）の在庫は 7 になりました。発注点の 10 を下回ったので、明朝の発注点チェックで発注書の下書きを作ります`,
  );
  const u201 = await itemOf(ITEM.u201.sku);
  expect(u201.data).toMatchObject({ stock: 7, shortage: 3 });
  // 読み直さずに、画面の表に 1 件増えて出る（BUG-19 の道）。
  await expect(gridCaption(bob, "発注点割れの品目")).toContainText(`${shortBefore.length + 1} 件`, { timeout: 30_000 });
  const row = bob.getByTestId("yui-runs").locator(`li[data-run-id="${pressed.runId}"]`);
  await expect(row.getByTestId("yui-run-section").first()).toHaveAttribute("data-tone", "warning");
  await shot(bob, "app06", "bob-out-pc");

  // 画面の表（bob の権限）と /query（alice）の集合が一致する。
  const shortNow = await rows.query(alice(), W.tables.items.id, cmp("shortage", "gt", 0));
  const grid = await gridRows(bob, app.id, app.version, "発注点割れの品目");
  expect(grid.status).toBe(200);
  expect(grid.rows.map((r) => r.data.sku).sort()).toEqual(shortNow.map((r) => r.data.sku).sort());
  expect(grid.rows.map((r) => r.data.sku)).toContain(ITEM.u201.sku);
  const drafts = await gridRows(bob, app.id, app.version, "下書きの発注");
  const draftRows = await rows.query(alice(), W.tables.orders.id, cmp("status", "eq", "下書き"));
  expect(drafts.rows.map((r) => r.data.po_key).sort()).toEqual(draftRows.map((r) => r.data.po_key).sort());

  const phone = await phoneOf(browser, P.bob);
  try {
    await phone.goto(`/apps/${app.id}`);
    await expect(phone.getByTestId("ui-figure").getByRole("button", { name: "登録", exact: true })).toBeVisible({ timeout: 30_000 });
    await expect(gridCaption(phone, "発注点割れの品目")).toContainText(`${shortNow.length} 件`, { timeout: 30_000 });
    await noSideScroll(phone);
    await shot(phone, "app06", "bob-phone");
  } finally {
    await phone.context().close();
  }

  // alice が API から U-202 を 4 個出庫（同じ入口・本人の権限）: 11 → 7。
  const a = await pressApiDone(alice(), app.id, {
    version: app.version,
    actionId: app.actions["登録"],
    params: { sku: ITEM.u202.sku, qty: 4, kind: "out" },
  });
  expect(replyTones(a)).toEqual(["warning"]);
  expect((await itemOf(ITEM.u202.sku)).data).toMatchObject({ stock: 7, shortage: 3 });
  // 結果は押した本人だけ: bob は alice の run を、alice は bob の run を、ミニアプリの入口から読めない。
  await expectStatus(bob, [{ path: `/mini-apps/${app.id}/runs/${a.run_id}`, what: "alice の run" }]);
  await expectStatus(alice(), [{ path: `/mini-apps/${app.id}/runs/${pressed.runId}`, what: "bob の run" }]);
  const bobList = await call<{ runs: { run_id?: string }[] }>(bob, "GET", `/mini-apps/${app.id}/runs`);
  expect(bobList.status).toBe(200);
  expect(bobList.body.runs.map((r) => r.run_id)).toContain(pressed.runId);
  expect(bobList.body.runs.map((r) => r.run_id)).not.toContain(a.run_id);
});

test("(2)(b) 発注点チェックを 2 本同時に流しても、SKU ごとの未完了の発注は 1 件まで", async () => {
  test.setTimeout(180_000);
  const before = await orders();
  const sheetsBefore = await sheetsOf(day);
  const openBefore = new Set(Object.keys(openCount(before)));
  // 前の確かめで U-201・U-202 が発注点を下回った（未完了の発注は無い）。
  const want = expectedDrafts({ [ITEM.u201.sku]: 7, [ITEM.u202.sku]: 7 }, openBefore, day);
  expect(want.map((w) => w.sku)).toEqual([ITEM.u201.sku, ITEM.u202.sku]);

  const ids = await Promise.all([0, 1].map(() => runAt(alice(), W.workflows.check.id, `${day}T08:00`)));
  const ds = await Promise.all(ids.map((id) => runs.waitEnd(alice(), W.workflows.check.id, id)));
  const created: number[] = [];
  const paths: string[] = [];
  for (const d of ds) {
    expect(d.status, JSON.stringify(d.failure)).toBe("succeeded");
    created.push(await createdBy(d));
    if (runs.takenPorts(d, "any")?.[0] === "false") paths.push("組む前に先の発注が見えた");
    else {
      const s = await runs.stepOutput<{ rows_applied: number; rows_matched?: number; rows_rejected: number }>(
        alice(),
        W.workflows.check.id,
        d.run_id,
        "save",
      );
      paths.push(`適用 ${s.rows_applied}・一致 ${s.rows_matched ?? 0}・断り ${s.rows_rejected}`);
    }
  }
  test.info().annotations.push({ type: "同時に流した 2 本の道", description: paths.join(" / ") });
  // 2 本で作ったのは合わせて 2 件（同じ SKU を 2 本が作ることはない）。
  expect(created.reduce((a, b) => a + b, 0), paths.join(" / ")).toBe(want.length);
  const after = await orders();
  expect(after.length).toBe(before.length + want.length);
  const open = openCount(after);
  for (const [sku, n] of Object.entries(open)) expect(n, `${sku} の未完了の発注`).toBe(1);
  for (const w of want) {
    const r = await orderOf(`${w.sku}:${day}`);
    expect(r.data).toMatchObject({ qty: w.qty, status: "下書き" });
  }
  // 発注書は、作った run の数だけ増える（作った 0 件の run は書かない）。
  const sheets = await sheetsOf(day);
  expect(sheets.length).toBe(sheetsBefore.length + created.filter((n) => n > 0).length);
});

test("(2)(c)(d)・遷移の権限: 発注済に進めた SKU は作らず、入荷済の後も足りなければ翌日の実行で 1 件だけ作る", async () => {
  test.setTimeout(180_000);
  const draft = await orderOf(`${ITEM.s101.sku}:${day}`);
  expect(draft.data.status).toBe("下書き");
  // 倉庫の alice は 下書き→発注済 に進められない（購買だけ）。状態は変わらない。
  expect(await transition(alice(), draft, "発注済")).toBe(403);
  expect((await rows.get(alice(), W.tables.orders.id, draft.id)).data.status).toBe("下書き");
  // 購買の bob は進められる。
  expect(await transition(P.bob.page, draft, "発注済")).toBe(200);
  // 購買の bob は 発注済→入荷済 に進められない（倉庫だけ）。
  expect(await transition(P.bob.page, draft, "入荷済")).toBe(403);

  // (c) 発注済に進めた SKU は、同じ日にもう一度流しても作らない。
  const before = await orders();
  const again = await checkAt(day);
  expect(again.status, JSON.stringify(again.failure)).toBe("succeeded");
  expect(await createdBy(again)).toBe(0);
  expect((await orders()).length).toBe(before.length);

  // (d) 倉庫の alice が入荷済に進める（在庫はまだ足りないまま）→ 翌日の実行で S-101 だけが 1 件できる。
  expect(await transition(alice(), draft, "入荷済")).toBe(200);
  expect((await rows.get(alice(), W.tables.orders.id, draft.id)).data.status).toBe("入荷済");
  const next = addDays(day, 1);
  const d = await checkAt(next);
  expect(d.status, JSON.stringify(d.failure)).toBe("succeeded");
  expect(await createdBy(d)).toBe(1);
  const after = await orders();
  expect(after.length).toBe(before.length + 1);
  const made = await orderOf(`${ITEM.s101.sku}:${next}`);
  expect(made.data).toMatchObject({ sku: ITEM.s101.sku, qty: 12, status: "下書き", ordered_on: next, due_on: addDays(next, ITEM.s101.lead_time_days) });
  for (const [sku, n] of Object.entries(openCount(after))) expect(n, `${sku} の未完了の発注`).toBe(1);
  const sheets = await sheetsOf(next);
  expect(sheets.length).toBe(1);
  const sheet = await sheetRows(sheets[0].id);
  expect(sheet.rows.map((r) => [r[1], r[3]])).toEqual([[ITEM.s101.sku, "12"]]);
});

/// 押した本人の画面で、その run の記録が終わった状態で、結果の最後の節が `tone` で出ている（待ちを挟む版は、その前に
/// 読んだ在庫の節がある）。
async function expectSeenInApp(page: Page, appId: string, runId: string, tone: string, text: RegExp) {
  await page.goto(`/apps/${appId}`);
  const row = page.getByTestId("yui-runs").locator(`li[data-run-id="${runId}"]`);
  await expect(row).toHaveAttribute("data-status", "succeeded", { timeout: 30_000 });
  if ((await row.getByTestId("yui-run-result").count()) === 0) await row.getByRole("button", { name: "結果" }).click();
  const section = row.getByTestId("yui-run-section").last();
  await expect(section).toHaveAttribute("data-tone", tone, { timeout: 15_000 });
  await expect(section).toContainText(text);
}

type Pressed = { who: PersonName; runId: string; run: AppRun };

/// 結果の最後の節の色合いと文（待ちを挟む版は、その前に読んだ在庫の節がある）。
function lastTone(run: AppRun): string | undefined {
  return replyTones(run).at(-1);
}
function lastText(run: AppRun): string | undefined {
  return run.reply?.sections.at(-1)?.text;
}

/// alice と bob が同時に同じ品目を出庫する（同じ入口・本人の権限）。
async function racePress(appId: string, version: number, actionId: string, params: Record<string, unknown>): Promise<Pressed[]> {
  const who: PersonName[] = ["alice", "bob"];
  const sent = await Promise.all(who.map((w) => pressApi(P[w].page, appId, { version, actionId, params })));
  for (const s of sent) expect(s.status, JSON.stringify(s.body)).toBe(200);
  const done = await Promise.all(sent.map((s, i) => waitAppRun(P[who[i]].page, appId, s.runId!, 120_000)));
  return done.map((run, i) => ({ who: who[i], runId: sent[i].runId!, run }));
}

test("(3) 本番の IR: alice と bob が同時に 3 個ずつ出庫しても、在庫 = 初め − 成功した分で、0 未満にならない", async () => {
  test.setTimeout(180_000);
  const app = W.apps.main;
  const it = ITEM.r301;
  const start = await itemOf(it.sku);
  expect(start.data.stock).toBe(it.stock);
  const pressed = await racePress(app.id, app.version, app.actions["登録"], { sku: it.sku, qty: 3, kind: "out" });
  for (const p of pressed) expect(p.run.status, `${p.who} の run`).toBe("succeeded");
  const won = pressed.filter((p) => replyTones(p.run)[0] === "success");
  const lost = pressed.filter((p) => replyTones(p.run)[0] !== "success");
  // 不変条件: 最終の在庫 = 初めの在庫 − 成功した出庫の合計。5 から 3 は 1 回しか引けない。
  const end = await itemOf(it.sku);
  expect(end.data.stock).toBe(it.stock - 3 * won.length);
  expect(Number(end.data.stock)).toBeGreaterThanOrEqual(0);
  expect(won.length, pressed.map((p) => `${p.who}: ${replyText(p.run)}`).join(" / ")).toBe(1);
  expect(replyText(won[0].run)).toBe(`${it.name}（${it.sku}）の在庫は 2 になりました`);
  // 負けた側は error の色の理由: 先に減った在庫を読んだ（足りない）か、足す数の書き込みが最小値で断られた。
  const loser = lost[0];
  expect(replyTones(loser.run)).toEqual(["error"]);
  expect(replyText(loser.run)).toMatch(
    new RegExp(
      `^(在庫が足りません。${it.name}（${it.sku}）のいまの在庫は 2 です|ほかの人の登録と重なって、在庫が足りなくなりました。表で在庫を確かめてから、もう一度登録してください)$`,
    ),
  );
  // alice の run の step で、通った節を確かめる（bob の run の step は読まない・#917 に頼らない）。
  const aRun = pressed.find((p) => p.who === "alice")!;
  const d = await runs.get(alice(), W.workflows.move.id, aRun.run.run_id!);
  if (won[0].who === "alice") {
    expect(runs.takenPorts(d, "move")).toEqual(["out"]);
  } else if (runs.takenPorts(d, "move")) {
    expect(runs.takenPorts(d, "move")).toEqual(["error"]);
    expect(runs.errorCode(d, "move")).toBe("invalid");
    expect(runs.takenPorts(d, "why")).toEqual(["raced"]);
  } else {
    expect(runs.takenPorts(d, "enough")).toEqual(["false"]);
  }
  await expectSeenInApp(P[loser.who].page, app.id, loser.runId, "error", /在庫が足りません|在庫が足りなくなりました/);
  await shot(P[loser.who].page, "app06", "race-loser-pc");
});

test("(3) 待ちを挟んだ版では、負けた側が rev の競合の節を通り、入庫の後の再操作で在庫が整う", async () => {
  test.setTimeout(300_000);
  const app = W.apps.wait;
  const it = ITEM.r302;
  const action = app.actions["登録"];
  const start = await itemOf(it.sku);
  expect(start.data.stock).toBe(it.stock);
  const pressed = await racePress(app.id, app.version, action, { sku: it.sku, qty: 3, kind: "out" });
  for (const p of pressed) expect(p.run.status, `${p.who} の run`).toBe("succeeded");
  // 2 つの「探す」step は、どちらも待ちの間（先に書いた run が待ちを出る前）に終わった: 2 人とも、押す前と同じ版の
  // 在庫 5 を読んだ（押した本人の記録の最初の節・探した直後に返す）。読んだ版が違えば、下の節の判定は意味が無い。
  const seen = seenText(it.stock, start.rev);
  expect(pressed.map((p) => [p.run.reply?.sections[0]?.tone, p.run.reply?.sections[0]?.text])).toEqual([
    ["neutral", seen],
    ["neutral", seen],
  ]);
  const won = pressed.filter((p) => lastTone(p.run) === "success");
  expect(won.length, pressed.map((p) => `${p.who}: ${replyText(p.run)}`).join(" / ")).toBe(1);
  const loser = pressed.find((p) => p !== won[0])!;
  // 両方が在庫 5 を読んでから書くので、決まって rev の競合の節（在庫が足りない節ではない）。
  expect(replyTones(won[0].run)).toEqual(["neutral", "success"]);
  expect(replyTones(loser.run)).toEqual(["neutral", "error"]);
  expect(lastText(loser.run)).toBe("ほかの人が同時に登録しました。表を読み直して、もう一度登録してください");
  const aRun = pressed.find((p) => p.who === "alice")!;
  const d = await runs.get(alice(), W.workflows.move_wait.id, aRun.run.run_id!);
  expect(runs.takenPorts(d, "enough")).toEqual(["true"]);
  if (won[0].who === "alice") {
    expect(runs.takenPorts(d, "move")).toEqual(["out"]);
  } else {
    expect(runs.takenPorts(d, "move")).toEqual(["error"]);
    expect(runs.errorCode(d, "move")).toBe("conflict");
    expect(runs.takenPorts(d, "why")).toEqual(["conflict"]);
  }
  expect((await itemOf(it.sku)).data.stock).toBe(2);

  // 再操作: 負けた人がもう一度 3 個出庫 → 在庫 2 なので足りない（書かない）→ 5 個入庫 → もう一度 3 個出庫で通る。
  const page = P[loser.who].page;
  const deltas = [-3];
  const retry = await pressApiDone(page, app.id, { version: app.version, actionId: action, params: { sku: it.sku, qty: 3, kind: "out" } });
  expect(lastText(retry)).toBe(`在庫が足りません。${it.name}（${it.sku}）のいまの在庫は 2 です`);
  const restock = await pressApiDone(page, app.id, { version: app.version, actionId: action, params: { sku: it.sku, qty: 5, kind: "in" } });
  expect(replyTones(restock)).toEqual(["neutral", "success"]);
  deltas.push(5);
  const again = await pressApiDone(page, app.id, { version: app.version, actionId: action, params: { sku: it.sku, qty: 3, kind: "out" } });
  expect(replyTones(again)).toEqual(["neutral", "success"]);
  expect(lastText(again)).toBe(`${it.name}（${it.sku}）の在庫は 4 になりました`);
  deltas.push(-3);
  // 不変条件: 最終の在庫 = 初めの在庫 + 成功した増減の合計。
  expect((await itemOf(it.sku)).data.stock).toBe(it.stock + deltas.reduce((a, b) => a + b, 0));
  await expectSeenInApp(page, app.id, loser.runId, "error", /ほかの人が同時に登録しました/);
});

test("権限: charlie はどれも読めず存在も分からない。bob はワークフローを読めず、登録の無い SKU でも run は落ちない", async () => {
  test.setTimeout(120_000);
  const app = W.apps.main;
  const opts = { version: app.version, actionId: app.actions["登録"], params: { sku: MISSING_SKU, qty: 1, kind: "out" } };
  const itemsBefore = await rows.query(alice(), W.tables.items.id);
  const aRun = await pressApiDone(alice(), app.id, opts);
  const bRun = await pressApiDone(P.bob.page, app.id, opts);
  for (const r of [aRun, bRun]) {
    expect(r.status).toBe("succeeded");
    expect(replyTones(r)).toEqual(["error"]);
    expect(replyText(r)).toBe(`SKU「${MISSING_SKU}」の品目は登録がありません。SKU を確かめてください`);
  }
  const itemsAfter = await rows.query(alice(), W.tables.items.id);
  expect(itemsAfter.map((r) => [r.id, r.rev])).toEqual(itemsBefore.map((r) => [r.id, r.rev]));

  // bob はワークフローを共有されていない（アプリに束ねただけ）ので、ワークフローとその run を読めない。
  await expectDenied(P.bob.page, [
    { path: `/workflows/${W.workflows.move.id}/runs`, what: "入出庫の run" },
    { path: `/workflows/${W.workflows.move.id}/runs/${aRun.run_id}`, what: "alice の入出庫の run" },
    { path: `/workflows/${W.workflows.check.id}`, what: "発注点チェック" },
  ]);
  await expectStatus(P.bob.page, [{ path: `/mini-apps/${app.id}/runs/${aRun.run_id}`, what: "alice の run" }]);

  // charlie（別の組織）は、表・ビュー・ワークフロー・run・アプリ・押す・表を読む・フォルダー・発注書のどれも読めない。
  const sheets = await sheetsOf(day);
  const probes = sheets.map((s) => ({ path: `/files/${s.id}/text`, what: `発注書 ${s.name}` }));
  await expectDenied(P.charlie.page, probes);
  await expectOutsiderSeesNothing(P.charlie.page, W, {
    workflowRuns: [["move", aRun.run_id]],
    appRuns: [
      ["main", aRun.run_id],
      ["main", bRun.run_id],
    ],
  });
});
