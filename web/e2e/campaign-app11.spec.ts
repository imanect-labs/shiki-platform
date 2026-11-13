import { expect, test, type Page } from "@playwright/test";

import { ASSET, LOAN, OVERDUE_AT, OVERDUE_DAY, app11World } from "./campaign/apps/app11";
import { expectDenied, expectOnlyOwn, expectOutsiderSeesNothing, expectStatus } from "./campaign/checks";
import { call } from "./campaign/http";
import { people, phoneOf, type People, type PersonName } from "./campaign/people";
import { gridRows, pressApi, pressApiDone, pressInApp, replyText, replyTones, waitAppRun, type AppRun } from "./campaign/press";
import { noSideScroll, shot } from "./campaign/shots";
import { addDays, jstToday, runAtDone, withMinuteCron } from "./campaign/time";
import { cmp, expectSingleRow, files, rows, runs, type DataRecord } from "./campaign/truth";
import { buildWorld, cleanupWorld, type World } from "./campaign/world";

/// APP-11 備品の貸出管理（台帳 §7）を組んで、使う人として使い、成功判定をデータで確かめる（キャンペーンの E2E の
/// 共通部品 web/e2e/campaign/ の試運転を兼ねる）。成功判定の言い換えは台帳 §7 の APP-11 と計画のレビュー 12・16:
/// (1) 2 人が同時に同じ備品を借りると、貸出記録はちょうど 1 件で、負けた側には理由が出る（本番の IR では「もう
///     貸出中」か rev の競合のどちらかの節・待ちを挟んだ版では決まって rev の競合の節）。二度押し・同じ冪等キーの
///     送り直し・登録の無い番号でも壊れない。
/// (2) 返却で在庫に戻り、返却日が入る（前の返却済みの記録と他の備品は変わらない）。他人の借りた物は返せない。
/// (3) 延滞者に翌朝通知が届く → 通知（notify.send）は GAP-06・D-2 で見送りなので判定の外。代わりに、延滞チェックが
///     期日切れで未返却の行だけを見つけて一覧に載せ、印（overdue_on）を 1 度だけ付ける。予定の日時の手動の実行
///     （押した人の権限）に加えて、毎分の cron で本物のスケジュールの run（委譲の経路）を 1 回動かす。
/// 権限: 別の組織の charlie はどれも読めず存在も分からない、bob は他人の行と run をミニアプリの入口から読めない。
/// 作ったものは afterAll で片付ける（流し直しでは新しい世界を組むので、資産番号は固定でよい）。
/// `E2E_SHOTS_DIR` を渡すと、PC とスマホの画面を撮る。

test.describe.configure({ mode: "serial" });

let P: People;
let W: World;

/// 貸出の返却予定日（今日から 30 日後・延滞にならない）。
const dueLater = () => addDays(jstToday(), 30);

