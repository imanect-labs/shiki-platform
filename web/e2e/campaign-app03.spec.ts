import { expect, test, type Page } from "@playwright/test";

import {
  INJECTION,
  LATE,
  LLM_VARIANTS,
  NAMES,
  REPORTS,
  REPORTS_K,
  WEEK,
  WHY,
  WHY_OTHER,
  app03World,
  cronReportRows,
  cronReports,
  totalsOf,
  yen,
  type Report,
} from "./campaign/apps/app03";
import { expectDenied, expectOnlyOwn, expectOutsiderSeesNothing, expectStatus } from "./campaign/checks";
import { call, ok, type Json } from "./campaign/http";
import { people, phoneOf, type People } from "./campaign/people";
import { gridRows, pressApi, pressApiDone, pressInApp, replyText, replyTones } from "./campaign/press";
import { noSideScroll, shot } from "./campaign/shots";
import { CRON_WEEK_MARGIN_MS, awayFromJstWeekBoundary, jstToday, lastWeekStart, runAtDone, withMinuteCron } from "./campaign/time";
import { all, cmp, files, rows, runs, type DataRecord, type NodeItem, type RunDetail } from "./campaign/truth";
import { buildWorld, cleanupWorld, type World } from "./campaign/world";

/// APP-03 日報の収集と週次集計レポート（台帳 §7・§7.13 の APP-03）を組んで、営業（bob）と部長（alice）として使い、
/// 成功判定をデータで確かめる。LLM は stub（形の指定の無い要約は「回答: <prompt>」の echo）。長い待ちは毎分の cron の 1 つだけ。
/// (1) 月曜 9:00 に .md が保存される: (a) cron `0 9 * * 1`・Asia/Tokyo で、次の発火が月曜 09:00+09:00 (b) 予定の日時の
///     実行で、週報のフォルダーに `週報-<週の初め>.md` がちょうど 1 つでき、本文に集計表がある (c) 毎分の写しの本物の
///     スケジュールの run（委譲の経路）で、委譲元 alice の見え方で集計した行とファイルができる。
/// (2) 集計が手計算と一致: fixture から担当者ごとの合計と提出日数を計算し、週次集計と .md の表と一致する（境界の日曜・
///     月曜は除き、週の初めの月曜・終わりの日曜は含む）。K を既定（5）にした写しでは伏せられて null（0 ではない）。
/// (3) 未提出者がレポートに出る: 名簿に在籍で載っていて提出していない alice が、週報の missing と .md の「## 未提出」に出る。
/// (4) catchup で再発火しても二重にならない: 同じ予定の日時で 2 回（間に遅れた提出）と、本物の cron（2 回動かす）で、
///     週次集計は担当者の数のまま値だけ変わり、週報は 1 行、週報のファイルは 1 つのまま。
/// （追加）出し直し・二度押し・担当者を名乗れない・権限・LLM の失敗と打ち切り・所感に仕込んだ指示。
/// 予定の日時の週は、本物の cron の週（今日の前の週）と重ならない過去の週にずらした（app03.ts の WEEK）。
/// `E2E_SHOTS_DIR` を渡すと、PC とスマホの画面を撮る。

test.describe.configure({ mode: "serial" });

let P: People;
let W: World;

