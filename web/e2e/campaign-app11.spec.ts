import { expect, test, type Page } from "@playwright/test";

import { ASSET, HOLD_SEC, LOAN, OVERDUE_AT, OVERDUE_DAY, TEXT, app11World, overdueGrants } from "./campaign/apps/app11";
import { expectDenied, expectOnlyOwn, expectOutsiderSeesNothing, expectStatus } from "./campaign/checks";
import { call, ok } from "./campaign/http";
import { people, phoneOf, type People, type PersonName } from "./campaign/people";
import { gridRows, pressApi, pressApiDone, pressInApp, replyText, replyTones, waitAppRun, type AppRun } from "./campaign/press";
import { noSideScroll, shot } from "./campaign/shots";
import { addDays, jstToday, runAtDone, withMinuteCron } from "./campaign/time";
import { audit, cmp, expectSingleRow, files, rows, runs, type AuditEvent, type DataRecord } from "./campaign/truth";
import { buildWorld, cleanupWorld, type World } from "./campaign/world";

/// APP-11 備品の貸出管理（台帳 §7）を組んで、使う人として使い、成功判定をデータで確かめる（キャンペーンの E2E の
/// 共通部品 web/e2e/campaign/ の試運転を兼ねる）。成功判定の言い換えは台帳 §7.13 の APP-11:
/// (1) 2 人が同時に同じ備品を借りると、貸出記録はちょうど 1 件で、負けた側には理由が出る（本番の IR では「もう
///     貸出中」か rev の競合のどちらかの節・待ちを挟んだ版では決まって rev の競合の節）。二度押し・同じ冪等キーの
///     送り直し・登録の無い番号でも壊れない。
/// (2) 返却で在庫に戻り、返却日が入る（変わるのは返した備品の行と、その未返却の貸出記録の 2 行だけ）。他人の借りた物は
///     返せない（状態の進め方の判定で断られ、どの行も変わらない）。
/// (3) 延滞者に翌朝通知が届く → 通知（notify.send）は GAP-06・D-2 で見送りなので判定の外。代わりに、延滞チェックが
///     期日切れで未返却の行だけを見つけて一覧に載せ、印（overdue_on）を 1 度だけ付ける。予定の日時の手動の実行
///     （押した人の権限）に加えて、毎分の cron で本物のスケジュールの run（委譲の経路）を 1 回動かす。
/// 権限: 別の組織の charlie はどれも読めず存在も分からない、bob は他人の行と run をミニアプリの入口から読めない。
///
/// 裏取りの決まり:
/// - bob の run の step は誰も読まない（bob にはワークフローを共有しない・alice が /workflows から他人の run を読めるのは
///   #917 の穴）。bob の run で通った節は、bob 本人の記録の結果の文（節ごとに文が違う）と、行の版と、組織の監査
///   （状態の遷移の許可・拒否は 1 件ずつ残る）で確かめる。alice の run は本人の run なので step を読む。
/// - 期待の日付は run の出力から取る（貸出・返却の結果の文に出る `$from run /date`・延滞チェックの transform.date）。
///   端末の今日（jstToday）は、run の日付が押した日か（押す前と終わった後のどちらかの日）の確かめにだけ使う（BUG-283）。
/// 作ったものは afterAll で片付ける（流し直しでは新しい世界を組むので、資産番号は固定でよい）。
/// `E2E_SHOTS_DIR` を渡すと、PC とスマホの画面を撮る。

test.describe.configure({ mode: "serial" });

let P: People;
let W: World;

/// 貸出の返却予定日（今日から 30 日後・延滞にならない・入力の値なので端末の日付でよい）。
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

/// 結果の文から、その run の日付（`$from run /date`）を取り出す（`<見出し>は YYYY-MM-DD`）。
function runDay(run: AppRun, heading: "貸出日" | "返却日"): string {
  const m = new RegExp(`${heading}は (\\d{4}-\\d{2}-\\d{2})`).exec(replyText(run));
  expect(m, `結果に${heading}が出る（${replyText(run)}）`).not.toBeNull();
  return m![1];
}

