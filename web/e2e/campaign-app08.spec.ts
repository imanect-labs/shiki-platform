import { expect, test, type Page } from "@playwright/test";

import {
  DEALS,
  HIDDEN,
  LATE_DEAL,
  LLM_VARIANTS,
  NORTH_RAW,
  WEEK,
  WHY,
  WHY_OTHER,
  app08World,
  cronDeals,
  kpiOf,
  type Deal,
  type Kpi,
} from "./campaign/apps/app08";
import { expectDenied, expectOnlyOwn, expectOutsiderSeesNothing, expectStatus } from "./campaign/checks";
import { call, ok, type Json } from "./campaign/http";
import { people, phoneOf, type People } from "./campaign/people";
import { gridRows, pressInApp, replyText, replyTones } from "./campaign/press";
import { noSideScroll, shot } from "./campaign/shots";
import { jstToday, runAtDone, withMinuteCron } from "./campaign/time";
import { cmp, files, rows, runs, type DataRecord, type NodeItem, type RunDetail } from "./campaign/truth";
import { buildWorld, cleanupWorld, type World } from "./campaign/world";

/// APP-08 週次 KPI ダッシュボード（台帳 §7・§7.13 の APP-08）を組んで、経営企画（alice）と営業（bob）として使い、
/// 成功判定をデータで確かめる。LLM は stub（形の指定の無いコメントは「回答: <prompt>」の echo）。長い待ちは毎分の cron の 1 つだけ。
/// (1) 数値が手集計と一致: fixture（東 受注 6・失注 2、西 受注 5・失注 1、北 受注 2・失注 1・範囲の外の日に大きな額）を
///     予定の日時の実行で流し、週次 KPI の東・西の受注額・受注件数・決着件数・受注率・前週比が fixture から計算した値と
///     一致し、境界の日が除かれる。won と closed の step 出力の groups も同じ値。
/// (2) 再実行しても snapshot が重複しない: 同じ予定の日時で 2 回（間に受注を 1 件足す）と retry（mode new）で、行は地域の
///     数のまま値だけ変わり（rows_rejected=0）、`KPI-<週の初め>*.md` は 1 つのまま。2 本を同時に始めても 2 行にならない。
/// (3) 少人数の地域が suppressed として正しく表示される: 北（K=5 未満）は won の step 出力で null・suppressed=true、
///     週次 KPI の数は null で状態は「件数が少ないため非表示」。bob の表でも同じで、.md と最新の週の reply に北の生の額が出ない。
/// （追加）月曜 7:00 に動く・毎分の写しの本物のスケジュール（委譲元 alice の見え方）・権限・LLM コメントの失敗。
/// （F6）集計のビューを表（data_grid）に束ねると、0 件と出さずに、表示できないことを伝える（PC とスマホ）。
/// 予定の日時の週は、本物の cron の週（今日の前の週）と重ならない過去の週にずらした（app08.ts の WEEK）。
/// `E2E_SHOTS_DIR` を渡すと、PC とスマホの画面を撮る。

test.describe.configure({ mode: "serial" });

let P: People;
let W: World;
/// 本物の cron の週に置いた商談（組んだ日の今日から決める）。
let cronFixture: Deal[] = [];