test.beforeAll(async ({ browser }) => {
  test.setTimeout(300_000);
  P = await people(browser);
  W = await buildWorld(P, app03World());
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
const idOf = (who: "alice" | "bob") => P[who].id;

type Totals = Record<string, { visits: number; deals: number; prospect: number; days: number }>;

/// 週次集計の行（週の初めで絞る）。
async function totalsRows(weekStart: string): Promise<DataRecord[]> {
  return rows.query(alice(), W.tables.totals.id, cmp("week_start", "eq", weekStart));
}

/// 週次集計の行が手計算と一致する（担当者ごとに 1 行・キーは「週の初め|担当者」）。
async function expectTotals(weekStart: string, want: Totals): Promise<DataRecord[]> {
  const got = await totalsRows(weekStart);
  const byRep = Object.fromEntries(got.map((r) => [String(r.data.rep), r.data]));
  const expected = Object.fromEntries(
    Object.entries(want).map(([who, t]) => [idOf(who as "alice" | "bob"), { key: `${weekStart}|${idOf(who as "alice" | "bob")}`, week_start: weekStart, ...t }]),
  );
  expect(got.length, `週次集計（${weekStart}）は担当者ごとに 1 行`).toBe(Object.keys(want).length);
  for (const [rep, t] of Object.entries(expected)) expect(byRep[rep], `週次集計（${weekStart}）の ${rep}`).toMatchObject(t);
  return got;
}

/// 週報のファイル（`週報-<週の初め>.md`・番号つきも含む）。
async function reportFiles(weekStart: string): Promise<NodeItem[]> {
  const re = new RegExp(`^週報-${weekStart}( \\(\\d+\\))?\\.md$`);
  return (await files.list(alice(), W.folders.weekly.id)).filter((n) => n.kind === "file" && re.test(n.name));
}

/// 週報の行（週のキーで絞る・ちょうど 1 行）。
async function reportRow(weekStart: string): Promise<DataRecord> {
  const found = await rows.query(alice(), W.tables.reports.id, cmp("week_key", "eq", weekStart));
  expect(found.length, `週報（${weekStart}）はちょうど 1 行`).toBe(1);
  return found[0];
}

/// .md の集計表の 1 行（script と同じ形）。
function tableLine(name: string, t: { visits: number; deals: number; prospect: number; days: number }): string {
  return `| ${name} | ${t.visits} | ${t.deals} | ${yen(t.prospect)} | ${t.days} |`;
}

/// 本文の節（最初の `## 見出し` から次の `## ` の前まで）。「まとめ（AI）」は最後の節で、stub の echo が集計表の
/// 見出しを含むので、終わりまでを返す。
function section(md: string, heading: string): string {
  const at = md.indexOf(`## ${heading}\n`);
  if (at < 0) return "";
  const rest = md.slice(at + heading.length + 4);
  if (heading === "まとめ（AI）") return rest;
  const next = rest.indexOf("\n## ");
  return next < 0 ? rest : rest.slice(0, next);
}

test("(1)(a) 週報のきっかけは毎週月曜 9 時（日本時間）で、次の発火は月曜の 09:00+09:00", async () => {
  const ir = (await ok<{ ir: { triggers: Json[] } }>(alice(), "GET", `/workflows/${W.workflows.weekly.id}`)).ir;
  expect(ir.triggers).toEqual([expect.objectContaining({ kind: "schedule", cron: "0 9 * * 1", tz: "Asia/Tokyo" })]);
  // 有効化は組み立てで同意計画と照らして済ませた（委譲は alice）。
  expect(W.workflows.weekly.enabled).toBe(true);
  const preview = await ok<{ next: string[]; error?: string }>(alice(), "POST", "/workflows/schedule-preview", {
    cron: "0 9 * * 1",
    tz: "Asia/Tokyo",
    count: 3,
  });
  expect(preview.error).toBeUndefined();
  expect(preview.next.length).toBe(3);
  for (const at of preview.next) {
    // 日本時間の月曜 9:00 = UTC の月曜 0:00。
    const d = new Date(at);
    expect([d.getUTCDay(), d.getUTCHours(), d.getUTCMinutes()], at).toEqual([1, 0, 0]);
    expect(d.toLocaleString("sv-SE", { timeZone: "Asia/Tokyo" }), at).toMatch(/ 09:00:00$/);
  }
  expect(new Date(preview.next[1]).getTime() - new Date(preview.next[0]).getTime()).toBe(7 * 24 * 3600 * 1000);
});

/// IR の中で `$from` が指す先（ノードの id → 参照する場所の一覧 `<ノード>:<params の場所>`）。
function consumersOf(ir: Json, source: string): string[] {
  const out: string[] = [];
  const walk = (v: unknown, node: string, path: string) => {
    if (Array.isArray(v)) v.forEach((x, i) => walk(x, node, `${path}/${i}`));
    else if (v && typeof v === "object") {
      const o = v as Json;
      if (o.$from === source) out.push(`${node}:${path}`);
      for (const [k, x] of Object.entries(o)) walk(x, node, `${path}/${k}`);
    }
  };
  for (const n of ir.nodes as Json[]) walk(n.params, String(n.id), "");
  return out.sort();
}

let mainRun = "";

test("(1)(b)(2)(3) 予定の日時（10-19 月 9:00）の実行で、先週の集計が手計算と一致し、未提出者と集計表の週報が 1 つできる", async () => {
  test.setTimeout(180_000);
  const wf = W.workflows.weekly;
  const wk = WEEK.main;
  const d = await runAtDone(alice(), wf.id, wk.at);
  mainRun = d.run_id;
  expect(d.status, JSON.stringify(d.failure)).toBe("succeeded");
  const lastWeek = await runs.stepOutput<{ start: string; end: string }>(alice(), wf.id, d.run_id, "last_week");
  expect([lastWeek.start, lastWeek.end]).toEqual([wk.start, wk.end]);
  const totals = await runs.stepOutput<{ suppressed: boolean; groups: Json[] }>(alice(), wf.id, d.run_id, "totals");
  expect(totals.suppressed, "K = 1 なので伏せない").toBe(false);

  // (2) 手計算: 月曜 10-12 と日曜 10-18 は含み、前の日曜 10-11 と次の月曜 10-19 は除く。
  const want = totalsOf(REPORTS, wk.start, wk.end);
  expect(want).toEqual({ bob: { visits: 10, deals: 4, prospect: 400000, days: 4 } });
  const withBoundary = totalsOf(REPORTS, "2026-10-11", "2026-10-19");
  expect(withBoundary.bob.days, "境界の日を含めると 6 日（含めないことの裏）").toBe(6);
  await expectTotals(wk.start, want);

  // (3) 週報の行: 未提出は在籍で出していない alice。ファイルが付いている。
  const rep = await reportRow(wk.start);
  expect(rep.data).toMatchObject({ week_start: wk.start, missing: NAMES.alice });
  const made = await files.waitFor(alice(), W.folders.weekly.id, new RegExp(`^週報-${wk.start}\\.md$`));
  const sheets = await reportFiles(wk.start);
  expect(sheets.map((s) => s.name)).toEqual([`週報-${wk.start}.md`]);
  expect(rep.data.report_file).toBe(made[0].id);
  expect(runs.takenPorts(d, "save_file")).toEqual(["out"]);

  // (1)(b)(2)(3) 本文: script が作った集計表（同じ数）・未提出（alice だけ）・AI のまとめ（stub の echo）。
  const md = await files.text(alice(), made[0].id);
  expect(md).toContain(`# 週報（${wk.start} 〜 ${wk.end}）`);
  expect(section(md, "集計")).toContain(tableLine(NAMES.bob, want.bob));
  expect(section(md, "集計")).not.toContain(NAMES.alice);
  expect(section(md, "未提出").trim()).toBe(`- ${NAMES.alice}`);
  const ai = section(md, "まとめ（AI）");
  expect(ai).toContain("回答: ");
  expect(String(rep.data.summary).trim(), "週報の summary は本文の「まとめ（AI）」と同じ").toBe(ai.trim());

  // （追加）注入: 所感の指示は AI のまとめにだけ流れ（echo に出る）、週次集計の値・ファイル名・保存先は変わらない。
  expect(ai).toContain(INJECTION);
  expect(section(md, "集計")).toContain(yen(400000));
  const ir = wf.ir;
  expect(consumersOf(ir, "nodes.summarize.output")).toEqual(["body:/input/err", "body:/input/stop", "body:/input/text"]);
  expect(consumersOf(ir, "nodes.body.output")).toEqual([
    "save_file:/content",
    "save_report:/data/summary",
    "save_report_nofile:/data/summary",
  ]);
});

test("(4) 同じ予定の日時でもう一度流す（間に遅れた提出）と、週次集計は担当者の数のまま値だけ変わり、週報とファイルは増えない", async () => {
  test.setTimeout(180_000);
  expect(mainRun, "前の確かめで 1 回目が流れた").not.toBe("");
  const wk = WEEK.main;
  const app = W.apps.daily;
  const reportBefore = await reportRow(wk.start);
  const filesBefore = await reportFiles(wk.start);
  // bob が水曜の日報を遅れて出す（同じ入口・本人の権限）。
  const late = await pressApiDone(P.bob.page, app.id, {
    version: app.version,
    actionId: app.actions["提出する"],
    params: { report_date: LATE.date, visits: LATE.visits, deals: LATE.deals, prospect: LATE.prospect, note: LATE.note },
  });
  expect(replyText(late)).toBe(`${LATE.date} の日報を登録しました`);

  const d = await runAtDone(alice(), W.workflows.weekly.id, wk.at);
  expect(d.status, JSON.stringify(d.failure)).toBe("succeeded");
  // 2 回目の保存は同じ名前でぶつかり、error の出口からファイルなしで週報を書き直す（run は成功）。
  expect(runs.takenPorts(d, "save_file")).toEqual(["error"]);
  expect(runs.errorCode(d, "save_file")).toBe("name_taken");
  expect(runs.takenPorts(d, "save_report_nofile")).toEqual(["out"]);
  expect(runs.stepStatus(d, "save_report")).toBe("skipped");

  const want = totalsOf([...REPORTS, { who: "bob", ...LATE }], wk.start, wk.end);
  expect(want).toEqual({ bob: { visits: 12, deals: 5, prospect: 430000, days: 5 } });
  await expectTotals(wk.start, want);
  const reportAfter = await reportRow(wk.start);
  expect(reportAfter.id).toBe(reportBefore.id);
  expect(reportAfter.data.report_file, "前に保存したファイルのまま").toBe(reportBefore.data.report_file);
  expect((await reportFiles(wk.start)).map((f) => f.id)).toEqual(filesBefore.map((f) => f.id));
});

test("(2) K を既定（5）にした写しでは、1 週に 5 件未満の担当者の合計は伏せられて null（0 ではない）になる", async () => {
  test.setTimeout(180_000);
  const wf = W.workflows.weekly_k;
  const wk = WEEK.k;
  const d = await runAtDone(alice(), wf.id, wk.at);
  expect(d.status, JSON.stringify(d.failure)).toBe("succeeded");
  const totals = await runs.stepOutput<{ suppressed: boolean; groups: { key: Json; value: unknown; values: unknown[] }[] }>(
    alice(),
    wf.id,
    d.run_id,
    "totals",
  );
  expect(totals.suppressed).toBe(true);
  expect(totals.groups.length).toBe(1);
  expect(totals.groups[0].values).toEqual([null, null, null, null]);
  expect(Object.keys(totalsOf(REPORTS_K, wk.start, wk.end))).toEqual(["bob"]);
  const got = await totalsRows(wk.start);
  expect(got.length).toBe(1);
  for (const f of ["visits", "deals", "prospect", "days"]) {
    expect(got[0].data[f] ?? null, `${f} は伏せて null（0 ではない）`).toBeNull();
  }
  const sheet = (await reportFiles(wk.start))[0];
  const md = await files.text(alice(), sheet.id);
  expect(section(md, "集計")).toContain(`| ${NAMES.bob} | — | — | — | — |`);
  expect(section(md, "集計")).toContain("個人が分からないよう伏せています");
});

test("（追加）要約の LLM が失敗・打ち切りでも、週次集計は書かれていて、理由が step の code と週報に出る", async () => {
  test.setTimeout(300_000);
  const wk = WEEK.llm;
  const want = totalsOf(REPORTS, wk.start, wk.end);
  const codes: string[] = [];
  for (const [ref, v] of Object.entries(LLM_VARIANTS)) {
    const wf = W.workflows[ref];
    const d = await runAtDone(alice(), wf.id, wk.at);
    expect(d.status, `${ref}: ${JSON.stringify(d.failure)}`).toBe("succeeded");
    expect(runs.takenPorts(d, "summarize"), ref).toEqual(["error"]);
    const code = runs.errorCode(d, "summarize");
    if (v.code) expect(code, ref).toBe(v.code);
    else expect(code, `${ref} の code`).toBeTruthy();
    codes.push(`${ref}=${code}`);
    // 週次集計は LLM より前に書いている。
    await expectTotals(wk.start, want);
    const body = await runs.stepOutput<{ state: string; summary: string }>(alice(), wf.id, d.run_id, "body");
    expect(body.state).toBe("failed");
    expect((await reportRow(wk.start)).data.summary).toBe(`（AI のまとめを作れませんでした。理由: ${WHY[code ?? ""] ?? WHY_OTHER}）`);
  }
  test.info().annotations.push({ type: "要約の失敗の code", description: codes.join(" / ") });

  // 形の指定の無い打ち切り（truncate:）は成功のまま stop_reason=max_tokens で返り、本文に途中までと書く。
  const cut = W.workflows.weekly_cut;
  const d = await runAtDone(alice(), cut.id, wk.at);
  expect(d.status, JSON.stringify(d.failure)).toBe("succeeded");
  expect(runs.takenPorts(d, "summarize")).toEqual(["out"]);
  const out = await runs.stepOutput<{ stop_reason: string; text: string }>(alice(), cut.id, d.run_id, "summarize");
  expect(out.stop_reason).toBe("max_tokens");
  const body = await runs.stepOutput<{ state: string; summary: string }>(alice(), cut.id, d.run_id, "body");
  expect(body.state).toBe("cut");
  expect((await reportRow(wk.start)).data.summary).toContain("（AI のまとめは長さの上限で途中までです）");
  await expectTotals(wk.start, want);
  // 週報のファイルは最初の 1 つだけ（2 回目からは同じ名前でぶつかる）。
  expect((await reportFiles(wk.start)).length).toBe(1);
});

/// 画面から日報を出す（数は「増やす」のボタンで入れる）。
async function submitInApp(page: Page, appId: string, v: { date: string; visits: number; deals: number; prospect: number; note: string }) {
  await page.goto(`/apps/${appId}`);
  const figure = page.getByTestId("ui-figure").first();
  await expect(figure.getByRole("button", { name: "提出する", exact: true })).toBeEnabled({ timeout: 30_000 });
  await figure.getByLabel("日付").fill(v.date);
  for (let i = 0; i < v.visits; i++) await figure.getByRole("button", { name: "訪問件数を増やす" }).click();
  for (let i = 0; i < v.deals; i++) await figure.getByRole("button", { name: "商談件数を増やす" }).click();
  await figure.getByLabel("見込み額").fill(String(v.prospect));
  await figure.getByLabel("所感").fill(v.note);
  return pressInApp(page, appId, { button: "提出する", open: false });
}

test("（追加）同じ日付の出し直しは 1 行のまま上書きし、同じ冪等キーの送り直しは run を増やさず、担当者は名乗れない", async ({ browser }) => {
  test.setTimeout(240_000);
  const app = W.apps.daily;
  const bob = P.bob.page;
  const day = "2026-10-20";
  const first = await submitInApp(bob, app.id, { date: day, visits: 2, deals: 1, prospect: 40000, note: "午前に 2 件" });
  expect(first.actionId).toBe(app.actions["提出する"]);
  expect(first.params).toEqual({ report_date: day, visits: 2, deals: 1, prospect: 40000, note: "午前に 2 件" });
  expect(first.run.status).toBe("succeeded");
  expect(replyTones(first.run)).toEqual(["success"]);
  expect(replyText(first.run)).toBe(`${day} の日報を登録しました`);
  const key = `${P.bob.id}-${day}`;
  const row1 = (await rows.query(alice(), W.tables.daily.id, cmp("entry_key", "eq", key)))[0];
  expect(row1?.data).toMatchObject({ rep: P.bob.id, report_date: day, visits: 2, deals: 1, prospect: 40000 });
  await shot(bob, "app03", "bob-submit-pc");
  const phone = await phoneOf(browser, P.bob);
  try {
    await phone.goto(`/apps/${app.id}`);
    await expect(phone.getByTestId("ui-figure").getByRole("button", { name: "提出する", exact: true })).toBeVisible({ timeout: 30_000 });
    await expect(phone.getByTestId("miniapp-data-grid-自分の日報")).toBeVisible({ timeout: 30_000 });
    await noSideScroll(phone);
    await shot(phone, "app03", "bob-phone");
  } finally {
    await phone.context().close();
  }

  // 同じ日付をもう一度（数を直して）出す → 1 行のまま上書きし、「上書き」と返す。
  const opts = { version: app.version, actionId: app.actions["提出する"], params: { report_date: day, visits: 5, deals: 2, prospect: 90000, note: "午後に 3 件追加" } };
  const second = await pressApi(bob, app.id, opts);
  expect(second.status, JSON.stringify(second.body)).toBe(200);
  // 同じ冪等キーの送り直しは、前の起動を返す（run は増えない）。
  const resend = await pressApi(bob, app.id, { ...opts, key: second.key });
  expect(resend.status).toBe(200);
  expect(resend.runId).toBe(second.runId);
  const r2 = await pressApiDone(bob, app.id, { ...opts, key: second.key });
  expect(r2.run_id).toBe(second.runId);
  expect(replyText(r2)).toBe(`${day} の日報を上書きしました`);
  const same = await rows.query(alice(), W.tables.daily.id, cmp("entry_key", "eq", key));
  expect(same.length, "同じ人・同じ日の日報は 1 行").toBe(1);
  expect(same[0].id).toBe(row1.id);
  expect(same[0].data).toMatchObject({ visits: 5, deals: 2, prospect: 90000, note: "午後に 3 件追加" });
  const runList = await call<{ runs: { run_id?: string }[] }>(bob, "GET", `/mini-apps/${app.id}/runs`);
  expect(runList.body.runs.filter((r) => r.run_id === second.runId).length).toBe(1);

  // 別の日付なら別の行。
  const other = "2026-10-21";
  await pressApiDone(bob, app.id, { ...opts, params: { ...opts.params, report_date: other } });
  const bobs = await rows.query(alice(), W.tables.daily.id, all(cmp("rep", "eq", P.bob.id), cmp("report_date", "gte", day), cmp("report_date", "lte", other)));
  expect(bobs.map((r) => r.data.report_date).sort()).toEqual([day, other]);

  // 担当者を値に混ぜても、担当者は押した本人（rep は入力から取らない）。
  const forged = "2026-10-22";
  const sneak = await pressApi(bob, app.id, { ...opts, params: { ...opts.params, report_date: forged, rep: P.alice.id } });
  test.info().annotations.push({ type: "担当者を混ぜた送信", description: `${sneak.status} ${JSON.stringify(sneak.body).slice(0, 200)}` });
  if (sneak.status === 200) {
    const done = await pressApiDone(bob, app.id, { ...opts, params: { ...opts.params, report_date: forged, rep: P.alice.id }, key: sneak.key });
    expect(done.status).toBe("succeeded");
  } else {
    expect([400, 422]).toContain(sneak.status);
  }
  const onForged = await rows.query(alice(), W.tables.daily.id, cmp("report_date", "eq", forged));
  for (const r of onForged) expect(r.data.rep, "担当者は bob のまま").toBe(P.bob.id);
  expect(onForged.some((r) => r.data.rep === P.alice.id)).toBe(false);
});

test("(1)(c)(4) 本物のスケジュール（毎分の写し・委譲）が 2 回動いても、alice の見え方で集計した行とファイルは週に 1 つずつ", async () => {
  test.setTimeout(420_000 + CRON_WEEK_MARGIN_MS);
  const mainFiles = await reportFiles(WEEK.main.start);
  const mainTotals = await totalsRows(WEEK.main.start);
  // 毎分の cron が数える「先週」に日報を置く（置いてから run までの間に週が替わらないよう、替わり目の手前なら待つ）。
  await awayFromJstWeekBoundary();
  const today = jstToday();
  const cronFixture: Report[] = cronReports(today);
  for (const row of cronReportRows(today)) {
    await ok(alice(), "POST", `/data/tables/${W.tables.daily.id}/records`, { data: W.fill(row) }, [201]);
  }
  await withMinuteCron(
    alice(),
    W.workflows.weekly,
    async ({ workflowId, run, detail }) => {
      expect(run.trigger_kind).toBe("schedule");
      expect(detail.status, JSON.stringify(detail.failure)).toBe("succeeded");
      const cronRuns = await runs.list(alice(), workflowId, { trigger_kind: "schedule" });
      expect(cronRuns.length).toBeGreaterThanOrEqual(2);
      const ends: RunDetail[] = [];
      for (const r of cronRuns) ends.push(await runs.waitEnd(alice(), workflowId, r.run_id, 120_000));
      for (const e of ends) expect(e.status, JSON.stringify(e.failure)).toBe("succeeded");
      const lw = await runs.stepOutput<{ start: string; end: string }>(alice(), workflowId, run.run_id, "last_week");
      for (const e of ends) {
        const w = await runs.stepOutput<{ start: string }>(alice(), workflowId, e.run_id, "last_week");
        test.info().annotations.push({ type: "cron の週", description: `${e.run_id}: ${w.start}` });
        expect(w.start, "cron の run の先週は、日報を置いた週").toBe(lastWeekStart(today));
      }
      // 期待値: この週に置いた日報（alice と bob）の手計算。alice（部長）の見え方なので、alice の分も入る。
      const want = totalsOf(cronFixture, lw.start, lw.end);
      expect(Object.keys(want).sort(), "置いた日報が cron の週にある（0 行 = 0 行で通る確かめにしない）").toEqual(["alice", "bob"]);
      const got = await expectTotals(lw.start, want);
      // 自動実行が作った行の作成者（GAP-58）: いまの動きは委譲元（有効化した alice）。docs はワークフローと書いている。
      for (const r of got) expect(r.created_by ?? (r as unknown as { owner?: string }).owner, `週次集計 ${String(r.data.key)} の作成者`).toBe(P.alice.id);
      const rep = await reportRow(lw.start);
      const missing = [NAMES.alice, NAMES.bob].filter((_, i) => !(["alice", "bob"][i] in want));
      expect(rep.data.missing).toBe(missing.length ? missing.join("、") : "なし");
      const sheets = await reportFiles(lw.start);
      expect(sheets.map((s) => s.name), "同じ週を 2 回処理しても週報のファイルは 1 つ").toEqual([`週報-${lw.start}.md`]);
      expect(rep.data.report_file).toBe(sheets[0].id);
      const md = await files.text(alice(), sheets[0].id);
      for (const [who, t] of Object.entries(want)) expect(section(md, "集計")).toContain(tableLine(NAMES[who as "alice" | "bob"], t));
    },
    { minRuns: 2, timeout: 200_000 },
  );
  // 予定の日時で処理した週は、本物の cron の後も増えない。
  expect((await reportFiles(WEEK.main.start)).map((f) => f.id)).toEqual(mainFiles.map((f) => f.id));
  expect((await totalsRows(WEEK.main.start)).map((r) => [r.id, r.rev])).toEqual(mainTotals.map((r) => [r.id, r.rev]));
});

test("権限: bob は自分の日報だけを見て、週次集計・週報・フォルダー・部長のアプリを読めない。charlie はどれも読めない", async () => {
  test.setTimeout(120_000);
  const app = W.apps.daily;
  const mine = await gridRows(P.bob.page, app.id, app.version, "自分の日報");
  expect(mine.status).toBe(200);
  expect(mine.rows.length).toBeGreaterThan(0);
  expectOnlyOwn(mine.rows, "rep", P.bob.id, "bob の「自分の日報」");
  const everyone = await gridRows(alice(), app.id, app.version, "自分の日報");
  expect(everyone.status).toBe(200);
  expect(new Set(everyone.rows.map((r) => r.data.rep))).toEqual(new Set([P.alice.id, P.bob.id]));
  // bob が日報の表を直接読んでも、行の決まりで自分の分だけ。
  const direct = await rows.try(P.bob.page, W.tables.daily.id);
  expect(direct.status).toBe(200);
  expectOnlyOwn(direct.items, "rep", P.bob.id, "bob が読んだ日報");
  // bob は alice の日報を書けない（行の決まりの書き込み側）。
  const aliceRow = (await rows.query(alice(), W.tables.daily.id, cmp("rep", "eq", P.alice.id)))[0];
  const write = await call(P.bob.page, "PATCH", `/data/tables/${W.tables.daily.id}/records/${aliceRow.id}`, { patch: { visits: 0 }, expected_rev: aliceRow.rev });
  expect([403, 404], `bob が alice の日報を書き換える → ${write.status}`).toContain(write.status);

  // bob は週次集計・週報の表・週報のフォルダー・部長のアプリ・週報のワークフローを読めない（存在しない id と同じ答え）。
  const bucho = W.apps.bucho;
  await expectDenied(P.bob.page, [
    { method: "POST", path: `/data/tables/${W.tables.totals.id}/query`, data: { limit: 1 }, what: "週次集計" },
    { method: "POST", path: `/data/tables/${W.tables.reports.id}/query`, data: { limit: 1 }, what: "週報の表" },
    { path: `/nodes?parent_id=${W.folders.weekly.id}`, what: "週報のフォルダー" },
    { path: `/mini-apps/${bucho.id}/resolved`, what: "部長のアプリ" },
    { path: `/workflows/${W.workflows.weekly.id}`, what: "週報のワークフロー" },
    { path: `/workflows/${W.workflows.weekly.id}/runs/${mainRun}`, what: "週報の run" },
  ]);
  const sheet = (await reportFiles(WEEK.main.start))[0];
  await expectDenied(P.bob.page, [{ path: `/files/${sheet.id}/text`, what: "週報のファイル" }]);
  await expectStatus(P.bob.page, [{ method: "POST", path: `/mini-apps/${bucho.id}/ui-actions`, data: { version: bucho.version, action_id: "data_grid:週次集計", params: {} }, what: "部長のアプリの表" }], [403, 404]);

  // charlie（別の組織）は、表・ビュー・ワークフロー・run・アプリ・押す・表を読む・フォルダー・ファイルのどれも読めない。
  await expectDenied(P.charlie.page, [{ path: `/files/${sheet.id}/text`, what: "週報のファイル" }]);
  await expectOutsiderSeesNothing(P.charlie.page, W, { workflowRuns: [["weekly", mainRun]] });
});