/// run の日付が押した日（押す前か終わった後の、日本時間の日付のどちらか・日付をまたいでもよい）。
function expectPressDay(day: string, before: string, after: string): void {
  expect([before, after], `run の日付 ${day} は押した日`).toContain(day);
}

/// 押した本人の記録の結果の文から、貸出の run が通った節を引く（文は節ごとに違う）。
function lendBranch(run: AppRun): "lost" | "busy" {
  const text = replyText(run);
  if (text === TEXT.lost) return "lost";
  if (text === TEXT.busy("貸出中")) return "busy";
  throw new Error(`負けた側の結果が、rev の競合の節でも「もう貸出中」の節でもない: ${text}`);
}

/// 行の写し（行の id → 表・資産番号・版）。備品と貸出記録の全部。
type Snapshot = Map<string, { table: string; no: string; rev: number }>;

async function snapshot(): Promise<Snapshot> {
  const out: Snapshot = new Map();
  for (const ref of ["equipment", "loans"] as const) {
    for (const r of await rows.query(alice(), W.tables[ref].id)) out.set(r.id, { table: ref, no: String(r.data.asset_no), rev: r.rev });
  }
  return out;
}

/// 写しから変わった行（版が変わった・増えた・消えた）を `表 資産番号 id` で返す（並べ替え済み）。
async function changedSince(before: Snapshot): Promise<string[]> {
  const now = await snapshot();
  const out: string[] = [];
  for (const [id, r] of now) {
    const b = before.get(id);
    if (!b) out.push(`増えた ${r.table} ${r.no} ${id}`);
    else if (b.rev !== r.rev) out.push(`${r.table} ${r.no} ${id}`);
  }
  for (const [id, b] of before) if (!now.has(id)) out.push(`消えた ${b.table} ${b.no} ${id}`);
  return out.sort();
}

/// 備品の行の状態の遷移の監査（新しい順・alice は組織の管理者）。許可は遷移した 1 回ごと、拒否は進められる人の判定で
/// 断られた 1 回ごとに残る（rev の競合は遷移の前に止まるので残らない）。
async function transitions(recordId: string): Promise<{ allow: AuditEvent[]; deny: AuditEvent[] }> {
  const events = await audit.events(alice(), {
    action: "data.record.transition",
    object_type: "data_record",
    object_id: recordId,
    limit: 100,
  });
  return { allow: events.filter((e) => e.decision === "allow"), deny: events.filter((e) => e.decision === "deny") };
}