test.beforeAll(async ({ browser }) => {
  // 組む時間と next dev の初回のコンパイルで 60 秒を超えうる（計画のレビュー 21）。
  test.setTimeout(300_000);
  P = await people(browser);
  W = await buildWorld(P, app11World());
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

async function equipment(no: string): Promise<DataRecord> {
  return expectSingleRow(alice(), W.tables.equipment.id, cmp("asset_no", "eq", no), `備品 ${no}`);
}

async function loansOf(no: string): Promise<DataRecord[]> {
  return rows.query(alice(), W.tables.loans.id, cmp("asset_no", "eq", no));
}

function who(id: unknown): PersonName | null {
  if (id === P.alice.id) return "alice";
  if (id === P.bob.id) return "bob";
  return null;
}

/// 押した本人の画面で、その run の記録が終わった状態で、結果の節が `tone` で出ている。
async function expectSeenInApp(page: Page, appId: string, runId: string, tone: string, text: RegExp) {
  await page.goto(`/apps/${appId}`);
  const row = page.getByTestId("yui-runs").locator(`li[data-run-id="${runId}"]`);
  await expect(row).toHaveAttribute("data-status", "succeeded", { timeout: 30_000 });
  if ((await row.getByTestId("yui-run-result").count()) === 0) await row.getByRole("button", { name: "結果" }).click();
  const section = row.getByTestId("yui-run-section").first();
  await expect(section).toHaveAttribute("data-tone", tone, { timeout: 15_000 });
  await expect(section).toContainText(text);
  // 業務上の失敗は「成功」でなく「完了」と出す（台帳 §9）。
  if (tone === "error") await expect(row).toContainText("完了");
}

test("(1) 2 人が同時に同じ備品を借りると、貸出記録は 1 件だけで、負けた側に理由が出る（本番の IR）", async () => {
  test.setTimeout(180_000);
  const app = W.apps.main;
  const params = { asset_no: ASSET.race, due_date: dueLater() };
  const press = (p: Page) => pressApi(p, app.id, { version: app.version, actionId: app.actions["借りる"], params });
  const [a, b] = await Promise.all([press(alice()), press(P.bob.page)]);
  expect([a.status, b.status], JSON.stringify([a.body, b.body])).toEqual([200, 200]);
  const [ra, rb] = await Promise.all([waitAppRun(alice(), app.id, a.runId!), waitAppRun(P.bob.page, app.id, b.runId!)]);
  // どちらの run も落ちない（業務上の失敗は reply で返して run は成功・engine.md:441）。
  expect([ra.status, rb.status]).toEqual(["succeeded", "succeeded"]);

  // 不変条件: 備品は貸出中で、借りた人は押した 2 人のどちらか 1 人。貸出記録はちょうど 1 件で、その人の分。
  const eq = await equipment(ASSET.race);
  expect(eq.data.status).toBe("貸出中");
  const winner = who(eq.data.borrower);
  expect(winner, `借りた人は押した 2 人のどちらか（${String(eq.data.borrower)}）`).not.toBeNull();
  const loan = await expectSingleRow(alice(), W.tables.loans.id, cmp("asset_no", "eq", ASSET.race), "貸出記録");
  expect(loan.data).toMatchObject({ borrower: eq.data.borrower, due_date: params.due_date, out_date: jstToday() });
  expect(eq.data.due_date).toBe(params.due_date);

  const [win, lose, loser, loseId]: [AppRun, AppRun, PersonName, string] =
    winner === "alice" ? [ra, rb, "bob", b.runId!] : [rb, ra, "alice", a.runId!];
  expect(replyTones(win)).toEqual(["success"]);
  expect(replyText(win)).toContain("を貸し出しました。返却予定日は");
  // 負けた側は「もう貸出中」（先の run が遷移し終えてから読んだ）か rev の競合（両方が在庫を読んだ）のどちらか。
  expect(replyTones(lose)).toEqual(["error"]);
  expect(replyText(lose)).toMatch(/^(ほかの人が先に借りました。表を読み直してください|この備品はいま貸し出せません（貸出中）)$/);

  // alice の run（持ち主で押した本人）の step で、通った節を確かめる。bob の run の step は読まない（#917 に頼らない）。
  const d = await runs.get(alice(), W.workflows.lend.id, a.runId!);
  if (winner === "alice") {
    expect(runs.takenPorts(d, "lend")).toEqual(["out"]);
  } else if (runs.takenPorts(d, "lend")) {
    expect(runs.takenPorts(d, "lend")).toEqual(["error"]);
    expect(runs.errorCode(d, "lend")).toBe("conflict");
    expect(runs.takenPorts(d, "why")).toEqual(["conflict"]);
  } else {
    expect(runs.takenPorts(d, "state")).toEqual(["default"]);
  }

  // 負けた人の画面: 記録は「完了」で、結果に error の色の理由が出る。
  await expectSeenInApp(P[loser].page, app.id, loseId, "error", /ほかの人が先に借りました|いま貸し出せません/);
  await shot(P[loser].page, "app11", "race-loser-pc");
});

test("(1) 待ちを挟んだ版では、両方が在庫を読んでから遷移し、負けた側は rev の競合の節を通る", async () => {
  test.setTimeout(180_000);
  const app = W.apps.wait;
  const params = { asset_no: ASSET.raceWait, due_date: dueLater() };
  const press = (p: Page) => pressApi(p, app.id, { version: app.version, actionId: app.actions["借りる"], params });
  const [a, b] = await Promise.all([press(alice()), press(P.bob.page)]);
  expect([a.status, b.status], JSON.stringify([a.body, b.body])).toEqual([200, 200]);
  const [ra, rb] = await Promise.all([waitAppRun(alice(), app.id, a.runId!, 120_000), waitAppRun(P.bob.page, app.id, b.runId!, 120_000)]);
  expect([ra.status, rb.status]).toEqual(["succeeded", "succeeded"]);

  const eq = await equipment(ASSET.raceWait);
  expect(eq.data.status).toBe("貸出中");
  const winner = who(eq.data.borrower);
  expect(winner).not.toBeNull();
  await expectSingleRow(alice(), W.tables.loans.id, cmp("asset_no", "eq", ASSET.raceWait), "貸出記録");
  const [win, lose] = winner === "alice" ? [ra, rb] : [rb, ra];
  expect(replyTones(win)).toEqual(["success"]);
  // 決まって rev の競合の節（「もう貸出中」の節ではない）。
  expect(replyTones(lose)).toEqual(["error"]);
  expect(replyText(lose)).toBe("ほかの人が先に借りました。表を読み直してください");
  // alice の run の step: 待ちの後に状態を見て、在庫として遷移へ進んだ（どちらが勝っても通る）。
  const d = await runs.get(alice(), W.workflows.lend_wait.id, a.runId!);
  expect(runs.takenPorts(d, "state")).toEqual(["ok"]);
  if (winner === "alice") {
    expect(runs.takenPorts(d, "lend")).toEqual(["out"]);
  } else {
    expect(runs.takenPorts(d, "lend")).toEqual(["error"]);
    expect(runs.errorCode(d, "lend")).toBe("conflict");
    expect(runs.takenPorts(d, "why")).toEqual(["conflict"]);
  }
});

test("(1) 二度押し・同じ冪等キーの送り直し・登録の無い番号でも、貸出記録は増えず、run は落ちない", async () => {
  test.setTimeout(120_000);
  const app = W.apps.main;
  const params = { asset_no: ASSET.twice, due_date: dueLater() };
  const opts = { version: app.version, actionId: app.actions["借りる"], params };
  const first = await pressApi(P.bob.page, app.id, opts);
  expect(first.status, JSON.stringify(first.body)).toBe(200);
  // 同じ冪等キーの送り直しは、前の起動の結果を返し、run を増やさない。
  const again = await pressApi(P.bob.page, app.id, { ...opts, key: first.key });
  expect(again.status, JSON.stringify(again.body)).toBe(200);
  expect(again.runId).toBe(first.runId);
  const r1 = await waitAppRun(P.bob.page, app.id, first.runId!);
  expect(replyTones(r1)).toEqual(["success"]);
  // もう一度押す（別の送信）: もう貸出中なので理由を返し、何も増やさない。
  const r2 = await pressApiDone(P.bob.page, app.id, opts);
  expect(r2.status).toBe("succeeded");
  expect(replyTones(r2)).toEqual(["error"]);
  expect(replyText(r2)).toBe("この備品はいま貸し出せません（貸出中）");
  const loans = await loansOf(ASSET.twice);
  expect(loans.length, "貸出記録は 1 件").toBe(1);
  expect(loans[0].data.borrower).toBe(P.bob.id);
  const eq = await equipment(ASSET.twice);
  expect(eq.data).toMatchObject({ status: "貸出中", borrower: P.bob.id });

  // 登録の無い番号: run は落ちず、番号を確かめるよう言う。貸出記録は作らない。
  const r3 = await pressApiDone(P.bob.page, app.id, { ...opts, params: { asset_no: ASSET.missing, due_date: dueLater() } });
  expect(r3.status).toBe("succeeded");
  expect(replyText(r3)).toBe(`資産番号「${ASSET.missing}」の備品は登録がありません。番号を確かめてください`);
  expect(await loansOf(ASSET.missing)).toEqual([]);
});

test("(2) bob が画面から借りて返すと在庫に戻り、返却日が入る。他人の借りた物は返せない", async ({ browser }) => {
  test.setTimeout(240_000);
  const app = W.apps.main;
  const bob = P.bob.page;
  const due = dueLater();
  // 画面から借りる（節「借りる」の部品だけを送る・action_id は画面の記述から求めたものと同じ）。
  const lent = await pressInApp(bob, app.id, {
    section: "借りる",
    fill: { 資産番号: ASSET.bobUi, 返却予定日: due },
    button: "借りる",
  });
  expect(lent.actionId).toBe(app.actions["借りる"]);
  expect(lent.params).toEqual({ asset_no: ASSET.bobUi, due_date: due });
  expect(lent.run.status).toBe("succeeded");
  expect(replyText(lent.run)).toBe(`ノート PC 4 号機 を貸し出しました。返却予定日は ${due} です`);
  await expect(bob.getByTestId("yui-runs").locator(`li[data-run-id="${lent.runId}"]`).getByTestId("yui-run-section")).toHaveAttribute(
    "data-tone",
    "success",
  );
  expect((await equipment(ASSET.bobUi)).data).toMatchObject({ status: "貸出中", borrower: P.bob.id, due_date: due });
  const before = await loansOf(ASSET.bobUi);
  const open = before.filter((l) => l.data.returned_on === undefined || l.data.returned_on === null);
  const past = before.filter((l) => l.data.returned_on === LOAN.bobUiPast.returned_on);
  expect(open.length, "未返却の貸出記録は 1 件").toBe(1);
  expect(past.length, "前に返し終わった記録が残っている").toBe(1);
  // bob の「自分の貸出中」に今借りた分が出る（他人の分は出ない）。
  const mine = await gridRows(bob, app.id, app.version, "自分の貸出中");
  expect(mine.status).toBe(200);
  expect(mine.rows.map((r) => r.data.asset_no)).toContain(ASSET.bobUi);
  expectOnlyOwn(mine.rows, "borrower", P.bob.id, "bob の「自分の貸出中」");

  if (process.env.E2E_SHOTS_DIR) await shot(bob, "app11", "bob-lent-pc");
  const phone = await phoneOf(browser, P.bob);
  try {
    await phone.goto(`/apps/${app.id}`);
    await expect(phone.getByTestId("ui-figure").getByRole("button", { name: "返す", exact: true })).toBeVisible({ timeout: 30_000 });
    await expect(phone.getByTestId(`miniapp-data-grid-自分の貸出中`)).toBeVisible({ timeout: 30_000 });
    await noSideScroll(phone);
    await shot(phone, "app11", "bob-phone");
  } finally {
    await phone.context().close();
  }

  // 画面から返す（同じ `as asset_no` の部品が 2 つの節にあっても、押した節の値だけが送られる）。
  const back = await pressInApp(bob, app.id, { section: "返す", fill: { 資産番号: ASSET.bobUi }, button: "返す" });
  expect(back.actionId).toBe(app.actions["返す"]);
  expect(back.params).toEqual({ asset_no: ASSET.bobUi });
  expect(back.run.status).toBe("succeeded");
  expect(replyTones(back.run)).toEqual(["success"]);
  expect(replyText(back.run)).toBe("ノート PC 4 号機 の返却を受け付けました");
  const eq = await equipment(ASSET.bobUi);
  expect(eq.data.status).toBe("在庫");
  expect(eq.data.borrower ?? null).toBeNull();
  expect(eq.data.due_date ?? null).toBeNull();
  const after = await loansOf(ASSET.bobUi);
  expect(after.length).toBe(2);
  expect(after.find((l) => l.id === open[0].id)?.data.returned_on).toBe(jstToday());
  const pastAfter = after.find((l) => l.id === past[0].id)!;
  expect(pastAfter.rev, "前に返し終わった記録は書き換えない").toBe(past[0].rev);
  expect(pastAfter.data.returned_on).toBe(LOAN.bobUiPast.returned_on);
  // 表: 自分の貸出中から消え、貸出できる備品に戻る。
  expect((await gridRows(bob, app.id, app.version, "自分の貸出中")).rows.map((r) => r.data.asset_no)).not.toContain(ASSET.bobUi);
  expect((await gridRows(bob, app.id, app.version, "貸出できる備品")).rows.map((r) => r.data.asset_no)).toContain(ASSET.bobUi);

  // alice が借りた物を bob が返そうとすると、状態の進め方の「進められる人」で断られ、理由が出る。何も変わらない。
  const aliceLent = await pressApiDone(alice(), app.id, {
    version: app.version,
    actionId: app.actions["借りる"],
    params: { asset_no: ASSET.alices, due_date: due },
  });
  expect(replyTones(aliceLent)).toEqual(["success"]);
  const heldBefore = await equipment(ASSET.alices);
  const loanBefore = await loansOf(ASSET.alices);
  const denied = await pressInApp(bob, app.id, { section: "返す", fill: { 資産番号: ASSET.alices }, button: "返す" });
  expect(denied.run.status).toBe("succeeded");
  expect(replyTones(denied.run)).toEqual(["error"]);
  expect(replyText(denied.run)).toBe("あなたが借りている備品ではありません。借りている人か総務に頼んでください");
  const held = await equipment(ASSET.alices);
  expect(held.rev).toBe(heldBefore.rev);
  expect(held.data).toMatchObject({ status: "貸出中", borrower: P.alice.id });
  expect(await loansOf(ASSET.alices)).toEqual(loanBefore);
  await expectSeenInApp(bob, app.id, denied.runId, "error", /あなたが借りている備品ではありません/);
  await shot(bob, "app11", "bob-denied-pc");
});

/// 延滞チェックの後の、貸出記録の延滞の印（資産番号 → overdue_on）。
async function overdueMarks(): Promise<Record<string, unknown>> {
  const all = await rows.query(alice(), W.tables.loans.id);
  return Object.fromEntries(all.map((r) => [String(r.data.asset_no), r.data.overdue_on ?? null]));
}

const SEEDED = Object.values(LOAN).map((l) => l.asset_no as string);

test("(3) 延滞チェック: 予定の日時の実行で、期日切れで未返却の行だけを一覧に載せて印を付け、二度目は印を付け直さない", async () => {
  test.setTimeout(180_000);
  const wf = W.workflows.overdue;
  const folder = W.folders.soumu.id;
  const d1 = await runAtDone(alice(), wf.id, OVERDUE_AT);
  expect(d1.status, JSON.stringify(d1.failure)).toBe("succeeded");
  expect(runs.takenPorts(d1, "any")).toEqual(["true"]);
  const today = await runs.stepOutput<{ date: string }>(alice(), wf.id, d1.run_id, "today");
  expect(today.date).toBe(OVERDUE_DAY);

  // 印: 期日が 11-09 より前で未返却の行（E-901・E-902）だけ。当日・期日前・返却済み・今日借りた分には付けない。
  const marks = await overdueMarks();
  for (const [no, mark] of Object.entries(marks)) {
    const late = no === LOAN.bobLate.asset_no || no === LOAN.aliceLate.asset_no;
    expect(mark, `${no} の延滞の印`).toBe(late ? OVERDUE_DAY : null);
  }

  // 一覧: 総務のフォルダーの「延滞一覧-2026-11-09.md」に期日切れの行だけが載る。
  const listed = await files.waitFor(alice(), folder, new RegExp(`^延滞一覧-${OVERDUE_DAY}\\.md$`));
  const text = await files.text(alice(), listed[0].id);
  expect(text).toContain(`# 延滞の一覧（${OVERDUE_DAY}）`);
  expect(text).toContain(`- ${LOAN.bobLate.asset_no}（返却予定日 ${LOAN.bobLate.due_date}・延滞の記録 ${OVERDUE_DAY}）`);
  expect(text).toContain(`- ${LOAN.aliceLate.asset_no}（返却予定日 ${LOAN.aliceLate.due_date}`);
  for (const no of [LOAN.bobToday, LOAN.aliceLater, LOAN.bobReturned, LOAN.bobUiPast].map((l) => l.asset_no)) {
    expect(text, `${no} は一覧に載らない`).not.toContain(no);
  }
  for (const no of Object.values(ASSET)) expect(text, `${no} は一覧に載らない`).not.toContain(no);

  // bob の「自分の貸出中」に、bob の延滞の行と印が出て、他人の貸出は出ない。返却済みの行も出ない。
  const app = W.apps.main;
  const mine = await gridRows(P.bob.page, app.id, app.version, "自分の貸出中");
  expect(mine.status).toBe(200);
  expectOnlyOwn(mine.rows, "borrower", P.bob.id, "bob の「自分の貸出中」");
  const byNo = Object.fromEntries(mine.rows.map((r) => [String(r.data.asset_no), r.data]));
  expect(byNo[LOAN.bobLate.asset_no]?.overdue_on).toBe(OVERDUE_DAY);
  expect(byNo[LOAN.bobToday.asset_no]).toBeDefined();
  expect(byNo[LOAN.bobToday.asset_no]?.overdue_on ?? null).toBeNull();
  expect(byNo[LOAN.bobReturned.asset_no]).toBeUndefined();
  expect(byNo[LOAN.aliceLate.asset_no]).toBeUndefined();

  // 同じ予定の日時でもう一度動かしても、印は付け直さない（行の版も変わらない）。一覧は同じ 2 件のまま。
  const revs = Object.fromEntries((await rows.query(alice(), W.tables.loans.id)).map((r) => [r.id, r.rev]));
  const d2 = await runAtDone(alice(), wf.id, OVERDUE_AT);
  expect(d2.status, JSON.stringify(d2.failure)).toBe("succeeded");
  expect(runs.takenPorts(d2, "any")).toEqual(["false"]);
  expect(runs.stepStatus(d2, "mark"), "印を付ける行が無いので一括の書き込みは通らない").toBe("skipped");
  const after = await rows.query(alice(), W.tables.loans.id);
  for (const r of after) expect(r.rev, `${String(r.data.asset_no)} の版は変わらない`).toBe(revs[r.id]);
  expect(await overdueMarks()).toEqual(marks);
  const both = await files.waitFor(alice(), folder, new RegExp(`^延滞一覧-${OVERDUE_DAY}.*\\.md$`), 2);
  expect(both.length).toBe(2);
  // bob は総務のフォルダーを読めない（共有していない）。
  const peek = await call(P.bob.page, "GET", `/nodes?parent_id=${folder}`);
  expect([403, 404], `bob が総務のフォルダーを読む → ${peek.status}`).toContain(peek.status);
});

test("(3) 毎分の cron（本物のスケジュール・委譲の経路）でも、期日切れの行にだけ印を付け、前の印は書き換えない", async () => {
  test.setTimeout(300_000);
  const before = await overdueMarks();
  await withMinuteCron(alice(), W.workflows.overdue, async ({ workflowId, run, detail }) => {
    expect(run.trigger_kind).toBe("schedule");
    expect(detail.status, JSON.stringify(detail.failure)).toBe("succeeded");
    const today = (await runs.stepOutput<{ date: string }>(alice(), workflowId, run.run_id, "today")).date;
    // 期日が今日より前で未返却なら延滞。前の実行で印のある行は、その日のまま。
    const loans = await rows.query(alice(), W.tables.loans.id);
    for (const r of loans) {
      const no = String(r.data.asset_no);
      const late = !r.data.returned_on && String(r.data.due_date) < today;
      const expected = before[no] ?? (late ? today : null);
      expect(r.data.overdue_on ?? null, `${no}（期日 ${String(r.data.due_date)}・今日 ${today}）の延滞の印`).toBe(expected);
    }
    // 今日の一覧が総務のフォルダーに置かれた（スケジュールの委譲で書いた）。
    const listed = await files.waitFor(alice(), W.folders.soumu.id, new RegExp(`^延滞一覧-${today}.*\\.md$`));
    const text = await files.text(alice(), listed[0].id);
    expect(text).toContain(`- ${LOAN.bobLate.asset_no}（返却予定日 ${LOAN.bobLate.due_date}・延滞の記録 ${OVERDUE_DAY}）`);
    expect(text).not.toContain(LOAN.bobReturned.asset_no);
    for (const no of SEEDED) expect(Object.keys(before)).toContain(no);
  });
});

test("権限: charlie はどれも読めず存在も分からない。bob は他人の行と run をミニアプリの入口から読めない", async () => {
  test.setTimeout(120_000);
  const app = W.apps.main;
  // alice と bob の run を 1 本ずつ（どちらも借りられない番号で、表を変えない）。
  const missing = { asset_no: ASSET.missing, due_date: dueLater() };
  const opts = { version: app.version, actionId: app.actions["借りる"], params: missing };
  const aRun = await pressApiDone(alice(), app.id, opts);
  const bRun = await pressApiDone(P.bob.page, app.id, opts);

  // ミニアプリの記録は本人の分だけ（他人の run は存在を区別せず 404）。
  await expectStatus(P.bob.page, [{ path: `/mini-apps/${app.id}/runs/${aRun.run_id}`, what: "alice の run" }]);
  await expectStatus(alice(), [{ path: `/mini-apps/${app.id}/runs/${bRun.run_id}`, what: "bob の run" }]);
  const bobList = await call<{ runs: { run_id?: string }[] }>(P.bob.page, "GET", `/mini-apps/${app.id}/runs`);
  expect(bobList.status).toBe(200);
  expect(bobList.body.runs.map((r) => r.run_id)).not.toContain(aRun.run_id);
  // bob はワークフローを共有されていない（アプリに束ねただけ）ので、ワークフローとその run も読めない（存在しない id と
  // 同じ答え。アーティファクトは権限を先に問うので 403・run の詳細の API の説明は 404 と書く・キャンペーンの記録を参照）。
  await expectDenied(P.bob.page, [
    { path: `/workflows/${W.workflows.lend.id}/runs`, what: "貸出の run" },
    { path: `/workflows/${W.workflows.lend.id}/runs/${aRun.run_id}`, what: "alice の貸出の run" },
    { path: `/workflows/${W.workflows.overdue.id}`, what: "延滞チェック" },
  ]);
  // bob が貸出記録を直接読んでも、行の決まりで自分の分だけ。
  const direct = await rows.try(P.bob.page, W.tables.loans.id);
  expect(direct.status).toBe(200);
  expect(direct.items.length).toBeGreaterThan(0);
  expectOnlyOwn(direct.items, "borrower", P.bob.id, "bob が読んだ貸出記録");
  // alice（総務）には全員の分が見える。
  const everyone = await rows.query(alice(), W.tables.loans.id);
  expect(new Set(everyone.map((r) => r.data.borrower))).toEqual(new Set([P.alice.id, P.bob.id]));

  // charlie（別の組織）は、表・ビュー・ワークフロー・run・アプリ・押す・表を読む・フォルダーのどれも読めず、存在しない
  // id のときと同じ答え。
  await expectOutsiderSeesNothing(P.charlie.page, W, {
    workflowRuns: [["lend", aRun.run_id]],
    appRuns: [
      ["main", aRun.run_id],
      ["main", bRun.run_id],
    ],
  });
});