test.beforeAll(async ({ browser }) => {
  test.setTimeout(300_000);
  P = await people(browser);
  const today = jstToday();
  cronFixture = cronDeals(today);
  W = await buildWorld(P, app08World(today));
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

type Group = { key: Record<string, unknown>; value: unknown; values: unknown[] };
type QueryOut = { suppressed: boolean; groups: Group[] };
type BulkOut = { rows_total: number; rows_applied: number; rows_rejected: number; rows_invalid: number; rows_matched?: number; errors: unknown[] };

/// 週次 KPI の行（週の初めで絞る）。
async function kpiRows(weekStart: string): Promise<DataRecord[]> {
  return rows.query(alice(), W.tables.kpi.id, cmp("week_start", "eq", weekStart));
}

/// 行の数（null の項目は data に無い）。
const numOrNull = (v: unknown) => (v === undefined || v === null ? null : Number(v));

/// 週次 KPI の行が手集計と一致する（地域ごとに 1 行・キーは「週の初め|地域」）。
async function expectKpi(weekStart: string, want: Record<string, Kpi>): Promise<DataRecord[]> {
  const got = await kpiRows(weekStart);
  expect(got.map((r) => String(r.data.region)).sort(), `週次 KPI（${weekStart}）は地域ごとに 1 行`).toEqual(Object.keys(want).sort());
  for (const r of got) {
    const k = want[String(r.data.region)];
    const seen = {
      region: r.data.region,
      won_amount: numOrNull(r.data.won_amount),
      won_count: numOrNull(r.data.won_count),
      closed_count: numOrNull(r.data.closed_count),
      win_rate: numOrNull(r.data.win_rate),
      wow_pct: numOrNull(r.data.wow_pct),
      status: r.data.status ?? null,
    };
    expect(seen, `週次 KPI（${weekStart}）の ${String(r.data.region)}`).toEqual(k);
    expect(r.data.snapshot_key).toBe(`${weekStart}|${String(r.data.region)}`);
  }
  return got;
}

/// 地域 → 集計の行の値（数にそろえる・値の文字は数の文字）。
function groupsByRegion(out: QueryOut): Record<string, (number | null)[]> {
  return Object.fromEntries(out.groups.map((g) => [String(g.key.region), g.values.map(numOrNull)]));
}

/// 報告のファイル（`KPI-<週の初め>.md`・番号つきも含む）。
async function reportFiles(weekStart: string): Promise<NodeItem[]> {
  const re = new RegExp(`^KPI-${weekStart}( \\(\\d+\\))?\\.md$`);
  return (await files.list(alice(), W.folders.weekly.id)).filter((n) => n.kind === "file" && re.test(n.name));
}

/// 週次報告の行（週のキーで絞る・ちょうど 1 行）。
async function reportRow(weekStart: string): Promise<DataRecord> {
  const found = await rows.query(alice(), W.tables.reports.id, cmp("week_key", "eq", weekStart));
  expect(found.length, `週次報告（${weekStart}）はちょうど 1 行`).toBe(1);
  return found[0];
}

/// 北の生の額がどの書き方でも出ていない（3 桁区切り・区切りなし・合計）。
function expectNoNorthRaw(text: string, where: string) {
  const sum = NORTH_RAW.reduce((s, x) => s + x, 0);
  for (const n of [...NORTH_RAW, sum]) {
    expect(text, `${where}に北の生の額 ${n} が出ない`).not.toContain(String(n));
    expect(text, `${where}に北の生の額 ${n.toLocaleString("en-US")} が出ない`).not.toContain(n.toLocaleString("en-US"));
  }
}

/// IR の中で `$from` が指す先（ノードの id → 参照する場所の一覧 `<ノード>:<params の場所>`・APP-03 と同じ）。
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

test("（追加）週次 KPI のきっかけは毎週月曜 7 時（日本時間）で、次の発火は月曜の 07:00+09:00", async () => {
  const ir = (await ok<{ ir: { triggers: Json[] } }>(alice(), "GET", `/workflows/${W.workflows.weekly.id}`)).ir;
  expect(ir.triggers).toEqual([expect.objectContaining({ kind: "schedule", cron: "0 7 * * 1", tz: "Asia/Tokyo" })]);
  expect(W.workflows.weekly.enabled, "有効化は組み立てで同意計画と照らして済ませた（委譲は alice）").toBe(true);
  const preview = await ok<{ next: string[]; error?: string }>(alice(), "POST", "/workflows/schedule-preview", {
    cron: "0 7 * * 1",
    tz: "Asia/Tokyo",
    count: 3,
  });
  expect(preview.error).toBeUndefined();
  expect(preview.next.length).toBe(3);
  for (const at of preview.next) {
    // 日本時間の月曜 7:00 = UTC の日曜 22:00。
    expect(new Date(at).toLocaleString("sv-SE", { timeZone: "Asia/Tokyo" }), at).toMatch(/ 07:00:00$/);
    expect(new Date(new Date(at).getTime() + 9 * 3600 * 1000).getUTCDay(), `${at} は日本時間の月曜`).toBe(1);
  }
  expect(new Date(preview.next[1]).getTime() - new Date(preview.next[0]).getTime()).toBe(7 * 24 * 3600 * 1000);
});

let mainRun = "";

test("(1) 予定の日時（10-19 月 7:00）の実行で、東・西の KPI が手集計と一致し、境界の日が除かれる", async () => {
  test.setTimeout(180_000);
  const wf = W.workflows.weekly;
  const wk = WEEK.main;
  const d = await runAtDone(alice(), wf.id, wk.at);
  mainRun = d.run_id;
  expect(d.status, JSON.stringify(d.failure)).toBe("succeeded");
  const lastWeek = await runs.stepOutput<{ start: string; end: string }>(alice(), wf.id, d.run_id, "last_week");
  expect([lastWeek.start, lastWeek.end]).toEqual([wk.start, wk.end]);
  const prevWeek = await runs.stepOutput<{ start: string; end: string }>(alice(), wf.id, d.run_id, "prev_week");
  expect([prevWeek.start, prevWeek.end]).toEqual([wk.prevStart, wk.prevEnd]);

  // 手計算: 月曜 10-12 と日曜 10-18 は含み、前の日曜 10-11 と次の月曜 10-19 は除く。
  const want = kpiOf(DEALS, wk);
  expect(want.東).toEqual({ region: "東", won_amount: 2000000, won_count: 6, closed_count: 8, win_rate: 75, wow_pct: 25, status: null });
  expect(want.西).toEqual({ region: "西", won_amount: 1500000, won_count: 5, closed_count: 6, win_rate: 83.3, wow_pct: -25, status: null });
  expect(want.北.status).toBe(HIDDEN);
  expect(Object.keys(want).sort(), "決着の無い南は行にしない").toEqual(["北", "東", "西"]);
  const widened = kpiOf(DEALS, { ...wk, start: "2026-10-11", end: "2026-10-19" });
  expect(widened.東.won_amount, "境界の日を含めると東の受注額が変わる（含めないことの裏）").not.toBe(want.東.won_amount);
  await expectKpi(wk.start, want);

  // won と closed の step 出力の groups も同じ値（北は伏せて null・結果全体の suppressed=true）。
  const won = await runs.stepOutput<QueryOut>(alice(), wf.id, d.run_id, "won");
  expect(won.suppressed).toBe(true);
  expect(groupsByRegion(won)).toEqual({ 東: [2000000, 6], 西: [1500000, 5], 北: [null, null] });
  const closed = await runs.stepOutput<QueryOut>(alice(), wf.id, d.run_id, "closed");
  expect(groupsByRegion(closed)).toEqual({ 東: [8], 西: [6], 北: [null] });
  const prev = await runs.stepOutput<QueryOut>(alice(), wf.id, d.run_id, "won_prev");
  expect(groupsByRegion(prev)).toEqual({ 東: [1600000, 5], 西: [2000000, 6], 北: [null, null] });

  // 書き込みは 3 行を追加（拒否なし）。
  const save = await runs.stepOutput<BulkOut>(alice(), wf.id, d.run_id, "save");
  expect(save).toMatchObject({ rows_total: 3, rows_applied: 3, rows_rejected: 0, rows_invalid: 0, rows_matched: 0 });

  // 報告: 週次報告のフォルダーに KPI-10-12.md がちょうど 1 つ。本文は script が作った表と AI のコメント（stub の echo）。
  const made = await files.waitFor(alice(), W.folders.weekly.id, new RegExp(`^KPI-${wk.start}\\.md$`));
  expect((await reportFiles(wk.start)).map((f) => f.name)).toEqual([`KPI-${wk.start}.md`]);
  const md = await files.text(alice(), made[0].id);
  expect(md).toContain(`# 週次 KPI（${wk.start} 〜 ${wk.end}）`);
  expect(md).toContain("| 東 | 2,000,000 円 | 6 | 8 | 75% | 25% |");
  expect(md).toContain("| 西 | 1,500,000 円 | 5 | 6 | 83.3% | -25% |");
  expect(md).toContain("| 北 | — | — | — | — | — |");
  expect(md).toContain("件数が少ない地域（北）は、個人が分からないよう数を伏せています（—）。");
  expect(md).not.toContain("9,990,000");
  expect(md).toContain("## コメント（AI）\n\n回答: ");
  const rep = await reportRow(wk.start);
  expect(rep.data.report_file).toBe(made[0].id);
  expect(String(rep.data.comment)).toContain("回答: ");

  // LLM の出力が流れる先は、本文（.md）と週次報告のコメントの列だけ（KPI の数・ファイル名・保存先には流れない）。
  const ir = W.workflows.weekly.ir;
  expect(consumersOf(ir, "nodes.comment.output")).toEqual(["body:/input/err", "body:/input/stop", "body:/input/text"]);
  expect(consumersOf(ir, "nodes.body.output")).toEqual(["save_file:/content", "save_report:/data/comment", "save_report_nofile:/data/comment"]);
});

test("(2) 同じ予定の日時の 2 回目（間に受注を 1 件足す）と retry（mode new）で、行は地域の数のまま値だけ変わり、.md は 1 つのまま", async () => {
  test.setTimeout(240_000);
  expect(mainRun, "前の確かめで 1 回目が流れた").not.toBe("");
  const wf = W.workflows.weekly;
  const wk = WEEK.main;
  const before = await kpiRows(wk.start);
  const filesBefore = await reportFiles(wk.start);
  const reportBefore = await reportRow(wk.start);
  // 経営企画の alice が、西の受注を 1 件足す（本番の週の水曜）。
  await ok(alice(), "POST", `/data/tables/${W.tables.deals.id}/records`, { data: { ...LATE_DEAL, sales: P.bob.id } }, [201]);

  const d2 = await runAtDone(alice(), wf.id, wk.at);
  expect(d2.status, JSON.stringify(d2.failure)).toBe("succeeded");
  const save2 = await runs.stepOutput<BulkOut>(alice(), wf.id, d2.run_id, "save");
  expect(save2).toMatchObject({ rows_total: 3, rows_applied: 3, rows_rejected: 0, rows_invalid: 0, rows_matched: 3 });
  // 2 回目の保存は同じ名前でぶつかり、error の出口からファイルなしで週次報告を書き直す（run は成功）。
  expect(runs.takenPorts(d2, "save_file")).toEqual(["error"]);
  expect(runs.errorCode(d2, "save_file")).toBe("name_taken");
  expect(runs.takenPorts(d2, "save_report_nofile")).toEqual(["out"]);

  const want = kpiOf([...DEALS, LATE_DEAL], wk);
  expect(want.西).toEqual({ region: "西", won_amount: 1700000, won_count: 6, closed_count: 7, win_rate: 85.7, wow_pct: -15, status: null });
  const after = await expectKpi(wk.start, want);
  expect(after.map((r) => r.id).sort(), "同じ行を書き換えた（行は増えない）").toEqual(before.map((r) => r.id).sort());

  // retry（mode new）: 同じ予定の日時を引き継いだ新しい run。行も .md も増えない。
  const retried = await ok<{ run_id: string; mode: string }>(alice(), "POST", `/workflows/${wf.id}/runs/${d2.run_id}/retry`, { mode: "new" }, [202]);
  expect(retried.mode).toBe("new");
  const d3 = await runs.waitEnd(alice(), wf.id, retried.run_id);
  expect(d3.status, JSON.stringify(d3.failure)).toBe("succeeded");
  const detail3 = await ok<{ scheduled_at: string | null; scheduled_at_source: string | null }>(alice(), "GET", `/workflows/${wf.id}/runs/${d3.run_id}`);
  expect(detail3.scheduled_at_source).toBe("rerun");
  const lw3 = await runs.stepOutput<{ start: string }>(alice(), wf.id, d3.run_id, "last_week");
  expect(lw3.start, "retry は元の予定の日時の週を数える").toBe(wk.start);
  const save3 = await runs.stepOutput<BulkOut>(alice(), wf.id, d3.run_id, "save");
  expect(save3).toMatchObject({ rows_total: 3, rows_applied: 3, rows_rejected: 0, rows_matched: 3 });
  const again = await expectKpi(wk.start, want);
  expect(again.map((r) => r.id).sort()).toEqual(before.map((r) => r.id).sort());
  expect((await reportFiles(wk.start)).map((f) => f.id), `KPI-${wk.start}*.md は 1 つのまま`).toEqual(filesBefore.map((f) => f.id));
  const reportAfter = await reportRow(wk.start);
  expect(reportAfter.id).toBe(reportBefore.id);
  expect(reportAfter.data.report_file, "前に保存したファイルのまま").toBe(reportBefore.data.report_file);
});

let bobLatestRun = "";

test("(3) 北は伏せられ、週次 KPI・bob の表・.md・最新の週の reply のどこにも北の生の額が出ない", async ({ browser }) => {
  test.setTimeout(240_000);
  const wk = WEEK.main;
  const app = W.apps.main;
  const bob = P.bob.page;
  // won の step 出力: 北は値が null、結果全体の suppressed=true（前の確かめの 1 回目の run）。
  const won = await runs.stepOutput<QueryOut>(alice(), W.workflows.weekly.id, mainRun, "won");
  expect(won.suppressed).toBe(true);
  expect(groupsByRegion(won).北).toEqual([null, null]);
  // 週次 KPI の北: 数は null（0 や数え直した値ではない）・状態は「件数が少ないため非表示」。
  const north = (await kpiRows(wk.start)).find((r) => r.data.region === "北");
  expect(north, "北の行がある").toBeDefined();
  for (const f of ["won_amount", "won_count", "closed_count", "win_rate", "wow_pct"]) {
    expect(north!.data[f] ?? null, `北の ${f} は null`).toBeNull();
  }
  expect(north!.data.status).toBe(HIDDEN);

  // bob の表（週次 KPI のビュー・bob は表の viewer）でも同じ表示。
  const grid = await gridRows(bob, app.id, app.version, "週次 KPI");
  expect(grid.status).toBe(200);
  const mine = grid.rows.filter((r) => r.data.week_start === wk.start);
  const byRegion = Object.fromEntries(mine.map((r) => [String(r.data.region), r.data]));
  expect(byRegion.北?.status).toBe(HIDDEN);
  expect(byRegion.北?.won_amount ?? null).toBeNull();
  expect(numOrNull(byRegion.東?.won_amount)).toBe(2000000);
  expectNoNorthRaw(JSON.stringify(grid.rows), "bob の表");

  // .md の本文に北の生の額が出ない。
  const sheet = (await reportFiles(wk.start))[0];
  expectNoNorthRaw(await files.text(alice(), sheet.id), "報告の .md");

  // bob が画面から「最新の週を見る」を押す: 最新の週（10-12）の東・西の受注額と前週比の表と、北は非表示の注記。
  const pressed = await pressInApp(bob, app.id, { button: "最新の週を見る" });
  bobLatestRun = pressed.runId;
  expect(pressed.run.status).toBe("succeeded");
  expect(replyTones(pressed.run)).toEqual(["neutral"]);
  const md = replyText(pressed.run);
  expect(md).toContain(`**${wk.start} の週の地域別の受注額**`);
  expect(md).toContain("| 東 | 2,000,000 円 | 25% |");
  expect(md).toContain("| 西 | 1,700,000 円 | -15% |");
  expect(md).toContain("北 は件数が少ないため非表示です。");
  expect(md).not.toMatch(/^\| 北 \|/m);
  expectNoNorthRaw(md, "最新の週の reply");
  const row = bob.getByTestId("yui-runs").locator(`li[data-run-id="${pressed.runId}"]`);
  await expect(row.getByTestId("yui-run-result")).toContainText("北 は件数が少ないため非表示です。");
  await expect(row.getByTestId("yui-run-result").locator("table")).toContainText("2,000,000 円");
  // 表は canvas に描く（文字は DOM に無い）ので、行の中身は上の gridRows で裏取りし、画面では件数と目視（撮影）で見る。
  await expect(bob.getByTestId("miniapp-data-grid-週次 KPI").locator("figcaption")).toContainText(`${grid.rows.length} 件`, { timeout: 30_000 });
  await shot(bob, "app08", "bob-kpi-pc");

  const phone = await phoneOf(browser, P.bob);
  try {
    await phone.goto(`/apps/${app.id}`);
    await expect(phone.getByTestId("ui-figure").getByRole("button", { name: "最新の週を見る", exact: true })).toBeVisible({ timeout: 30_000 });
    await expect(phone.getByTestId("miniapp-data-grid-週次 KPI").locator("figcaption")).toContainText(`${grid.rows.length} 件`, { timeout: 30_000 });
    const prow = phone.getByTestId("yui-runs").locator(`li[data-run-id="${pressed.runId}"]`);
    if ((await prow.getByTestId("yui-run-result").count()) === 0) await prow.getByRole("button", { name: "結果" }).click();
    await expect(prow.getByTestId("yui-run-result")).toContainText("北 は件数が少ないため非表示です。", { timeout: 15_000 });
    await noSideScroll(phone);
    await shot(phone, "app08", "bob-kpi-phone");
  } finally {
    await phone.context().close();
  }
});

test("(2) 2 本を同時に始めても、同じ週の KPI は地域の数のまま（並行の bulk_upsert の match_on）で、.md も 1 つ", async () => {
  test.setTimeout(240_000);
  const wf = W.workflows.weekly;
  const wk = WEEK.race;
  expect(await kpiRows(wk.start), "この週はまだ書いていない").toEqual([]);
  const [a, b] = await Promise.all([runAtDone(alice(), wf.id, wk.at), runAtDone(alice(), wf.id, wk.at)]);
  const notes: string[] = [];
  for (const x of [a, b]) {
    expect(x.status, JSON.stringify(x.failure)).toBe("succeeded");
    const s = await runs.stepOutput<BulkOut>(alice(), wf.id, x.run_id, "save");
    notes.push(
      `${x.run_id}: applied=${s.rows_applied} matched=${s.rows_matched} invalid=${s.rows_invalid} rejected=${s.rows_rejected} errors=${JSON.stringify(s.errors)}`,
    );
    // どちらの run も全行を書く（後から追加しようとして断られた行は、先に追加された行の更新として取り込み直す・
    // 黙って書かずに成功で終わらない）。
    expect(s, "save は全行を書いた").toMatchObject({ rows_total: 3, rows_applied: 3, rows_invalid: 0, rows_rejected: 0, errors: [] });
  }
  test.info().annotations.push({ type: "同時の 2 本の書き込み", description: notes.join(" / ") });
  const want = kpiOf(DEALS, wk);
  expect(want.東).toEqual({ region: "東", won_amount: 1600000, won_count: 5, closed_count: 5, win_rate: 100, wow_pct: null, status: null });
  expect(want.西).toEqual({ region: "西", won_amount: 2000000, won_count: 6, closed_count: 7, win_rate: 85.7, wow_pct: null, status: null });
  await expectKpi(wk.start, want);
  expect((await reportFiles(wk.start)).length, `KPI-${wk.start}*.md は 1 つ`).toBe(1);
  await reportRow(wk.start);
});

test("（追加）コメントの LLM が失敗しても、週次 KPI は書かれていて、理由が step の code と報告に出る", async () => {
  test.setTimeout(300_000);
  const wk = WEEK.race;
  const want = kpiOf(DEALS, wk);
  const codes: string[] = [];
  for (const [ref, v] of Object.entries(LLM_VARIANTS)) {
    const wf = W.workflows[ref];
    const d = await runAtDone(alice(), wf.id, wk.at);
    expect(d.status, `${ref}: ${JSON.stringify(d.failure)}`).toBe("succeeded");
    expect(runs.takenPorts(d, "comment"), ref).toEqual(["error"]);
    const code = runs.errorCode(d, "comment");
    expect(code, ref).toBe(v.code);
    codes.push(`${ref}=${code}`);
    // 週次 KPI は LLM より前に書いている（書き込みは成功・拒否なし）。
    expect(runs.stepStatus(d, "save"), `${ref} の save`).toBe("succeeded");
    const save = await runs.stepOutput<BulkOut>(alice(), wf.id, d.run_id, "save");
    expect(save).toMatchObject({ rows_total: 3, rows_applied: 3, rows_rejected: 0 });
    await expectKpi(wk.start, want);
    const body = await runs.stepOutput<{ state: string; comment: string }>(alice(), wf.id, d.run_id, "body");
    expect(body.state).toBe("failed");
    expect((await reportRow(wk.start)).data.comment).toBe(`（AI のコメントを作れませんでした。理由: ${WHY[code ?? ""] ?? WHY_OTHER}）`);
  }
  test.info().annotations.push({ type: "コメントの失敗の code", description: codes.join(" / ") });
  expect((await reportFiles(wk.start)).length, "報告のファイルは最初の 1 つだけ").toBe(1);
});

test("（F6）集計のビューを表に束ねると、0 件と出さずに、ここでは表示できないと伝える（PC とスマホ）", async ({ browser }) => {
  test.setTimeout(180_000);
  const app = W.apps.agg;
  const alias = "地域別の受注額";
  // 裏取り: 表の道は集計の結果（groups）を返し、行（items）を返さない。
  const r = await call<{ result?: { result?: { result?: { groups?: unknown[]; items?: unknown[] } } } }>(alice(), "POST", `/mini-apps/${app.id}/ui-actions`, {
    version: app.version,
    action_id: `data_grid:${alias}`,
    params: {},
  });
  expect(r.status, r.text.slice(0, 300)).toBe(200);
  const result = r.body.result?.result?.result;
  expect(Array.isArray(result?.groups), "集計の結果が返る").toBe(true);
  expect(result?.items ?? null, "行は返らない").toBeNull();

  const check = async (page: Page, name: string) => {
    await page.goto(`/apps/${app.id}`);
    const grid = page.getByTestId(`miniapp-data-grid-${alias}`);
    const note = grid.getByTestId("miniapp-data-grid-aggregate");
    await expect(note).toBeVisible({ timeout: 30_000 });
    await expect(note).toContainText("集計のビューは、ここでは表示できません");
    await expect(note).toContainText("アプリを作った人が集計しないビューに替えると、表示できます。");
    await expect(grid.getByTestId("miniapp-data-grid-empty")).toHaveCount(0);
    await expect(grid.getByTestId("miniapp-data-grid-scroll-hint")).toHaveCount(0);
    await expect(grid.locator("figcaption")).not.toContainText("0 件");
    await shot(page, "app08", name);
  };
  await check(alice(), "aggregate-view-pc");
  const phone = await phoneOf(browser, P.bob);
  try {
    await check(phone, "aggregate-view-phone");
    await noSideScroll(phone);
  } finally {
    await phone.context().close();
  }
});

test("（追加）本物のスケジュール（毎分の写し・委譲）が、委譲元 alice の見え方（bob の商談も含む）で KPI を書く", async () => {
  test.setTimeout(300_000);
  const mainBefore = (await kpiRows(WEEK.main.start)).map((r) => [r.id, r.rev]);
  await withMinuteCron(alice(), W.workflows.weekly, async ({ workflowId, run, detail }) => {
    expect(run.trigger_kind).toBe("schedule");
    expect(detail.status, JSON.stringify(detail.failure)).toBe("succeeded");
    const lw = await runs.stepOutput<{ start: string; end: string }>(alice(), workflowId, run.run_id, "last_week");
    const pw = await runs.stepOutput<{ start: string; end: string }>(alice(), workflowId, run.run_id, "prev_week");
    const ends: RunDetail[] = [];
    for (const r of await runs.list(alice(), workflowId, { trigger_kind: "schedule" })) ends.push(await runs.waitEnd(alice(), workflowId, r.run_id, 120_000));
    for (const e of ends) expect(e.status, JSON.stringify(e.failure)).toBe("succeeded");
    // 期待値: この週に置いた商談の手計算。東の受注 5 件のうち 2 件は bob の担当で、委譲元 alice の見え方なので数に入る
    // （入らなければ 3 件で伏せられる）。西は 2 件で伏せる。
    const want = kpiOf([...DEALS, LATE_DEAL, ...cronFixture], { start: lw.start, end: lw.end, prevStart: pw.start, prevEnd: pw.end });
    expect(want.東).toEqual({ region: "東", won_amount: 1150000, won_count: 5, closed_count: 6, win_rate: 83.3, wow_pct: 130, status: null });
    expect(want.西.status).toBe(HIDDEN);
    const got = await expectKpi(lw.start, want);
    // 自動実行が作った行の作成者（GAP-58）: いまの動きは委譲元（有効化した alice）。docs はワークフローと書いている。
    for (const r of got) expect(r.created_by ?? (r as unknown as { owner?: string }).owner, `週次 KPI ${String(r.data.snapshot_key)} の作成者`).toBe(P.alice.id);
    const sheets = await files.waitFor(alice(), W.folders.weekly.id, new RegExp(`^KPI-${lw.start}\\.md$`));
    expect(sheets.length).toBe(1);
    expect(await files.text(alice(), sheets[0].id)).toContain("| 東 | 1,150,000 円 | 5 | 6 | 83.3% | 130% |");
  });
  // 予定の日時で処理した週は、本物の cron の後も変わらない。
  expect((await kpiRows(WEEK.main.start)).map((r) => [r.id, r.rev])).toEqual(mainBefore);
});

test("権限: bob は KPI を見られ、商談は自分の分だけ、KPI のフローは動かせない。charlie はどれも読めない", async () => {
  test.setTimeout(120_000);
  const app = W.apps.main;
  const bob = P.bob.page;
  const grid = await gridRows(bob, app.id, app.version, "週次 KPI");
  expect(grid.status).toBe(200);
  expect(grid.rows.length).toBeGreaterThan(0);
  // bob が商談の表を直接読んでも、行の決まりで自分の担当の分だけ。
  const direct = await rows.try(bob, W.tables.deals.id);
  expect(direct.status).toBe(200);
  expect(direct.items.length).toBeGreaterThan(0);
  expectOnlyOwn(direct.items, "sales", P.bob.id, "bob が読んだ商談");
  const aliceDeal = (await rows.query(alice(), W.tables.deals.id, cmp("sales", "eq", P.alice.id)))[0];
  // bob は alice の商談・週次報告・報告のフォルダーとファイル・KPI のフローとその run を読めず、動かせない（存在しない id
  // と同じ答え）。
  const sheet = (await reportFiles(WEEK.main.start))[0];
  await expectDenied(bob, [
    { path: `/data/tables/${W.tables.deals.id}/records/${aliceDeal.id}`, what: "alice の商談" },
    { method: "POST", path: `/data/tables/${W.tables.reports.id}/query`, data: { limit: 1 }, what: "週次報告" },
    { path: `/nodes?parent_id=${W.folders.weekly.id}`, what: "週次報告のフォルダー" },
    { path: `/files/${sheet.id}/text`, what: "報告の .md" },
    { path: `/workflows/${W.workflows.weekly.id}`, what: "週次 KPI のフロー" },
    { path: `/workflows/${W.workflows.weekly.id}/runs/${mainRun}`, what: "週次 KPI の run" },
    { method: "POST", path: `/workflows/${W.workflows.weekly.id}/runs`, data: { input: {}, scheduled_at: WEEK.main.at }, what: "週次 KPI を動かす" },
    { method: "POST", path: `/workflows/${W.workflows.weekly.id}/runs/${mainRun}/retry`, data: { mode: "new" }, what: "週次 KPI をやり直す" },
  ]);
  // alice（持ち主）も、bob が押した run をミニアプリの入口からは読めない（本人の記録だけ）。
  expect(bobLatestRun, "(3) で bob が最新の週を見た").not.toBe("");
  await expectStatus(alice(), [{ path: `/mini-apps/${app.id}/runs/${bobLatestRun}`, what: "bob の最新の週の run" }]);

  // charlie（別の組織）は、表・ビュー・ワークフロー・run・アプリ・押す・表を読む・フォルダーのどれも読めない。
  await expectDenied(P.charlie.page, [{ path: `/files/${sheet.id}/text`, what: "報告の .md" }]);
  await expectOutsiderSeesNothing(P.charlie.page, W, { workflowRuns: [["weekly", mainRun]], appRuns: [["main", bobLatestRun]] });
});