/// 押した本人の画面で、その run の記録が終わった状態で、結果の節が `tone` で出ている。
async function expectSeenInApp(page: Page, appId: string, runId: string, tone: string, text: RegExp) {
  await page.goto(`/apps/${appId}`);
  const row = page.getByTestId("yui-runs").locator(`li[data-run-id="${runId}"]`);
  await expect(row).toHaveAttribute("data-status", "succeeded", { timeout: 30_000 });
  if ((await row.getByTestId("yui-run-result").count()) === 0) await row.getByRole("button", { name: "結果" }).click();
  const section = row.getByTestId("yui-run-section").last();
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
  const dayBefore = jstToday();
  const [a, b] = await Promise.all([press(alice()), press(P.bob.page)]);
  expect([a.status, b.status], JSON.stringify([a.body, b.body])).toEqual([200, 200]);
  const [ra, rb] = await Promise.all([waitAppRun(alice(), app.id, a.runId!), waitAppRun(P.bob.page, app.id, b.runId!)]);
  const dayAfter = jstToday();
  // どちらの run も落ちない（業務上の失敗は reply で返して run は成功・engine.md:441）。
  expect([ra.status, rb.status]).toEqual(["succeeded", "succeeded"]);

  // 不変条件: 備品は貸出中で、借りた人は押した 2 人のどちらか 1 人。貸出記録はちょうど 1 件で、その人の分。
  const eq = await equipment(ASSET.race);
  expect(eq.data.status).toBe("貸出中");
  const winner = who(eq.data.borrower);
  expect(winner, `借りた人は押した 2 人のどちらか（${String(eq.data.borrower)}）`).not.toBeNull();
  const [win, lose, loser]: [AppRun, AppRun, PersonName] = winner === "alice" ? [ra, rb, "bob"] : [rb, ra, "alice"];
  const loseId = loser === "alice" ? a.runId! : b.runId!;
  expect(replyTones(win)).toEqual(["success"]);
  const out = runDay(win, "貸出日");
  expectPressDay(out, dayBefore, dayAfter);
  expect(replyText(win)).toBe(TEXT.lent("プロジェクター 1 号機", out, params.due_date));
  const loan = await expectSingleRow(alice(), W.tables.loans.id, cmp("asset_no", "eq", ASSET.race), "貸出記録");
  expect(loan.data).toMatchObject({ borrower: eq.data.borrower, due_date: params.due_date, out_date: out });
  expect(eq.data.due_date).toBe(params.due_date);

  // 遷移したのは勝った人の 1 回だけ（負けた側は rev の競合で遷移の前に止まったか、遷移の節に進まなかった）。
  const audited = await transitions(eq.id);
  expect(audited.allow.map((e) => [e.actor, (e.metadata as { to?: string } | undefined)?.to])).toEqual([[eq.data.borrower, "貸出中"]]);
  expect(audited.deny).toEqual([]);

  // 負けた側は「もう貸出中」（先の run が遷移し終えてから読んだ）か rev の競合（両方が在庫を読んだ）のどちらかの節。
  expect(replyTones(lose)).toEqual(["error"]);
  const branch = lendBranch(lose);
  // alice の run（本人の run）は step で通った節を確かめる。bob の run の step は読まない（#917 に頼らない）ので、
  // bob が負けたときは本人の記録の文（上の lendBranch）が節の証拠になる。
  const d = await runs.get(alice(), W.workflows.lend.id, a.runId!);
  if (winner === "alice") {
    expect(runs.takenPorts(d, "state")).toEqual(["ok"]);
    expect(runs.takenPorts(d, "lend")).toEqual(["out"]);
    expect(runs.stepStatus(d, "done")).toBe("succeeded");
  } else if (branch === "lost") {
    expect(runs.takenPorts(d, "state")).toEqual(["ok"]);
    expect(runs.takenPorts(d, "lend")).toEqual(["error"]);
    expect(runs.errorCode(d, "lend")).toBe("conflict");
    expect(runs.takenPorts(d, "why")).toEqual(["conflict"]);
    expect(runs.takenPorts(d, "set_borrower"), "負けた run は借りた人を書かない").toBeNull();
  } else {
    expect(runs.takenPorts(d, "state")).toEqual(["default"]);
    expect(runs.takenPorts(d, "lend"), "もう貸出中なら遷移しない").toBeNull();
  }

  // 負けた人の画面: 記録は「完了」で、結果に error の色の理由が出る。
  await expectSeenInApp(P[loser].page, app.id, loseId, "error", /ほかの人が先に借りました|いま貸し出せません/);
  await shot(P[loser].page, "app11", "race-loser-pc");
});

