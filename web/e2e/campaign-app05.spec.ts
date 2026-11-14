import { expect, test, type Page } from "@playwright/test";

import {
  APPROVAL_TITLE,
  LEVELS,
  PEOPLE,
  REQUIREMENTS,
  REVIEW,
  app05World,
  expectedScore,
  judgeIr,
  refsOf,
  runForRecord,
  screenIr,
  waitStep,
  type Applicant,
  type PersonKey,
} from "./campaign/apps/app05";
import { expectDenied, expectOutsiderSeesNothing, expectStatus } from "./campaign/checks";
import { call, ok, type Json } from "./campaign/http";
import { people, phoneOf, type People } from "./campaign/people";
import { gridRows, pressApiDone, pressInApp, replyText, replyTones, type AppRun, type ReplySection } from "./campaign/press";
import { noSideScroll, shot } from "./campaign/shots";
import { REFUSE_LINE, jsonAlwaysLine, jsonLine } from "./campaign/stub";
import { approvals, audit, cmp, expectSingleRow, rows, runs, type DataRecord, type RunDetail } from "./campaign/truth";
import { buildWorld, cleanupWorld, type World } from "./campaign/world";

/// APP-05 採用応募の書類スクリーニング（台帳 §7）を組んで、人事（alice）と面接官（bob）として使い、成功判定をデータで
/// 確かめる。成功判定の言い換えは台帳 §7.13 の APP-05（R0・2026-11-13）:
/// (1) 評価理由が要件の項目に紐づく → 職種の要件の key の集合と、点数の step 出力・行の評価の理由の key の集合が一致し、
///     どの要件にも判定が 1 つある。AI が答えなかった要件は 要確認 で needs_review=あり。AI が返した一覧に無い key（hobby）は
///     記録されない。
/// (2) 同じ入力で点数が再現する → 点数は、検証済みの構造化出力から script が決める関数。「同じ入力でもう一度実行」すると
///     点数の step 出力が同じで、同じ応募番号の行は 1 件のまま（2 回目は unique_violation から注意の返事へ）。
/// (3) 自動で見送りにならない → 最小値・`"decision":"見送り"` を足した json（形が閉じているので断られ、直させた後は
///     最小値）・AI の失敗（`jsonalways:`・`refuse:`）・0 点のどれでも、行は 評価済 に留まり、alice の受信箱に 1 件出る。
///     コメントの無い却下は 400。却下すると 見送り になり、変更の記録に alice、承認の記録にコメントが残る。bob（面接官）
///     は見送りにできない（遷移も承認の却下も 403）。強い候補は承認なしで 書類通過 になる。
/// (4) 個人情報が LLM の記録に過剰に残らない → (a) 本番の IR で、prompt の $template に入る step 出力と prompt・system の
///     値式に、氏名・連絡先が無い (b) bob は表・ビュー・承認の show_fields で氏名と履歴書を読めない (c) 履歴書を読んだ
///     監査はファイルの id と大きさだけ (d) run の step 出力を読めるのは持ち主（ワークフローの editor）と押した本人だけ。
///     生成の記録の preview（Langfuse・先頭 200 字）は E2E の外（コードの読み取りで台帳に記録する）。
/// 権限: 別の組織の charlie はどれも読めず存在も分からない。
/// LLM は stub（形を指定した llm.invoke・駆動の語は入力の志望動機にだけ置く）。`E2E_SHOTS_DIR` を渡すと画面を撮る。

test.describe.configure({ mode: "serial" });

let P: People;
let W: World;

