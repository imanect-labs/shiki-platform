import { createHash } from "node:crypto";

import { expect, test, type Page } from "@playwright/test";

import {
  BATCH,
  EDITED_COMMENT,
  EDITED_ID,
  FAULT_DRIVE,
  FOREIGN_ID,
  INJECTED_ID,
  INJECTION,
  REVIEW,
  SEGMENTS,
  app10World,
  editedRows,
  npsOf,
  ruleSentiment,
  ruleTopics,
  shiftJis,
  surveyCsv,
  surveyRows,
  type SurveyRow,
} from "./campaign/apps/app10";
import { expectDenied, expectOutsiderSeesNothing } from "./campaign/checks";
import { call, ok, type Json } from "./campaign/http";
import { people, phoneOf, type People } from "./campaign/people";
import { noSideScroll, shot } from "./campaign/shots";
import { cmp, files, rows, runs, type DataRecord, type RunDetail } from "./campaign/truth";
import { buildWorld, cleanupWorld, uploadFile, type World } from "./campaign/world";

/// APP-10 顧客アンケート（CSV）の分析（台帳 §7・§7.13 の APP-10）を組んで、CS（bob）が CSV を置き、マーケ（alice）が
/// 結果を見る。成功判定をデータで確かめる。LLM は stub（台帳 §7.13 の例外: 束の script が依頼文の最後の行に stub の
/// `json:` の答えを置く。要約は形の指定の無い「回答: <prompt>」の echo）。長い待ちは新しい版の 60 秒のまとめの 1 つだけ。
/// (1) 500 行が全件取り込まれる: event の run が succeeded・回答の件数 500・csv.query は truncated=false・total_rows=500・
///     取込の rows_applied の合計 500・rows_rejected 0・推奨度は数で入る。Shift_JIS の版も同じ。AI の束が失敗した分の行も残る。
/// (2) NPS が手計算と一致: 分析の行・集計の step 出力の groups（suppressed=false）・レポートの本文（stub の echo）。5 件
///     未満の区分は null・suppressed=true で、伏せたことがレポートに書かれる。
/// (3) 再取込で件数が増えない: 同じ CSV を新しいファイルで置いても 500 のまま・どの取込も rows_matched = rows_total・分析は
///     ファイルごとに 1 行。新しい版（コメントを 1 件直した版）は 60 秒のまとめの後に op=update の run が成功し、件数は
///     変わらず、直した行だけが変わる（同じ中身の新しい版と、コメントを直す版を 1 回の版の保存で確かめる・長い待ちは 1 つ）。
///     束の外の回答 ID を返す答えを混ぜても落とされて 500 のまま。
/// (4) LLM 呼び出しが 15 回以内: run の step で llm.invoke（cls・rep）を数える（束 10 ＋ 要約 1 = 11）。どれも 1 回目で
///     成功（attempt 1）し、答えは形に合う（直させる必要が無い）。実際の呼び出しの回数は手元で llm_usage を数えて報告する。
/// （追加）形に合わない・拒否・打ち切りの束は map の errors に入り run は成功・その 50 行は要確認。注入された指示で束の外の
///     行・削除・書く列は変わらない。権限（委譲で読む・actor は bob・charlie は何も読めない・フォルダーの外では起きない）。
/// `E2E_SHOTS_DIR` を渡すと、PC とスマホの画面を撮る。

test.describe.configure({ mode: "serial" });

let P: People;
let W: World;