test("(1) 待ちを挟んだ版では、両方が在庫を同じ版で読んでから遷移し、負けた側は rev の競合の節を通る", async () => {
  test.setTimeout(240_000);
  const app = W.apps.wait;
  const start = await equipment(ASSET.raceWait);
  expect(start.data.status).toBe("在庫");
  const params = { asset_no: ASSET.raceWait, due_date: dueLater() };
  const press = (p: Page) => pressApi(p, app.id, { version: app.version, actionId: app.actions["借りる"], params });
  const [a, b] = await Promise.all([press(alice()), press(P.bob.page)]);
  expect([a.status, b.status], JSON.stringify([a.body, b.body])).toEqual([200, 200]);
  const wait = 120_000 + HOLD_SEC * 1_000;
  const [ra, rb] = await Promise.all([waitAppRun(alice(), app.id, a.runId!, wait), waitAppRun(P.bob.page, app.id, b.runId!, wait)]);
  expect([ra.status, rb.status]).toEqual(["succeeded", "succeeded"]);

  // 2 つの「探す」step は、どちらも待ちの間（先に遷移した run が待ちを出る前）に終わった: 2 人とも、押す前と同じ版の
  // 「在庫」を読んだ（押した本人の記録の最初の節・探した直後に返す）。
  const seen = TEXT.seen("在庫", start.rev);
  expect([ra, rb].map((r) => [r.reply?.sections[0]?.tone, r.reply?.sections[0]?.text])).toEqual([
    ["neutral", seen],
    ["neutral", seen],
  ]);

  const eq = await equipment(ASSET.raceWait);
  expect(eq.data.status).toBe("貸出中");
  // 版は遷移と借りた人の書き込みの 2 回だけ進む（負けた側は何も書かない）。
  expect(eq.rev).toBe(start.rev + 2);
  const winner = who(eq.data.borrower);
  expect(winner).not.toBeNull();
  await expectSingleRow(alice(), W.tables.loans.id, cmp("asset_no", "eq", ASSET.raceWait), "貸出記録");
  const [win, lose] = winner === "alice" ? [ra, rb] : [rb, ra];
  expect(replyTones(win)).toEqual(["neutral", "success"]);
  // 決まって rev の競合の節（「もう貸出中」の節ではない）。
  expect(replyTones(lose)).toEqual(["neutral", "error"]);
  expect(lose.reply?.sections[1]?.text).toBe(TEXT.lost);
  const audited = await transitions(eq.id);
  expect(audited.allow.map((e) => e.actor)).toEqual([eq.data.borrower]);
  expect(audited.deny).toEqual([]);

  // alice の run（本人の run）の step: 探す → 読んだ状態 → 待ち の後に状態を見て、在庫として遷移へ進んだ。
  const d = await runs.get(alice(), W.workflows.lend_wait.id, a.runId!);
  for (const id of ["find", "seen", "hold"]) expect(runs.stepStatus(d, id), `step ${id}`).toBe("succeeded");
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
  expect(replyText(r2)).toBe(TEXT.busy("貸出中"));
  const loans = await loansOf(ASSET.twice);
  expect(loans.length, "貸出記録は 1 件").toBe(1);
  expect(loans[0].data).toMatchObject({ borrower: P.bob.id, out_date: runDay(r1, "貸出日") });
  const eq = await equipment(ASSET.twice);
  expect(eq.data).toMatchObject({ status: "貸出中", borrower: P.bob.id });

  // 登録の無い番号: run は落ちず、番号を確かめるよう言う。貸出記録は作らない。
  const r3 = await pressApiDone(P.bob.page, app.id, { ...opts, params: { asset_no: ASSET.missing, due_date: dueLater() } });
  expect(r3.status).toBe("succeeded");
  expect(replyText(r3)).toBe(TEXT.missing(ASSET.missing));
  expect(await loansOf(ASSET.missing)).toEqual([]);
});