test.beforeAll(async ({ browser }) => {
  // 組む時間と next dev の初回のコンパイルで 60 秒を超えうる（計画のレビュー 21）。
  test.setTimeout(300_000);
  P = await people(browser);
  W = await buildWorld(P, app05World());
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
const bob = () => P.bob.page;
const applicants = () => W.tables.applicants.id;
const screenWf = () => W.workflows.screen.id;
const judgeWf = () => W.workflows.judge.id;

type ScoreOut = {
  score: number;
  needs_review: string;
  reason: string;
  rows: { key: string; label: string; kind: string; level: string; evidence: string }[];
  review_count: number;
  ignored_count: number;
  ai_failed: boolean;
  ai_error: string;
};

/// 試す応募者（応募番号・履歴書の持ち主・職種・志望動機の前に置く駆動の行）。
type Case = { no: string; who: PersonKey; position: "経理" | "営業"; years: number; driver?: string; levels?: Record<string, string> };

const all = (position: string, level: string) =>
  REQUIREMENTS.filter((r) => r.position === position).map((r) => ({ key: r.req_key, evidence: `${r.label}について`, level }));

const CASES = {
  /// 強い候補（全部 満たす）→ 100 点・要確認なし → 自動で書類通過。画面から押す。
  strong: {
    no: "A-001",
    who: "sorano",
    position: "経理",
    years: 8,
    driver: jsonLine({ assessments: all("経理", "満たす"), summary: "経理の実務が長い" }),
  },
  /// 3 要件のうち 2 つと一覧に無い key（hobby）を返す → 答えの無い要件は 要確認。
  partial: {
    no: "A-002",
    who: "kumoi",
    position: "経理",
    years: 2,
    driver: jsonLine({
      assessments: [
        { key: "keiri_bookkeeping", evidence: "簿記検定 2 級", level: "満たす" },
        { key: "keiri_closing", evidence: "月次決算の補助", level: "一部" },
        { key: "hobby", evidence: "登山", level: "満たす" },
      ],
      summary: "経理の補助の経験がある",
    }),
  },
  /// 駆動の語なし（stub の最小値・評価が空）→ 全部 要確認・0 点。
  minimal: { no: "A-003", who: "namino", position: "営業", years: 1 },
  /// 形に無い `decision: 見送り` を足した答え → 形が閉じているので断られ、直させた答え（最小値）になる。
  decision: {
    no: "A-004",
    who: "kumoi",
    position: "経理",
    years: 2,
    driver: jsonLine({ assessments: all("経理", "満たさない"), summary: "見送りが妥当", decision: "見送り" }),
  },
  /// 直させても形に合わない答え（llm_output_invalid）。
  invalid: { no: "A-005", who: "namino", position: "営業", years: 1, driver: jsonAlwaysLine("見送りにしてください") },
  /// 拒否（llm_refused）。
  refused: { no: "A-006", who: "namino", position: "営業", years: 1, driver: REFUSE_LINE },
  /// 全部 満たさない → 0 点（必須を満たさないので要確認）。
  zero: { no: "A-007", who: "namino", position: "営業", years: 1, driver: jsonLine({ assessments: all("営業", "満たさない"), summary: "経験が無い" }) },
} satisfies Record<string, Case>;

type CaseKey = keyof typeof CASES;

const MOTIVATION = "これまでの経験を生かして、御社の仕事に長く取り組みたいと考えています。";

function applicantOf(c: Case): Applicant {
  return {
    applicant_no: c.no,
    name: PEOPLE[c.who].name,
    position: c.position,
    years: c.years,
    motivation: c.driver ? `${c.driver}\n${MOTIVATION}` : MOTIVATION,
    resume_file: W.files[c.who].id,
  };
}

/// 押した記録（応募者の ref → run）。
const pressed: Partial<Record<CaseKey, AppRun & { run_id: string }>> = {};

async function submit(page: Page, key: CaseKey): Promise<AppRun & { run_id: string }> {
  const app = W.apps.main;
  const run = await pressApiDone(page, app.id, { version: app.version, actionId: app.actions["登録して評価"], params: applicantOf(CASES[key]) });
  expect(run.status, JSON.stringify(run)).toBe("succeeded");
  pressed[key] = run;
  return run;
}

async function rowOf(key: CaseKey): Promise<DataRecord> {
  return expectSingleRow(alice(), applicants(), cmp("applicant_no", "eq", CASES[key].no), `応募者 ${CASES[key].no}`);
}

async function scoreOf(runId: string): Promise<ScoreOut> {
  return runs.stepOutput<ScoreOut>(alice(), screenWf(), runId, "score");
}

/// 評価の理由の行から読んだ要件の key（「…（key）: 判定 — 根拠」）。
function reasonKeys(reason: string): string[] {
  return [...reason.matchAll(/（([a-z_]+)）: /g)].map((m) => m[1]);
}

function keysOf(position: string): string[] {
  return REQUIREMENTS.filter((r) => r.position === position).map((r) => r.req_key).sort();
}

test("(1) 評価の理由は職種の要件ごとに 1 つずつで、答えの無い要件は要確認、一覧に無い key は残らない", async () => {
  test.setTimeout(240_000);
  const app = W.apps.main;
  // 強い候補は画面から押す（応募職種はラジオ・ほかは文字の欄）。
  const strong = applicantOf(CASES.strong);
  await alice().goto(`/apps/${app.id}`);
  await alice().getByTestId("ui-figure").first().getByRole("radio", { name: "経理", exact: true }).click();
  const ui = await pressInApp(alice(), app.id, {
    open: false,
    button: "登録して評価",
    fill: {
      応募番号: strong.applicant_no,
      氏名: strong.name,
      経験年数: String(strong.years),
      志望動機: strong.motivation,
      "履歴書のファイル ID": strong.resume_file,
    },
  });
  expect(ui.actionId).toBe(app.actions["登録して評価"]);
  expect(ui.params).toEqual(strong);
  expect(ui.run.status, JSON.stringify(ui.run)).toBe("succeeded");
  pressed.strong = { ...ui.run, run_id: ui.runId };
  expect(replyTones(ui.run)).toEqual(["neutral", "neutral"]);
  expect(replyText(ui.run)).toContain("応募番号 A-001 を登録しました。評価点は 100 点（100 点満点）で、確認が要る要件は 0 件です");
  await alice().getByTestId("yui-runs").locator(`li[data-run-id="${ui.runId}"]`).getByTestId("yui-run-result").scrollIntoViewIfNeeded();
  await shot(alice(), "app05", "alice-result-pc");

  await Promise.all([submit(alice(), "partial"), submit(alice(), "minimal")]);

  for (const key of ["strong", "partial", "minimal"] as CaseKey[]) {
    const c = CASES[key];
    const runId = pressed[key]!.run_id;
    const d = await runs.get(alice(), screenWf(), runId);
    expect(d.status).toBe("succeeded");
    expect(runs.takenPorts(d, "ai"), `${c.no} の AI は答えた`).toEqual(["out"]);
    const s = await scoreOf(runId);
    // 要件の key の集合と評価の key の集合が一致し、どの要件にも判定が 1 つある。
    expect(s.rows.map((r) => r.key).sort(), `${c.no} の評価の key`).toEqual(keysOf(c.position));
    for (const r of s.rows) expect([...LEVELS, REVIEW], `${c.no} ${r.key} の判定`).toContain(r.level);
    // 行: 点数・要確認・評価の理由は step 出力のまま。理由の key も要件の key と一致する。
    const row = await rowOf(key);
    expect(row.data).toMatchObject({ score: s.score, needs_review: s.needs_review, reason: s.reason, position: c.position });
    expect(reasonKeys(String(row.data.reason)).sort(), `${c.no} の評価の理由の key`).toEqual(keysOf(c.position));
    const levels = Object.fromEntries(s.rows.map((r) => [r.key, r.level]));
    expect(s.score, `${c.no} の点数は決まった計算`).toBe(expectedScore(c.position, levels));
  }

  const strongOut = await scoreOf(pressed.strong!.run_id);
  expect(strongOut).toMatchObject({ score: 100, needs_review: "なし", review_count: 0, ai_failed: false });

  // 2 つだけ答えた候補: 答えの無い「表計算ソフトでの集計」は要確認で、要確認あり。hobby は捨て、どこにも残らない。
  const partial = await scoreOf(pressed.partial!.run_id);
  expect(Object.fromEntries(partial.rows.map((r) => [r.key, r.level]))).toEqual({
    keiri_bookkeeping: "満たす",
    keiri_closing: "一部",
    keiri_sheet: REVIEW,
  });
  expect(partial).toMatchObject({ score: 64, needs_review: "あり", review_count: 1, ignored_count: 1 });
  const partialRow = await rowOf("partial");
  expect(String(partialRow.data.reason)).not.toContain("hobby");
  expect(String(partialRow.data.reason)).not.toContain("登山");
  expect(JSON.stringify(partial.rows)).not.toContain("hobby");

  // 最小値（評価が空）: 全部 要確認・0 点・要確認あり。
  const minimal = await scoreOf(pressed.minimal!.run_id);
  expect(minimal.rows.every((r) => r.level === REVIEW)).toBe(true);
  expect(minimal).toMatchObject({ score: 0, needs_review: "あり", review_count: 3 });

  // 画面の結果の表は、点数の step 出力の行と同じ（要件・区分・判定・根拠）。
  const table = pressed.partial!.reply!.sections.find((x) => x.format === "table") as ReplySection;
  expect(table.columns).toEqual(["要件", "区分", "判定", "根拠"]);
  expect(table.rows).toEqual(partial.rows.map((r) => [r.label, r.kind, r.level, r.evidence]));
});

test("(2) 同じ入力でもう一度実行すると点数の step 出力が同じで、同じ応募番号の行は 1 件のまま", async () => {
  test.setTimeout(180_000);
  const first = pressed.strong;
  expect(first, "(1) の強い候補").toBeDefined();
  const before = await scoreOf(first!.run_id);
  // 書類の判定が動き終わってから比べる（強い候補は書類通過に進む）。
  const row0 = await rows.waitForStatus(alice(), applicants(), (await rowOf("strong")).id, ["書類通過"], { timeout: 60_000 });

  // 実行履歴の「同じ入力でもう一度実行」（元の版・同じ入力・持ち主の alice の権限）。
  const again = await ok<{ run_id: string }>(alice(), "POST", `/workflows/${screenWf()}/runs/${first!.run_id}/retry`, { mode: "new" }, [202]);
  const d = await runs.waitEnd(alice(), screenWf(), again.run_id);
  expect(d.status).toBe("succeeded");
  expect((d.input as Applicant).applicant_no).toBe(CASES.strong.no);
  const after = await scoreOf(again.run_id);
  expect(after, "同じ構造化出力から同じ点数・同じ理由").toEqual(before);
  // 行は作らず、unique_violation から注意の返事へ流れた。
  expect(runs.takenPorts(d, "save")).toEqual(["error"]);
  expect(runs.errorCode(d, "save")).toBe("unique_violation");
  expect(runs.takenPorts(d, "dup")).toEqual(["unique_violation"]);
  expect(runs.stepStatus(d, "done")).toBe("skipped");
  const warn = await runs.stepOutput<ReplySection>(alice(), screenWf(), again.run_id, "r_dup");
  expect(warn).toMatchObject({ tone: "warning", text: "応募番号 A-001 はもう登録されています。登録済みの応募者は「判定待ちの応募者」の表で確かめてください" });

  // 画面からもう一度押しても同じ（押した人に注意の節が出る）。
  const app = W.apps.main;
  const r = await pressApiDone(alice(), app.id, { version: app.version, actionId: app.actions["登録して評価"], params: applicantOf(CASES.strong) });
  expect(r.status).toBe("succeeded");
  expect(replyTones(r)).toEqual(["warning"]);
  const row = await rowOf("strong");
  expect(row.rev, "行は書き換えない").toBe(row0.rev);
  expect(row.data.score).toBe(before.score);
  expect(await rows.count(alice(), applicants(), cmp("applicant_no", "eq", CASES.strong.no))).toBe(1);
});

/// 書類の判定の run が人の判定で止まり、alice の受信箱にその行の承認がちょうど 1 件ある。
async function expectAwaitingHuman(key: CaseKey): Promise<{ row: DataRecord; run: RunDetail }> {
  const row = await rowOf(key);
  const run = await runForRecord(alice(), judgeWf(), row.id);
  // 承認で止まった step は waiting_event（人の判断の知らせを待つ）。
  const d = await waitStep(alice(), judgeWf(), run.run_id, "ask", "waiting_event");
  expect(runs.takenPorts(d, "route"), `${CASES[key].no} は基準を満たさない`).toEqual(["false"]);
  expect(runs.stepStatus(d, "pass")).toBe("skipped");
  await approvals.approvalFor(alice(), row.id, APPROVAL_TITLE);
  const mine = (await approvals.inbox(alice())).filter((a) => a.record.id === row.id);
  expect(mine.length, `${CASES[key].no} の承認は 1 件`).toBe(1);
  const now = await rows.get(alice(), applicants(), row.id);
  expect(now.data.status, `${CASES[key].no} は評価済に留まる`).toBe("評価済");
  return { row: now, run: d };
}

test("(3) どの入力でも自動では見送りにならず、人の判定に回る。強い候補だけが承認なしで書類通過になる", async () => {
  test.setTimeout(300_000);
  await Promise.all((["decision", "invalid", "refused", "zero"] as CaseKey[]).map((k) => submit(alice(), k)));

  // 形に無い項目（decision）を足した答えは断られ、直させた答え（最小値）で全部 要確認・0 点。
  const dec = await runs.get(alice(), screenWf(), pressed.decision!.run_id);
  expect(runs.takenPorts(dec, "ai")).toEqual(["out"]);
  const decAi = await runs.stepOutput<{ data: Json }>(alice(), screenWf(), pressed.decision!.run_id, "ai");
  expect(Object.keys(decAi.data).sort()).toEqual(["assessments", "summary"]);
  expect(await scoreOf(pressed.decision!.run_id)).toMatchObject({ score: 0, needs_review: "あり", ai_failed: false });
  // AI の失敗（直させても形に合わない・拒否）は error の出口から同じ点数の script を通り、0 点・要確認・注意の節。
  for (const [key, code] of [
    ["invalid", "llm_output_invalid"],
    ["refused", "llm_refused"],
  ] as [CaseKey, string][]) {
    const d = await runs.get(alice(), screenWf(), pressed[key]!.run_id);
    expect(runs.takenPorts(d, "ai"), key).toEqual(["error"]);
    expect(runs.errorCode(d, "ai"), key).toBe(code);
    const s = await scoreOf(pressed[key]!.run_id);
    expect(s).toMatchObject({ score: 0, needs_review: "あり", ai_failed: true, ai_error: code });
    expect(s.reason).toContain("AI の評価ができませんでした。人事が履歴書を読んで判定します。");
    expect(s.reason, "人が読む理由に誤りの code を書かない").not.toContain(code);
    expect(replyTones(pressed[key]!)).toEqual(["neutral", "neutral", "warning"]);
    expect(replyText(pressed[key]!)).toContain("AI の評価ができなかったため、点数は 0 点です。人事が履歴書を読んで判定してください");
  }
  expect(await scoreOf(pressed.zero!.run_id)).toMatchObject({ score: 0, needs_review: "あり", ai_failed: false });

  // 強い候補は承認なしで書類通過（書類の判定の run は基準の true の出口から遷移し、承認は通らない）。
  const strong = await rows.waitForStatus(alice(), applicants(), (await rowOf("strong")).id, ["書類通過"], { timeout: 60_000 });
  const sRun = await runs.waitEnd(alice(), judgeWf(), (await runForRecord(alice(), judgeWf(), strong.id)).run_id);
  expect(sRun.status).toBe("succeeded");
  expect(runs.takenPorts(sRun, "route")).toEqual(["true"]);
  expect(runs.takenPorts(sRun, "pass")).toEqual(["out"]);
  expect(runs.stepStatus(sRun, "ask")).toBe("skipped");
  expect((await approvals.inbox(alice())).filter((a) => a.record.id === strong.id)).toEqual([]);

  // 弱い候補・AI の失敗・0 点・要確認のある候補は、どれも評価済のまま人の判定を待つ。
  const waiting: Partial<Record<CaseKey, { row: DataRecord; run: RunDetail }>> = {};
  for (const key of ["partial", "minimal", "decision", "invalid", "refused", "zero"] as CaseKey[]) {
    waiting[key] = await expectAwaitingHuman(key);
  }

  // 面接官（bob）は見送りにできない: 承認の却下は「進め方の規則で判断できない」403、表から直接の遷移も 403。
  const target = waiting.minimal!.row;
  const bobItem = (await approvals.inbox(bob())).find((a) => a.record.id === target.id);
  expect(bobItem, "面接官の受信箱にも出る").toBeDefined();
  const opened = await ok<{ decisions: { kind: string; allowed: boolean | null }[] }>(bob(), "GET", `/approvals/${bobItem!.approval_id}`);
  expect(Object.fromEntries(opened.decisions.map((x) => [x.kind, x.allowed]))).toEqual({ approve: true, reject: false });
  const bobReject = await call<{ code?: string }>(bob(), "POST", `/approvals/${bobItem!.approval_id}/decision`, {
    decision: "reject",
    comment: "経験が足りない",
    expected_rev: target.rev,
  });
  expect(bobReject.status, bobReject.text).toBe(403);
  const bobMove = await call(bob(), "POST", `/data/tables/${applicants()}/records/${target.id}/transition`, { to: "見送り", expected_rev: target.rev });
  expect(bobMove.status, bobMove.text).toBe(403);
  expect((await rows.get(alice(), applicants(), target.id)).rev, "bob の操作で行は変わらない").toBe(target.rev);

  // 人事（alice）: コメントの無い却下は 400。コメントを書くと見送りになり、変更の記録に alice、承認の記録にコメントが残る。
  const a = await approvals.approvalFor(alice(), target.id, APPROVAL_TITLE);
  for (const comment of [undefined, "  "]) {
    const r = await call<{ code?: string }>(alice(), "POST", `/approvals/${a.approval_id}/decision`, {
      decision: "reject",
      comment,
      expected_rev: a.record.rev,
    });
    expect(r.status, r.text).toBe(400);
    expect(r.body?.code).toBe("comment_required");
  }
  const why = "営業の必須の要件（法人向けの新規開拓）の経験が履歴書に無いため";
  await approvals.decide(alice(), a, "reject", why);
  const rejected = await rows.waitForStatus(alice(), applicants(), target.id, ["見送り"], { timeout: 30_000 });
  const revs = await rows.revisions(alice(), applicants(), target.id);
  const last = revs.reduce((x, y) => (Number(y.rev) > Number(x.rev) ? y : x));
  expect(last).toMatchObject({ rev: rejected.rev, changed_by: P.alice.id, change_kind: "transition" });
  const done = await runs.waitEnd(alice(), judgeWf(), waiting.minimal!.run.run_id);
  expect(done.status).toBe("succeeded");
  expect(runs.takenPorts(done, "ask")).toEqual(["rejected"]);
  const decided = await runs.stepOutput<Json>(alice(), judgeWf(), done.run_id, "ask");
  expect(decided).toMatchObject({ decision: "reject", to: "見送り", comment: why, decided_by: P.alice.id });

  // 面接官は書類通過にはできる（面接に呼ぶ判断）。
  const pass = waiting.partial!.row;
  const bobPass = (await approvals.inbox(bob())).find((x) => x.record.id === pass.id);
  expect(bobPass).toBeDefined();
  await approvals.decide(bob(), bobPass!, "approve");
  const passed = await rows.waitForStatus(alice(), applicants(), pass.id, ["書類通過"], { timeout: 30_000 });
  const passRevs = await rows.revisions(alice(), applicants(), pass.id);
  expect(passRevs.find((x) => Number(x.rev) === passed.rev)).toMatchObject({ changed_by: P.bob.id, change_kind: "transition" });

  // 見送りは人事の却下を通った 1 件だけ。どちらのワークフローにも見送りへ進めるノードは無い（見送りは承認の却下だけ）。
  const out = await rows.query(alice(), applicants(), cmp("status", "eq", "見送り"));
  expect(out.map((r) => r.data.applicant_no)).toEqual([CASES.minimal.no]);
  for (const ir of [screenIr, judgeIr]) {
    const nodes = (ir.nodes as Json[]).filter((n) => n.type === "data.transition");
    for (const n of nodes) expect((n.params as Json).to, `${String(ir.display_name)} の ${String(n.id)}`).not.toBe("見送り");
  }
  const ask = (judgeIr.nodes as Json[]).find((n) => n.id === "ask")!;
  expect(((ask.params as Json).reject as Json).to).toBe("見送り");
});

test("(4) 個人情報: AI に渡す材料に氏名・連絡先が無く、面接官には氏名と履歴書が見えず、監査は id だけ", async ({ browser }) => {
  test.setTimeout(240_000);
  // (a) 本番の IR: prompt の $template に入るのは応募職種・経験年数と、伏せた後の本文だけ。system は要件の一覧だけ。
  const ai = (screenIr.nodes as Json[]).find((n) => n.id === "ai")!.params as Json;
  const prompt = ai.prompt as { $template: string; vars: Json };
  expect(Object.keys(prompt.vars).sort()).toEqual(["body", "position", "years"]);
  expect(refsOf(prompt)).toEqual([
    { from: "input", path: "/position" },
    { from: "input", path: "/years" },
    { from: "nodes.mask.output", path: "/text" },
  ]);
  expect(refsOf(ai.system)).toEqual([{ from: "nodes.brief.output", path: "/system_text" }]);
  for (const r of refsOf(ai)) {
    expect(r, "AI に氏名や履歴書の原文を直接渡さない").not.toEqual({ from: "input", path: "/name" });
    expect(r.from).not.toBe("nodes.read.output");
  }
  // step 出力: 伏せた後の本文と指示の文に、氏名・ふりがな・連絡先・住所・生年月日が無い（履歴書の中身は届いている）。
  for (const key of ["strong", "partial", "minimal"] as CaseKey[]) {
    const c = CASES[key];
    const runId = pressed[key]!.run_id;
    const masked = (await runs.stepOutput<{ text: string }>(alice(), screenWf(), runId, "mask")).text;
    const brief = (await runs.stepOutput<{ system_text: string }>(alice(), screenWf(), runId, "brief")).system_text;
    // 見た目が同じ別の文字（康熙部首）で残っていないかも見る（NFKC でそろえて比べる・雲井の履歴書は「⽣年⽉⽇」）。
    for (const secret of [PEOPLE[c.who].name, ...PEOPLE[c.who].secrets]) {
      expect(masked.normalize("NFKC"), `${c.no} の伏せた本文に「${secret}」が無い`).not.toContain(secret);
      expect(brief.normalize("NFKC"), `${c.no} の指示に「${secret}」が無い`).not.toContain(secret);
    }
    expect(masked).not.toMatch(/@example\.com/);
    expect(masked).toContain("【履歴書】");
    expect(masked).toContain("職歴");
    expect(masked).toContain("（氏名は伏せました）");
  }
  // 履歴書の原文は読んだ step にはある（伏せる前・持ち主にだけ見える）。
  const raw = await runs.stepOutput<{ text: string; name: string }>(alice(), screenWf(), pressed.strong!.run_id, "read");
  expect(raw.text).toContain(PEOPLE.sorano.name);
  expect(raw.name).not.toContain(PEOPLE.sorano.name);

  // (b) 面接官（bob）は、表・行・ビュー（アプリの表）・承認の列のどこからも氏名と履歴書を読めない。
  const seen = await rows.try(bob(), applicants());
  expect(seen.status).toBe(200);
  expect(seen.items.length).toBeGreaterThan(0);
  for (const r of seen.items) {
    expect(Object.keys(r.data), `bob が読んだ ${String(r.data.applicant_no)}`).not.toContain("name");
    expect(Object.keys(r.data)).not.toContain("resume");
  }
  const one = await rows.get(bob(), applicants(), seen.items[0].id);
  expect(Object.keys(one.data)).not.toContain("name");
  const byName = await call(bob(), "POST", `/data/tables/${applicants()}/query`, { filter: cmp("name", "eq", PEOPLE.kumoi.name), limit: 5 });
  expect(byName.status, `bob が氏名で絞る → ${byName.text}`).toBe(403);
  const app = W.apps.main;
  const grid = await gridRows(bob(), app.id, app.version, "判定待ちの応募者");
  expect(grid.status).toBe(200);
  expect(grid.rows.length).toBeGreaterThan(0);
  for (const r of grid.rows) {
    expect(r.data.status).toBe("評価済");
    expect(Object.keys(r.data)).not.toContain("name");
    expect(Object.keys(r.data)).not.toContain("resume");
  }
  // alice（人事）の表には氏名が出る（決まった条件でなく、列の権限で伏せている証拠）。
  const aliceGrid = await gridRows(alice(), app.id, app.version, "判定待ちの応募者");
  expect(aliceGrid.rows.some((r) => r.data.name === PEOPLE.kumoi.name)).toBe(true);
  type Inbox = { items: { record: { id: string; fields: { key: string; value: unknown }[] } }[] };
  const bobInbox = await ok<Inbox>(bob(), "GET", "/approvals");
  const zeroRow = await rowOf("zero");
  const bobFields = bobInbox.items.find((x) => x.record.id === zeroRow.id)?.record.fields.map((f) => f.key);
  expect(bobFields, "bob の受信箱に出ている").toBeDefined();
  expect(bobFields).toEqual(expect.arrayContaining(["position", "score", "reason"]));
  expect(bobFields).not.toContain("name");
  expect(bobFields).not.toContain("resume");
  const aliceInbox = await ok<Inbox>(alice(), "GET", "/approvals");
  const aliceFields = aliceInbox.items.find((x) => x.record.id === zeroRow.id)?.record.fields;
  expect(aliceFields?.find((f) => f.key === "name")?.value).toBe(PEOPLE.namino.name);

  // (c) 履歴書を読んだ監査（file.read.internal）は、ファイルの id と大きさだけで、氏名も本文も残さない。
  const events = await audit.events(alice(), { action: "file.read.internal", object_id: W.files.sorano.id, limit: 50 });
  expect(events.length, "履歴書を読んだ監査がある").toBeGreaterThan(0);
  for (const e of events) {
    const s = JSON.stringify(e);
    for (const secret of [PEOPLE.sorano.name, ...PEOPLE.sorano.secrets, "月次決算"]) expect(s, `監査に「${secret}」が無い`).not.toContain(secret);
  }

  // (d) run の step 出力: bob は書類選考・書類の判定のワークフローと run を読めず、alice の押した記録もアプリから読めない。
  const runId = pressed.strong!.run_id;
  await expectDenied(bob(), [
    { path: `/workflows/${screenWf()}/runs`, what: "書類選考の run" },
    { path: `/workflows/${screenWf()}/runs/${runId}`, what: "alice の書類選考の run" },
    { path: `/workflows/${screenWf()}/runs/${runId}/steps?path=read`, what: "履歴書を読んだ step" },
    { path: `/workflows/${judgeWf()}/runs`, what: "書類の判定の run" },
  ]);
  await expectStatus(bob(), [{ path: `/mini-apps/${app.id}/runs/${runId}`, what: "alice の押した記録" }]);
  const bobList = await call<{ runs: { run_id?: string }[] }>(bob(), "GET", `/mini-apps/${app.id}/runs`);
  expect(bobList.status).toBe(200);
  expect(bobList.body.runs.map((r) => r.run_id)).not.toContain(runId);

  // 画面: bob（面接官）がスマホで判定待ちの応募者を見る（氏名の列が無い）。
  const phone = await phoneOf(browser, P.bob);
  try {
    await phone.goto(`/apps/${app.id}`);
    const g = phone.getByTestId("miniapp-data-grid-判定待ちの応募者");
    await expect(g).toBeVisible({ timeout: 30_000 });
    // 表の中身は画面の部品が描く（文字の裏取りは上の data_grid の行で行った）。件数の表示が出るまで待つ。
    await expect(g).toContainText(`${grid.rows.length} 件`, { timeout: 30_000 });
    await expect(g).not.toContainText(PEOPLE.namino.name);
    await noSideScroll(phone);
    await g.scrollIntoViewIfNeeded();
    await shot(phone, "app05", "bob-pending-phone");
  } finally {
    await phone.context().close();
  }
  const alicePhone = await phoneOf(browser, P.alice);
  try {
    await alicePhone.goto(`/apps/${app.id}`);
    await expect(alicePhone.getByTestId("ui-figure").getByRole("button", { name: "登録して評価", exact: true })).toBeVisible({ timeout: 30_000 });
    await noSideScroll(alicePhone);
    await shot(alicePhone, "app05", "alice-form-phone");
  } finally {
    await alicePhone.context().close();
  }
});

test("権限: charlie はどれも読めず存在も分からない", async () => {
  test.setTimeout(120_000);
  await expectOutsiderSeesNothing(P.charlie.page, W, {
    workflowRuns: [["screen", pressed.strong!.run_id]],
    appRuns: [["main", pressed.strong!.run_id]],
  });
});