test.beforeAll(async ({ browser }) => {
  test.setTimeout(300_000);
  P = await people(browser);
  W = await buildWorld(P, app10World());
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
const ROWS = surveyRows();
const EDITED = editedRows();

type Input = { file_id?: string; op?: string; version?: number; actor?: string; parent_id?: string };
type Bulk = { rows_total: number; rows_applied: number; rows_rejected: number; rows_invalid: number; rows_matched?: number; errors: unknown[] };

/// API でファイルを置く（declare → presigned PUT → finalize・`target` を渡すと新しい版）。
async function putFile(
  page: Page,
  opts: { parentId?: string; name?: string; body: Buffer; contentType?: string; target?: string; baseVersion?: number },
): Promise<{ id: string; version: number }> {
  const contentType = opts.contentType ?? "text/csv";
  const ticket = await ok<{ upload_id: string; upload_url: string }>(page, "POST", "/files", {
    parent_id: opts.target ? null : (opts.parentId ?? null),
    name: opts.target ? null : opts.name,
    content_type: contentType,
    size: opts.body.length,
    sha256: createHash("sha256").update(opts.body).digest("hex"),
    target_node_id: opts.target ?? null,
    base_version: opts.target ? (opts.baseVersion ?? null) : null,
  });
  const put = await page.request.put(ticket.upload_url, { headers: { "content-type": contentType }, data: opts.body });
  expect(put.ok(), `presigned PUT → ${put.status()} ${await put.text()}`).toBe(true);
  return ok<{ id: string; version: number }>(page, "POST", `/files/${ticket.upload_id}/finalize`);
}

/// きっかけの run のうち、`fileId` の `op` の保存で起きたもの（新しい順）。
async function eventRunsFor(fileId: string, op?: string): Promise<RunDetail[]> {
  const wf = W.workflows.survey.id;
  const out: RunDetail[] = [];
  for (const r of await runs.list(alice(), wf, { trigger_kind: "event" })) {
    const d = await runs.get(alice(), wf, r.run_id);
    const input = d.input as Input;
    if (input?.file_id === fileId && (op === undefined || input.op === op)) out.push(d);
  }
  return out;
}

/// `fileId` の `op` の保存で起きた run が出て、終わるまで待つ。
async function eventRunDone(fileId: string, op: string, timeout = 90_000): Promise<RunDetail> {
  let found: RunDetail | undefined;
  await expect
    .poll(
      async () => {
        found = (await eventRunsFor(fileId, op))[0];
        return Boolean(found);
      },
      { timeout, intervals: [2_000, 5_000], message: `${fileId} の ${op} で run が起きる` },
    )
    .toBe(true);
  return runs.waitEnd(alice(), W.workflows.survey.id, found!.run_id, 300_000);
}

/// 回答の件数（with_count の正確な件数）。
async function countResponses(): Promise<number> {
  const r = await ok<{ count?: { exact?: number; at_least?: number } }>(alice(), "POST", `/data/tables/${W.tables.responses.id}/query`, {
    limit: 1,
    with_count: true,
  });
  expect(r.count?.exact, `件数は正確な数（${JSON.stringify(r.count)}）`).toBeDefined();
  return r.count!.exact!;
}

/// 回答の全行（回答 ID → 行）。
async function allResponses(): Promise<Record<string, DataRecord>> {
  return Object.fromEntries((await rows.query(alice(), W.tables.responses.id)).map((r) => [String(r.data.response_id), r]));
}

/// step の一覧から、ノード id の step（map の要素は要素ごと）。
function stepsOf(d: RunDetail, nodeId: string) {
  return d.steps.filter((s) => s.node_id === nodeId);
}

/// map の要素の step の出力（要素の順）。
async function elementOutputs<T>(d: RunDetail, nodeId: string): Promise<{ path: string; out: T }[]> {
  const out: { path: string; out: T }[] = [];
  for (const s of stepsOf(d, nodeId)) {
    out.push({ path: s.step_path, out: await runs.stepOutput<T>(alice(), W.workflows.survey.id, d.run_id, s.step_path) });
  }
  return out.sort((a, b) => a.path.localeCompare(b.path, "en", { numeric: true }));
}

/// 分析の行（取り込んだファイルで絞る・ちょうど 1 行）。
async function reportFor(fileId: string): Promise<DataRecord> {
  const found = await rows.query(alice(), W.tables.reports.id, cmp("source_file_id", "eq", fileId));
  expect(found.length, `分析（${fileId}）はちょうど 1 行`).toBe(1);
  return found[0];
}

/// 区分ごとの手計算の NPS（K 未満は null）。
function segmentNps(src: SurveyRow[]): Record<string, number | null> {
  return Object.fromEntries(
    SEGMENTS.map(([s]) => {
      const rs = src.filter((x) => x.segment === s);
      return [s, rs.length < 5 ? null : npsOf(rs).nps];
    }),
  );
}

/// 行の感情・話題・要確認が、stub の答え（コメントの言葉から決めたもの）のとおり。
function expectLabels(byId: Record<string, DataRecord>, src: SurveyRow[], skip: Set<string> = new Set()) {
  const bad: string[] = [];
  for (const x of src) {
    if (skip.has(x.id)) continue;
    const r = byId[x.id];
    const want = { sentiment: ruleSentiment(x.comment), topics: ruleTopics(x.comment), needs_review: null };
    const got = { sentiment: r?.data.sentiment ?? null, topics: r?.data.topics ?? null, needs_review: r?.data.needs_review ?? null };
    if (JSON.stringify(got) !== JSON.stringify(want)) bad.push(`${x.id}: ${JSON.stringify(got)} ≠ ${JSON.stringify(want)}`);
  }
  expect(bad.slice(0, 10), `感情と話題（${bad.length} 件の食い違い）`).toEqual([]);
}

let file1 = "";
let run1 = "";

test("(1)(2)(4) bob がアンケートのフォルダーに 500 行の CSV を置くと、全件が取り込まれ、NPS が手計算と一致し、AI の呼び出しは 11 回", async ({
  browser,
}) => {
  test.setTimeout(420_000);
  const bob = P.bob.page;
  // きっかけのフォルダーの外に置いた CSV（run は起きない）。後で置く本番の CSV の run より前に置く（作成はすぐ起きるので、
  // 後の run が出た時点で、前の保存の run も出ているはず）。
  const outside = await putFile(bob, { parentId: W.folders.elsewhere.id, name: "回答-下書き.csv", body: Buffer.from(surveyCsv(ROWS.slice(0, 10)), "utf8") });

  // bob がドライブの画面から「アンケート」フォルダーに置く。
  file1 = await uploadFile(bob, W.folders.inbox.id, "顧客アンケート-2026-10.csv", Buffer.from(surveyCsv(ROWS), "utf8"), "text/csv");
  await shot(bob, "app10", "bob-drive-pc");
  const phone = await phoneOf(browser, P.bob);
  try {
    await phone.goto(`/drive?folder=${W.folders.inbox.id}`);
    await expect(phone.getByText("顧客アンケート-2026-10.csv").first()).toBeVisible({ timeout: 30_000 });
    await noSideScroll(phone);
    await shot(phone, "app10", "bob-drive-phone");
  } finally {
    await phone.context().close();
  }

  const d = await eventRunDone(file1, "create", 120_000);
  run1 = d.run_id;
  expect(d.status, JSON.stringify(d.failure)).toBe("succeeded");
  const wf = W.workflows.survey.id;
  const detail = await ok<{ created_at: string; started_at: string | null; finished_at: string | null }>(alice(), "GET", `/workflows/${wf}/runs/${d.run_id}`);
  const secs = (Date.parse(detail.finished_at ?? "") - Date.parse(detail.started_at ?? detail.created_at)) / 1000;
  test.info().annotations.push({ type: "500 行の run の時間", description: `${detail.created_at} 〜 ${detail.finished_at}（${secs} 秒）・run ${d.run_id}` });

  // 権限: bob が置いた CSV を、alice の委譲で読んだ（run の入力の actor は bob・op は create）。
  expect(d.input as Input).toMatchObject({ file_id: file1, op: "create", actor: P.bob.id, parent_id: W.folders.inbox.id });
  // フォルダーの外に置いた CSV では run が起きていない。
  expect(await eventRunsFor(outside.id), "きっかけのフォルダーの外の保存では run が起きない").toEqual([]);

  // (1) 全件: 件数 500・csv.query は打ち切りなし・取込の合計 500・拒否 0。
  expect(await countResponses()).toBe(500);
  const qOut = await runs.stepOutput<{ truncated: boolean; total_rows: number; columns: string[] }>(alice(), wf, d.run_id, "q");
  expect([qOut.truncated, qOut.total_rows]).toEqual([false, 500]);
  const imps = await elementOutputs<Bulk>(d, "imp");
  expect(imps.length, "束は 10").toBe(Math.ceil(500 / BATCH));
  expect(imps.reduce((n, x) => n + x.out.rows_applied, 0)).toBe(500);
  expect(imps.map((x) => [x.out.rows_rejected, x.out.rows_invalid])).toEqual(imps.map(() => [0, 0]));
  const m = await runs.stepOutput<{ items: Json[]; errors: unknown[] }>(alice(), wf, d.run_id, "m");
  expect(m.errors).toEqual([]);
  expect(m.items.length).toBe(10);
  const byId = await allResponses();
  expect(typeof byId.R0001.data.nps, "推奨度は数で入る").toBe("number");
  for (const x of ROWS) {
    expect(byId[x.id]?.data, x.id).toMatchObject({ nps: x.nps, segment: x.segment, submitted_on: x.date, source_file: file1 });
  }
  expectLabels(byId, ROWS);
  // 自動実行が作った行の作成者（GAP-58）: いまの動きは委譲元（有効化した alice）。docs はワークフローと書いている。
  expect(byId.R0001.created_by ?? (byId.R0001 as unknown as { owner?: string }).owner, "回答の行の作成者").toBe(P.alice.id);

  // (2) NPS: 手計算 = 分析の行 = 集計の step 出力の groups（suppressed=false）= レポートの本文。
  const want = npsOf(ROWS);
  test.info().annotations.push({ type: "手計算の NPS", description: JSON.stringify(want) });
  const rep = await reportFor(file1);
  expect(rep.data).toMatchObject({ responses: 500, promoters: want.promoters, passives: want.passives, detractors: want.detractors, nps: want.nps, needs_review: 0 });
  const agg = await runs.stepOutput<{ suppressed: boolean; groups: { key: { nps_class: string }; values: unknown[] }[] }>(alice(), wf, d.run_id, "agg");
  expect(agg.suppressed).toBe(false);
  expect(Object.fromEntries(agg.groups.map((g) => [g.key.nps_class, Number(g.values[0])]))).toEqual({
    推奨者: want.promoters,
    中立者: want.passives,
    批判者: want.detractors,
  });
  // 5 件未満の区分（その他・3 件）は値が null・suppressed=true。
  const seg = await runs.stepOutput<{ suppressed: boolean; groups: { key: { segment: string }; values: unknown[] }[] }>(alice(), wf, d.run_id, "seg");
  expect(seg.suppressed).toBe(true);
  const segOut = Object.fromEntries(seg.groups.map((g) => [g.key.segment, g.values]));
  expect(segOut.その他).toEqual([null, null]);
  const segWant = segmentNps(ROWS);
  for (const [s, n] of SEGMENTS) {
    if (segWant[s] === null) continue;
    expect(Number(segOut[s][1]), `${s} の件数`).toBe(n);
  }
  const reportFile = String(rep.data.report);
  const md = await files.text(alice(), reportFile);
  expect(md).toContain(`回答 500 件・NPS ${want.nps.toFixed(1)}（推奨者 ${want.promoters}・中立者 ${want.passives}・批判者 ${want.detractors}）`);
  for (const [s, n] of SEGMENTS) {
    const v = segWant[s];
    expect(md, `区分 ${s} の行`).toContain(v === null ? `| ${s} | — | — |` : `| ${s} | ${n} | ${v.toFixed(1)} |`);
  }
  expect(md).toContain("回答が 5 件未満の区分（その他）は、個人が分からないよう数を伏せています（—）。");
  expect(md).toContain("AI が分類できなかった回答: 0 件");
  // 要約（形の指定の無い llm.invoke）は stub の echo なので、AI の節にも同じ NPS が載る。
  const ai = md.slice(md.indexOf("## まとめ（AI）"));
  expect(ai).toContain(`回答: 顧客アンケートの集計`);
  expect(ai).toContain(`NPS ${want.nps.toFixed(1)}`);

  // (4) AI の呼び出し: llm.invoke の step は束 10 ＋ 要約 1 = 11（15 以下）。どれも 1 回目で成功。
  const llmSteps = d.steps.filter((s) => s.node_id === "cls" || s.node_id === "rep");
  expect(llmSteps.length).toBe(11);
  expect(llmSteps.length).toBeLessThanOrEqual(15);
  expect(llmSteps.map((s) => [s.status, s.attempt])).toEqual(llmSteps.map(() => ["succeeded", 1]));
  // 答えは形に合い（stub の json: をそのまま検証に通した）、束の 50 件がそろう（直させると stub は最小の値 = 0 件を返す）。
  const cls = await elementOutputs<{ data: { results: unknown[] } }>(d, "cls");
  expect(cls.map((x) => x.out.data.results.length)).toEqual(cls.map(() => BATCH));
  test.info().annotations.push({ type: "llm.invoke の step", description: llmSteps.map((s) => s.step_path).join(",") });

  // 注入: コメントの指示に関わらず、R0001 は残り（500 のまま）、R0123 も言葉のとおりの分類。
  expect(byId[INJECTED_ID].data.comment).toBe(INJECTION);
  expect(byId.R0001).toBeDefined();

  // alice が実行の記録（ブロックごとの記録）を見る画面。
  await alice().goto(`/workflows/${wf}/runs?run=${d.run_id}`);
  await expect(alice().locator('[role="dialog"]').getByText("ブロックごとの記録")).toBeVisible({ timeout: 30_000 });
  await shot(alice(), "app10", "alice-run-pc");
});

test("(3) 1 件のコメントを直した新しい版を置くと、60 秒のまとめの後に op=update の run が成功し、件数は 500 のまま、直した行だけが変わる", async () => {
  test.setTimeout(300_000);
  expect(file1, "前の確かめで CSV を置いた").not.toBe("");
  const before = await allResponses();
  const v2 = await putFile(P.bob.page, { target: file1, baseVersion: 1, body: Buffer.from(surveyCsv(EDITED), "utf8") });
  expect(v2.version).toBe(2);
  // 新しい版は、同じファイルの保存が 60 秒途切れてから 1 回だけ起きる（D-38）。
  const d = await eventRunDone(file1, "update", 180_000);
  expect(d.status, JSON.stringify(d.failure)).toBe("succeeded");
  expect((d.input as Input).version).toBe(2);
  expect(await countResponses()).toBe(500);
  const imps = await elementOutputs<Bulk>(d, "imp");
  expect(imps.map((x) => [x.out.rows_matched, x.out.rows_total])).toEqual(imps.map(() => [BATCH, BATCH]));

  // 直した行だけが変わる（コメントと、言葉から決めた感情・話題）。ほかの 499 行は版（rev）も変わらない。
  const after = await allResponses();
  const edited = after[EDITED_ID];
  expect(edited.data.comment).toBe(EDITED_COMMENT);
  expect(edited.data.sentiment).toBe(ruleSentiment(EDITED_COMMENT));
  expect(edited.data.topics).toEqual(ruleTopics(EDITED_COMMENT));
  expect(edited.rev).toBeGreaterThan(before[EDITED_ID].rev);
  const changed = Object.keys(after).filter((id) => id !== EDITED_ID && after[id].rev !== before[id]?.rev);
  expect(changed, "直していない行は変わらない（注入の指示があっても束の外の行は書き換えない）").toEqual([]);
  expect(after.R0001.rev).toBe(before.R0001.rev);
  expect(after[INJECTED_ID].rev).toBe(before[INJECTED_ID].rev);
  // 分析は同じファイルの 1 行を書き換える。
  const rep = await reportFor(file1);
  expect(rep.data.responses).toBe(500);
  expect(rep.data.nps).toBe(npsOf(EDITED).nps);
});

let file3 = "";

test("(1)(3) 同じ CSV を新しいファイルで置いても 500 のまま・どの取込も照合で更新・分析はファイルごとに 1 行。Shift_JIS の版も同じ", async () => {
  test.setTimeout(420_000);
  const bob = P.bob.page;
  const csv = surveyCsv(EDITED);
  const file2 = await putFile(bob, { parentId: W.folders.inbox.id, name: "顧客アンケート-2026-10-再送.csv", body: Buffer.from(csv, "utf8") });
  const d2 = await eventRunDone(file2.id, "create", 120_000);
  expect(d2.status, JSON.stringify(d2.failure)).toBe("succeeded");
  expect(await countResponses()).toBe(500);
  const imps2 = await elementOutputs<Bulk>(d2, "imp");
  expect(imps2.map((x) => [x.out.rows_matched, x.out.rows_total, x.out.rows_rejected])).toEqual(imps2.map(() => [BATCH, BATCH, 0]));
  await reportFor(file1);
  expect((await reportFor(file2.id)).data).toMatchObject({ responses: 500, nps: npsOf(EDITED).nps });

  // Shift_JIS（日本語の Windows の Excel の既定）の版。読み直したコメントは元の文と同じ（文字化けしない）。
  const sjis = shiftJis(csv);
  expect(sjis.length).toBeLessThan(Buffer.byteLength(csv, "utf8"));
  const f3 = await putFile(bob, { parentId: W.folders.inbox.id, name: "顧客アンケート-2026-10-sjis.csv", body: sjis });
  file3 = f3.id;
  const d3 = await eventRunDone(file3, "create", 120_000);
  expect(d3.status, JSON.stringify(d3.failure)).toBe("succeeded");
  const q3 = await runs.stepOutput<{ truncated: boolean; total_rows: number }>(alice(), W.workflows.survey.id, d3.run_id, "q");
  expect([q3.truncated, q3.total_rows]).toEqual([false, 500]);
  expect(await countResponses()).toBe(500);
  const imps3 = await elementOutputs<Bulk>(d3, "imp");
  expect(imps3.map((x) => [x.out.rows_matched, x.out.rows_total, x.out.rows_rejected])).toEqual(imps3.map(() => [BATCH, BATCH, 0]));
  const byId = await allResponses();
  const garbled = EDITED.filter((x) => byId[x.id]?.data.comment !== x.comment || byId[x.id]?.data.segment !== x.segment);
  expect(garbled.map((x) => x.id), "Shift_JIS のコメントと区分が元の文のまま").toEqual([]);
  expect(Object.values(byId).every((r) => r.data.source_file === file3)).toBe(true);
  expectLabels(byId, EDITED);
  const all = await rows.query(alice(), W.tables.reports.id);
  expect(all.map((r) => r.data.source_file_id).sort(), "分析は取り込んだファイルごとに 1 行").toEqual([file1, file2.id, file3].sort());
});

test("（追加）形に合わない・拒否・打ち切りの束は map の errors に入り、run は成功し、その 50 行は要確認。束の外の回答 ID は落とす", async () => {
  test.setTimeout(300_000);
  expect(file3, "前の確かめで Shift_JIS の CSV を置いた").not.toBe("");
  const wf = W.workflows.faults.id;
  const before = await allResponses();
  // 失敗の確かめの写し（有効化しない）を、置いた CSV をきっかけの内容にして alice が試しに動かす。
  const started = await ok<{ run_id: string }>(alice(), "POST", `/workflows/${wf}/runs`, { input: {}, trigger: { file_id: file3, op: "create" } }, [202]);
  const d = await runs.waitEnd(alice(), wf, started.run_id, 300_000);
  expect(d.status, JSON.stringify(d.failure)).toBe("succeeded");
  const m = await runs.stepOutput<{ items: Json[]; errors: { index: number; error: { code?: string } }[] }>(alice(), wf, d.run_id, "m");
  const codes = Object.fromEntries(m.errors.map((e) => [e.index, e.error?.code]));
  expect(codes).toEqual({ 1: "llm_output_invalid", 3: "llm_refused", 5: "llm_output_truncated" });
  for (const k of [1, 3, 5]) {
    const step = d.steps.find((s) => s.node_id === "cls" && s.step_path.startsWith(`m[${k}]`));
    expect(step?.status, `束 ${k} の AI`).toBe("failed");
  }
  // 失敗した束の 150 行は「要確認」で、感情と話題は空。取り込み（AI より前）は済んでいて、行は 500 のまま。
  expect(await countResponses()).toBe(500);
  const after = await allResponses();
  const failedIds = new Set([1, 3, 5].flatMap((k) => EDITED.slice(k * BATCH, (k + 1) * BATCH).map((x) => x.id)));
  for (const id of failedIds) {
    expect({ id, review: after[id].data.needs_review, s: after[id].data.sentiment ?? null, t: after[id].data.topics ?? null }).toEqual({
      id,
      review: REVIEW,
      s: null,
      t: null,
    });
  }
  const fl = await runs.stepOutput<Bulk>(alice(), wf, d.run_id, "fl");
  expect(fl).toMatchObject({ rows_total: 150, rows_applied: 150, rows_rejected: 0, rows_matched: 150 });
  // ほかの束の行は、版（rev）も変わらない（同じ中身・同じ分類）。
  const moved = EDITED.filter((x) => !failedIds.has(x.id) && after[x.id].rev !== before[x.id].rev).map((x) => x.id);
  expect(moved, "失敗した束の外の行は変わらない").toEqual([]);
  expectLabels(after, EDITED, failedIds);
  // 束 7: 答えに混ぜた束の外の回答 ID は落とし（行を作らない）、書く列は感情・話題・要確認だけ。
  expect(after[FOREIGN_ID]).toBeUndefined();
  const lab7 = await runs.stepOutput<{ dropped: string[]; missing: string[]; rows: { data: Json }[] }>(alice(), wf, d.run_id, "m[7].lab");
  expect(lab7.dropped).toEqual([FOREIGN_ID]);
  expect(lab7.missing).toEqual([]);
  const keys = new Set(lab7.rows.flatMap((r) => Object.keys(r.data)));
  expect([...keys].sort()).toEqual(["needs_review", "response_id", "sentiment", "topics"]);
  expect(FAULT_DRIVE["7"]).toBe("foreign");
  // 分析の行に要確認の件数（150）が載る。
  expect((await reportFor(file3)).data.needs_review).toBe(150);
});

test("権限: bob は置くだけで、回答・分析・レポート・ワークフローは読めない。charlie はどれも読めない", async () => {
  test.setTimeout(120_000);
  expect(run1).not.toBe("");
  const bob = P.bob.page;
  const rep = await reportFor(file1);
  await expectDenied(bob, [
    { method: "POST", path: `/data/tables/${W.tables.responses.id}/query`, data: { limit: 1 }, what: "回答" },
    { method: "POST", path: `/data/tables/${W.tables.reports.id}/query`, data: { limit: 1 }, what: "分析" },
    { path: `/nodes?parent_id=${W.folders.reports.id}`, what: "レポートのフォルダー" },
    { path: `/files/${String(rep.data.report)}/text`, what: "レポート" },
    { path: `/workflows/${W.workflows.survey.id}`, what: "アンケート分析" },
    { path: `/workflows/${W.workflows.survey.id}/runs/${run1}`, what: "bob の CSV で起きた run" },
  ]);
  // bob は自分の置いたフォルダーは読める（editor）。
  const own = await call(bob, "GET", `/nodes?parent_id=${W.folders.inbox.id}`);
  expect(own.status).toBe(200);
  // charlie（別の組織）は、表・ワークフロー・run・フォルダー・ファイル・レポートのどれも読めない。
  await expectDenied(P.charlie.page, [
    { path: `/files/${file1}/text`, what: "置いた CSV" },
    { path: `/files/${String(rep.data.report)}/text`, what: "レポート" },
  ]);
  await expectOutsiderSeesNothing(P.charlie.page, W, { workflowRuns: [["survey", run1]] });
});