test("(2) bob が画面から借りて返すと在庫に戻り、返却日が入る。他人の借りた物は返せない", async ({ browser }) => {
  test.setTimeout(240_000);
  const app = W.apps.main;
  const bob = P.bob.page;
  const due = dueLater();
  // 画面から借りる（節「借りる」の部品だけを送る・action_id は画面の記述から求めたものと同じ）。
  const lendDay = jstToday();
  const lent = await pressInApp(bob, app.id, {
    section: "借りる",
    fill: { 資産番号: ASSET.bobUi, 返却予定日: due },
    button: "借りる",
  });
  expect(lent.actionId).toBe(app.actions["借りる"]);
  expect(lent.params).toEqual({ asset_no: ASSET.bobUi, due_date: due });
  expect(lent.run.status).toBe("succeeded");
  const out = runDay(lent.run, "貸出日");
  expectPressDay(out, lendDay, jstToday());
  expect(replyText(lent.run)).toBe(TEXT.lent("ノート PC 4 号機", out, due));
  await expect(bob.getByTestId("yui-runs").locator(`li[data-run-id="${lent.runId}"]`).getByTestId("yui-run-section")).toHaveAttribute(
    "data-tone",
    "success",
  );
  expect((await equipment(ASSET.bobUi)).data).toMatchObject({ status: "貸出中", borrower: P.bob.id, due_date: due });
  const before = await loansOf(ASSET.bobUi);
  const open = before.filter((l) => l.data.returned_on === undefined || l.data.returned_on === null);
  const past = before.filter((l) => l.data.returned_on === LOAN.bobUiPast.returned_on);
  expect(open.length, "未返却の貸出記録は 1 件").toBe(1);
  expect(open[0].data.out_date).toBe(out);
  expect(past.length, "前に返し終わった記録が残っている").toBe(1);
  // bob の「自分の貸出中」に今借りた分が出る（他人の分は出ない）。
  const mine = await gridRows(bob, app.id, app.version, "自分の貸出中");
  expect(mine.status).toBe(200);
  expect(mine.rows.map((r) => r.data.asset_no)).toContain(ASSET.bobUi);
  expectOnlyOwn(mine.rows, "borrower", P.bob.id, "bob の「自分の貸出中」");

  await shot(bob, "app11", "bob-lent-pc");
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
  const eqId = (await equipment(ASSET.bobUi)).id;
  const snap = await snapshot();
  const backDay = jstToday();
  const back = await pressInApp(bob, app.id, { section: "返す", fill: { 資産番号: ASSET.bobUi }, button: "返す" });
  expect(back.actionId).toBe(app.actions["返す"]);
  expect(back.params).toEqual({ asset_no: ASSET.bobUi });
  expect(back.run.status).toBe("succeeded");
  expect(replyTones(back.run)).toEqual(["success"]);
  const on = runDay(back.run, "返却日");
  expectPressDay(on, backDay, jstToday());
  expect(replyText(back.run)).toBe(TEXT.returned("ノート PC 4 号機", on));
  // 変わったのは、返した備品の行と、その未返却だった貸出記録の 2 行だけ（ほかの備品・前の返却済みの記録・ほかの人の
  // 貸出記録は版が変わらず、行は増えも消えもしない）。
  expect(await changedSince(snap), "返却で変わった行").toEqual(
    [`equipment ${ASSET.bobUi} ${eqId}`, `loans ${ASSET.bobUi} ${open[0].id}`].sort(),
  );
  const eq = await equipment(ASSET.bobUi);
  expect(eq.data.status).toBe("在庫");
  expect(eq.data.borrower ?? null).toBeNull();
  expect(eq.data.due_date ?? null).toBeNull();
  const after = await loansOf(ASSET.bobUi);
  expect(after.length).toBe(2);
  expect(after.find((l) => l.id === open[0].id)?.data.returned_on).toBe(on);
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
  const held = await equipment(ASSET.alices);
  const deniedBefore = (await transitions(held.id)).deny.length;
  const snap2 = await snapshot();
  const denied = await pressInApp(bob, app.id, { section: "返す", fill: { 資産番号: ASSET.alices }, button: "返す" });
  expect(denied.run.status).toBe("succeeded");
  expect(replyTones(denied.run)).toEqual(["error"]);
  // この文は、遷移（back）の error の出口 → 理由（why）の forbidden の節だけが返す。
  expect(replyText(denied.run)).toBe(TEXT.notYours);
  expect(await changedSince(snap2), "断られた返却で変わった行").toEqual([]);
  expect((await equipment(ASSET.alices)).data).toMatchObject({ status: "貸出中", borrower: P.alice.id });
  // 断ったのは状態の進め方の「進められる人」の判定（bob の run の step は読まない・#917）: 組織の監査に、bob が
  // 貸出中 → 在庫 へ進めようとして拒否された記録がちょうど 1 件増えた（遷移の error.code=forbidden の元）。
  const deny = (await transitions(held.id)).deny;
  expect(deny.length - deniedBefore, "遷移の拒否の監査が 1 件増えた").toBe(1);
  expect(deny[0]).toMatchObject({ actor: P.bob.id, metadata: { from: "貸出中", to: "在庫" } });
  await expectSeenInApp(bob, app.id, denied.runId, "error", /あなたが借りている備品ではありません/);
  await shot(bob, "app11", "bob-denied-pc");

  // 後ろに記録が増えても（一覧は結果を最新の 1 件にしか付けない）、業務上の失敗を返した記録は「成功」でなく「完了」と
  // 出る（一覧の結果の色合いの要約 reply_tone で決める）。登録の無い番号を押して、断られた返却を古い行にする。
  const extra = await pressApiDone(bob, app.id, {
    version: app.version,
    actionId: app.actions["借りる"],
    params: { asset_no: ASSET.missing, due_date: due },
  });
  expect(replyTones(extra)).toEqual(["error"]);
  const list = await ok<{ runs: (AppRun & { reply_tone?: string })[] }>(bob, "GET", `/mini-apps/${app.id}/runs`);
  const older = list.runs.find((r) => r.run_id === denied.runId);
  expect(older?.reply ?? null, "古い行には結果を付けない").toBeNull();
  expect(older?.reply_tone).toBe("error");
  expect(list.runs.find((r) => r.run_id === lent.runId)?.reply_tone).toBe("success");
  await bob.goto(`/apps/${app.id}`);
  const failedRows = list.runs.filter((r) => r.status === "succeeded" && r.reply_tone === "error" && r.run_id);
  expect(failedRows.length, "業務上の失敗を返した bob の記録").toBeGreaterThanOrEqual(2);
  for (const r of failedRows) {
    const row = bob.getByTestId("yui-runs").locator(`li[data-run-id="${r.run_id}"]`);
    await expect(row, `${r.run_id} は完了`).toContainText("完了", { timeout: 30_000 });
    await expect(row, `${r.run_id} は成功と出ない`).not.toContainText("成功");
  }
  await expect(bob.getByTestId("yui-runs").locator(`li[data-run-id="${lent.runId}"]`)).toContainText("成功");
  await shot(bob, "app11", "bob-older-failures-pc");
});

/// 貸出記録の延滞の印（行の id → overdue_on）。
async function overdueMarks(): Promise<Record<string, unknown>> {
  const all = await rows.query(alice(), W.tables.loans.id);
  return Object.fromEntries(all.map((r) => [r.id, r.data.overdue_on ?? null]));
}

test("(3) 延滞チェック: 予定の日時の実行で、期日切れで未返却の行だけを一覧に載せて印を付け、二度目は印を付け直さない", async () => {
  test.setTimeout(180_000);
  const wf = W.workflows.overdue;
  const folder = W.folders.soumu.id;
  const d1 = await runAtDone(alice(), wf.id, OVERDUE_AT);
  expect(d1.status, JSON.stringify(d1.failure)).toBe("succeeded");
  expect(runs.takenPorts(d1, "any")).toEqual(["true"]);
  // run の日付（transform.date）は、予定の日時の日。以下の期待はこの日から求める。
  const today = (await runs.stepOutput<{ date: string }>(alice(), wf.id, d1.run_id, "today")).date;
  expect(today).toBe(OVERDUE_DAY);

  // 印: 期日がその日より前で未返却の行（E-901・E-902）だけ。当日・期日前・返却済み・今日借りた分には付けない。
  const loans = await rows.query(alice(), W.tables.loans.id);
  for (const r of loans) {
    const no = String(r.data.asset_no);
    const late = no === LOAN.bobLate.asset_no || no === LOAN.aliceLate.asset_no;
    expect(r.data.overdue_on ?? null, `${no} の延滞の印`).toBe(late ? today : null);
  }
  const marks = await overdueMarks();

  // 一覧: 総務のフォルダーの「延滞一覧-<その日>.md」に期日切れの行だけが載る。
  const listed = await files.waitFor(alice(), folder, new RegExp(`^延滞一覧-${today}\\.md$`));
  const text = await files.text(alice(), listed[0].id);
  expect(text).toContain(`# 延滞の一覧（${today}）`);
  expect(text).toContain(`- ${LOAN.bobLate.asset_no}（返却予定日 ${LOAN.bobLate.due_date}・延滞の記録 ${today}）`);
  expect(text).toContain(`- ${LOAN.aliceLate.asset_no}（返却予定日 ${LOAN.aliceLate.due_date}・延滞の記録 ${today}）`);
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
  expect(byNo[LOAN.bobLate.asset_no]?.overdue_on).toBe(today);
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
  const both = await files.waitFor(alice(), folder, new RegExp(`^延滞一覧-${today}.*\\.md$`), 2);
  expect(both.length).toBe(2);
  // bob は総務のフォルダーを読めない（共有していない）。
  const peek = await call(P.bob.page, "GET", `/nodes?parent_id=${folder}`);
  expect([403, 404], `bob が総務のフォルダーを読む → ${peek.status}`).toContain(peek.status);
});

test("(3) 新しい延滞が無い日は、分岐を置かない延滞チェックも、行が 0 件のまとめての登録で成功し、何も変えない", async () => {
  test.setTimeout(180_000);
  const wf = W.workflows.overdue_raw;
  // 1 回目でまだ印の無い延滞の行に印を付け（前のテストを流していれば 0 件）、2 回目は必ず印を付ける行が 0 件になる。
  const first = await runAtDone(alice(), wf.id, OVERDUE_AT);
  expect(first.status, JSON.stringify(first.failure)).toBe("succeeded");
  const marks = await overdueMarks();
  const revs = Object.fromEntries((await rows.query(alice(), W.tables.loans.id)).map((r) => [r.id, r.rev]));
  const d = await runAtDone(alice(), wf.id, OVERDUE_AT);
  // 行が 0 件の data.bulk_upsert は、設定の誤り（invalid）で落とさず、件数 0 で成功する（取込は作らない）。
  expect(d.status, JSON.stringify(d.failure)).toBe("succeeded");
  expect(runs.stepStatus(d, "mark")).toBe("succeeded");
  expect(runs.errorCode(d, "mark")).toBeNull();
  const out = await runs.stepOutput<Record<string, unknown>>(alice(), wf.id, d.run_id, "mark");
  expect(out).toMatchObject({ status: "done", rows_total: 0, rows_applied: 0, rows_invalid: 0, rows_rejected: 0, errors: [], last_error: null });
  const after = await rows.query(alice(), W.tables.loans.id);
  for (const r of after) expect(r.rev, `${String(r.data.asset_no)} の版は変わらない`).toBe(revs[r.id]);
  expect(await overdueMarks()).toEqual(marks);
});

test("(3) 毎分の cron（本物のスケジュール・委譲の経路）でも、期日切れの行にだけ印を付け、前の印は書き換えない", async () => {
  test.setTimeout(300_000);
  // 前のテストの結果に頼らない: 動かす前の印をここで読み、期待はこの印と、run の日付（transform.date）から求める。
  const before = await overdueMarks();
  const src = { ...W.workflows.overdue, grants: overdueGrants(W) };
  await withMinuteCron(alice(), src, async ({ workflowId, run, detail }) => {
    expect(run.trigger_kind).toBe("schedule");
    expect(detail.status, JSON.stringify(detail.failure)).toBe("succeeded");
    const today = (await runs.stepOutput<{ date: string }>(alice(), workflowId, run.run_id, "today")).date;
    // 期日が今日より前で未返却なら延滞。前の実行で印のある行は、その日のまま。
    const loans = await rows.query(alice(), W.tables.loans.id);
    const late = loans.filter((r) => !r.data.returned_on && String(r.data.due_date) < today);
    expect(late.length, "延滞の行がある（seed の期日は過去）").toBeGreaterThan(0);
    for (const r of loans) {
      const no = String(r.data.asset_no);
      const expected = before[r.id] ?? (late.includes(r) ? today : null);
      expect(r.data.overdue_on ?? null, `${no}（期日 ${String(r.data.due_date)}・今日 ${today}）の延滞の印`).toBe(expected);
    }
    // 今日の一覧が総務のフォルダーに置かれた（スケジュールの委譲で書いた）。延滞の行が全部、印の日つきで載る。
    const listed = await files.waitFor(alice(), W.folders.soumu.id, new RegExp(`^延滞一覧-${today}.*\\.md$`));
    const text = await files.text(alice(), listed[0].id);
    expect(text).toContain(`# 延滞の一覧（${today}）`);
    for (const r of late) {
      const mark = String(before[r.id] ?? today);
      expect(text).toContain(`- ${String(r.data.asset_no)}（返却予定日 ${String(r.data.due_date)}・延滞の記録 ${mark}）`);
    }
    for (const r of loans.filter((x) => !late.includes(x))) {
      expect(text, `${String(r.data.asset_no)} は一覧に載らない`).not.toContain(`- ${String(r.data.asset_no)}（`);
    }
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
  // 同じ答え。アーティファクトは権限を先に問うので 403）。
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
