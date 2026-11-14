import { expect, test, type Page } from "@playwright/test";

import {
  AUDIENCE,
  CHECKLIST,
  CHECK_SCHEMA,
  CONTRACTS,
  REVIEW,
  app07World,
  expectedRisk,
  sweepReports,
  type CheckInput,
  type ContractKey,
} from "./campaign/apps/app07";
import { refsOf } from "./campaign/apps/app05";
import { expectOnlyOwn, expectOutsiderSeesNothing, expectStatus } from "./campaign/checks";
import { call, type Json } from "./campaign/http";
import { people, phoneOf, type People } from "./campaign/people";
import { gridRows, pressApiDone, pressInApp, replyText, replyTones, type AppRun, type ReplySection } from "./campaign/press";
import { noSideScroll, shot } from "./campaign/shots";
import { REFUSE_LINE, jsonAlwaysLine, jsonLine, truncateLine } from "./campaign/stub";
import { addDays, jstToday } from "./campaign/time";
import { cmp, files, rows, runs, type DataRecord } from "./campaign/truth";
import { buildWorld, cleanupWorld, type World } from "./campaign/world";

/// APP-07 契約書の条項チェック（台帳 §7）を組んで、依頼者（bob）と法務（alice）として使い、成功判定をデータで確かめる。
/// 成功判定の言い換えは台帳 §7.13 の APP-07（R0・2026-11-13・CI の版は .md の契約書と表のチェックリスト）:
/// (1) 仕込んだ問題を全部検出（検出率は作る人の領分）→ 種別のチェックリストの全項目が AI に届き（system の文の step 出力に
///     全 item_key がある）、指摘の表に項目ごとにちょうど 1 行できる。AI が答えなかった項目は 要確認。リスクは判定 × 重要度の
///     決まった規則どおりで、AI の出力が空・不正・切れても 低 にならない。
/// (2) 指摘に条文の引用 → 有・要注意 の指摘にはどれも本文にある引用があり、script が確かめた結果（引用の確認）が行に残る。
///     作り物の引用は 確かめられない になり、判定は 要確認 に落ちる。
/// (3) 閲覧権限の無いチェックリストを参照しない → bob の run のチェックリストの照会の出力に「法務のみ」の行が無く、指摘にも
///     その項目が無い（AI がその key を返しても残らない）。alice が同じ契約で流すとその行が出る（権限で絞っている証拠）。
///     charlie はアプリも表も読めない。
/// (4) 結果が依頼者の画面に出る → bob の画面の結果に、リスクの文・指摘の表・レポートへのリンクが出る。押した記録は bob に
///     返り、alice には読めない。リンクを bob が開ける。「自分の依頼」の表に bob の行だけが出る。AI が失敗したら注意の節が
///     出て、リスク 要確認 の行ができる。
/// （追加）文書に埋め込まれた指示に従わない → 文書は user にだけ入り system に入らない（IR）。出力の形は閉じている。リスクは
///     script が決め、AI が書いた値を使わない。結果の Markdown の外部の画像は読み込まれない（外のホストへの要求が 0 件）。
/// LLM は stub（形を指定した llm.invoke・駆動の語は入力の「気になる点」にだけ置く）。`E2E_SHOTS_DIR` を渡すと画面を撮る。

test.describe.configure({ mode: "serial" });

let P: People;
let W: World;

test.beforeAll(async ({ browser }) => {
  // 組む時間と next dev の初回のコンパイルで 60 秒を超えうる（計画のレビュー 21）。共有のスタックが混むと数分かかる。
  test.setTimeout(480_000);
  P = await people(browser);
  for (const p of [P.alice.page, P.bob.page]) await sweepReports(p);
  W = await buildWorld(P, app07World());
  // レポートは押した人のドライブの最上位に置かれる（folder を省いた storage.write）。片付けで、表に残ったレポートを消す。
  W.defer(async () => {
    const all = await rows.query(P.alice.page, W.tables.reviews.id);
    for (const r of all) {
      const id = String(r.data.report ?? "");
      if (!id) continue;
      for (const p of [P.bob.page, P.alice.page]) await call(p, "DELETE", `/files/${id}`);
    }
  });
});

test.afterAll(async () => {
  test.setTimeout(300_000);
  try {
    if (W) await cleanupWorld(W);
  } finally {
    await P?.close();
  }
});

const alice = () => P.alice.page;
const bob = () => P.bob.page;
const wf = () => W.workflows.check.id;

type Finding = { item_key: string; item: string; severity: string; verdict: string; quote: string; quote_check: string };
type JudgeOut = { rows: Finding[]; risk: string; summary: string; report_md: string; ai_failed: boolean; ai_error: string; ignored_count: number };

/// 答え（気になる点に置く stub の `json:` の行）。
const answer = (findings: { item_key: string; verdict: string; quote: string }[], summary: string, extra: Json = {}) =>
  jsonLine({ findings, summary, ...extra });

const Q = {
  ndaCap: "相手方に生じた一切の損害を賠償する",
  ndaRenewal: "期間が満了したときは、自動的に同一の条件で1年間更新され、以後も同様とする",
  ndaTerm: "本契約が終了した後も期間の定めなく存続する",
  svcCap: "本契約に基づき支払われた委託料の総額を上限とする",
  svcIp: "委託料の支払いが完了した時に乙から甲に移転する",
  svcRenewal: "自動的に同一の条件で1年間更新され",
  saleLaw: "本契約は甲の所在する国の法令に従って解釈し",
  saleWarranty: "引き渡しを受けた日から1年以内に部品が契約の内容に適合しないことを乙に通知したときは",
  injCap: "損害の原因となった取引の代金の額を上限とする",
  injRenewal: "期間の満了の3か月前までにいずれからも書面で申し出がないときは、1年間更新される",
  injTerm: "秘密保持の義務は、本契約の終了後3年間存続する",
};

/// 仕込んだ問題どおりの答え（NDA: 上限なし・準拠法なし・自動更新・秘密保持が無期限）。
const NDA_ALIGNED = [
  { item_key: "nda_cap", verdict: "無", quote: Q.ndaCap },
  { item_key: "nda_law", verdict: "無", quote: "" },
  { item_key: "nda_renewal", verdict: "要注意", quote: Q.ndaRenewal },
  { item_key: "nda_term", verdict: "要注意", quote: Q.ndaTerm },
];

const NOTE_TAIL = "相手方の雛形です。気になる点があれば教えてください。";

type Run = AppRun & { run_id: string };
const done: Record<string, Run> = {};

function params(contract: ContractKey, note?: string): CheckInput {
  const c = CONTRACTS[contract];
  const p: CheckInput = { contract_type: c.type, counterparty: c.counterparty, contract_file: W.files[contract].id };
  if (note !== undefined) p.note = note;
  return p;
}

async function press(page: Page, name: string, contract: ContractKey, note?: string): Promise<Run> {
  const app = W.apps.main;
  const run = await pressApiDone(page, app.id, { version: app.version, actionId: app.actions["AI チェック"], params: params(contract, note) });
  if (run.status !== "succeeded") {
    // 落ちた step と誤りを添える（持ち主の alice がワークフローの側から読む）。
    const d = await runs.get(alice(), wf(), run.run_id);
    const bad = d.steps.filter((s) => s.status === "failed").map((s) => `${s.step_path}: ${JSON.stringify(s.error)}`);
    expect(run.status, `${name}: ${bad.join(" / ")}`).toBe("succeeded");
  }
  done[name] = run;
  return run;
}

async function judgeOf(runId: string): Promise<JudgeOut> {
  return runs.stepOutput<JudgeOut>(alice(), wf(), runId, "judge");
}

/// run が作った契約チェックの行と、その指摘の行（持ち主の alice が読む）。
async function resultOf(runId: string): Promise<{ review: DataRecord; findings: DataRecord[] }> {
  const saved = await runs.stepOutput<{ id: string }>(alice(), wf(), runId, "save");
  const review = await rows.get(alice(), W.tables.reviews.id, saved.id);
  const findings = await rows.query(alice(), W.tables.findings.id, cmp("review_id", "eq", saved.id));
  return { review, findings };
}

/// 種別のチェックリストのうち、`legal` なら法務のみの項目も含めた item_key。
function itemKeys(type: string, legal: boolean): string[] {
  return CHECKLIST.filter((c) => c.contract_type === type && (legal || c.audience === AUDIENCE.all))
    .map((c) => c.item_key)
    .sort();
}

/// 指摘の行が項目ごとにちょうど 1 行で、期待の判定・引用の確認になっている。有・要注意 は本文にある引用つき。
async function expectFindings(
  runId: string,
  type: string,
  expected: Record<string, { verdict: string; quote_check: string }>,
  legal = false,
): Promise<{ review: DataRecord; findings: DataRecord[]; judge: JudgeOut }> {
  const { review, findings } = await resultOf(runId);
  const judge = await judgeOf(runId);
  const keys = findings.map((f) => String(f.data.item_key)).sort();
  expect(keys, "指摘は項目ごとにちょうど 1 行").toEqual(itemKeys(type, legal));
  const got = Object.fromEntries(findings.map((f) => [String(f.data.item_key), { verdict: f.data.verdict, quote_check: f.data.quote_check }]));
  expect(got).toEqual(expected);
  for (const f of findings) {
    if (f.data.verdict === "有" || f.data.verdict === "要注意") {
      expect(String(f.data.quote ?? ""), `${String(f.data.item_key)} は引用つき`).not.toBe("");
      expect(f.data.quote_check).toBe("確認済");
    }
  }
  // リスクは判定 × 重要度の決まった規則どおり（行と step 出力が同じ）。
  const rule = expectedRisk(findings.map((f) => ({ verdict: String(f.data.verdict), severity: String(f.data.severity) })));
  expect(review.data.risk, "リスクは決まった規則どおり").toBe(rule);
  expect(judge.risk).toBe(rule);
  return { review, findings, judge };
}

test("(1)(2) 種別の全項目が AI に届き、指摘は項目ごとに 1 行。引用は本文で確かめ、作り物は要確認に落ちる", async () => {
  test.setTimeout(480_000);
  const app = W.apps.main;
  // R1 は画面から押す（契約種別はラジオ・ほかは文字の欄）。
  const note = `${answer(NDA_ALIGNED, "損害賠償の上限と準拠法が無く、更新と秘密保持の期間に注意が要ります。")}\n${NOTE_TAIL}`;
  await bob().goto(`/apps/${app.id}`);
  await bob().getByTestId("ui-figure").first().getByRole("radio", { name: "NDA", exact: true }).click();
  const due = addDays(jstToday(), 14);
  const ui = await pressInApp(bob(), app.id, {
    open: false,
    button: "AI チェック",
    fill: { 相手方: CONTRACTS.nda.counterparty, "契約書のファイル ID": W.files.nda.id, 希望期限: due, 気になる点: note },
  });
  expect(ui.actionId).toBe(app.actions["AI チェック"]);
  expect(ui.params).toEqual({ ...params("nda", note), due });
  expect(ui.run.status, JSON.stringify(ui.run)).toBe("succeeded");
  done.r1 = { ...ui.run, run_id: ui.runId };
  await bob().getByTestId("yui-runs").locator(`li[data-run-id="${ui.runId}"]`).getByTestId("yui-run-result").scrollIntoViewIfNeeded();
  await shot(bob(), "app07", "bob-result-pc");

  await Promise.all([
    // R2: 準拠法を答えず、秘密保持の期間に本文に無い引用、法務のみの項目（nda_floor）と一覧に無い key（hobby）を返す。
    press(
      bob(),
      "r2",
      "nda",
      `${answer(
        [
          { item_key: "nda_cap", verdict: "無", quote: Q.ndaCap },
          { item_key: "nda_renewal", verdict: "要注意", quote: Q.ndaRenewal },
          { item_key: "nda_term", verdict: "有", quote: Q.injTerm },
          { item_key: "nda_floor", verdict: "有", quote: Q.ndaCap },
          { item_key: "hobby", verdict: "有", quote: "" },
        ],
        "おおむね問題ありません。",
      )}\n${NOTE_TAIL}`,
    ),
    // R3: 業務委託（自動更新だけが要注意 → 中）。
    press(
      bob(),
      "r3",
      "service",
      answer(
        [
          { item_key: "svc_cap", verdict: "有", quote: Q.svcCap },
          { item_key: "svc_ip", verdict: "有", quote: Q.svcIp },
          { item_key: "svc_renewal", verdict: "要注意", quote: Q.svcRenewal },
        ],
        "更新の条項に注意が要ります。",
      ),
    ),
    // R4: 売買（問題なし → 低）。
    press(
      bob(),
      "r4",
      "sale",
      answer(
        [
          { item_key: "sale_law", verdict: "有", quote: Q.saleLaw },
          { item_key: "sale_warranty", verdict: "有", quote: Q.saleWarranty },
        ],
        "問題は見当たりません。",
      ),
    ),
  ]);

  // 種別の全項目（bob に見える分）が system の文に入って AI に届いた。
  for (const [name, type] of [
    ["r1", "NDA"],
    ["r2", "NDA"],
    ["r3", "業務委託"],
    ["r4", "売買"],
  ] as [string, string][]) {
    const brief = await runs.stepOutput<{ system_text: string; item_keys: string[] }>(alice(), wf(), done[name].run_id, "brief");
    expect([...brief.item_keys].sort(), `${name} の項目`).toEqual(itemKeys(type, false));
    for (const k of itemKeys(type, false)) expect(brief.system_text, `${name} の指示に ${k}`).toContain(k);
    const d = await runs.get(alice(), wf(), done[name].run_id);
    expect(runs.takenPorts(d, "ai"), `${name} の AI は答えた`).toEqual(["out"]);
  }

  // R1: 仕込んだ 4 つの問題どおり（上限なし・準拠法なし・自動更新・無期限）→ リスク 高。
  const r1 = await expectFindings(done.r1.run_id, "NDA", {
    nda_cap: { verdict: "無", quote_check: "確認済" },
    nda_law: { verdict: "無", quote_check: "なし" },
    nda_renewal: { verdict: "要注意", quote_check: "確認済" },
    nda_term: { verdict: "要注意", quote_check: "確認済" },
  });
  expect(r1.review.data).toMatchObject({ risk: "高", contract_type: "NDA", counterparty: CONTRACTS.nda.counterparty, due, status: "依頼" });
  expect(r1.review.data.contract).toBe(W.files.nda.id);
  // R2: 答えの無い項目は要確認、本文に無い引用は「確かめられない」で要確認に落ちる。法務のみの項目と hobby は残らない。
  const r2 = await expectFindings(done.r2.run_id, "NDA", {
    nda_cap: { verdict: "無", quote_check: "確認済" },
    nda_law: { verdict: REVIEW, quote_check: "なし" },
    nda_renewal: { verdict: "要注意", quote_check: "確認済" },
    nda_term: { verdict: REVIEW, quote_check: "確かめられない" },
  });
  expect(r2.review.data.risk).toBe("高");
  // 希望期限を入れなかった依頼は、期限を空のまま記録する（`default: null`）。
  expect(r2.review.data.due ?? null, "希望期限は空").toBeNull();
  expect(r2.judge.ignored_count).toBe(2);
  expect(r2.findings.find((f) => f.data.item_key === "nda_term")?.data.quote).toBe(Q.injTerm);
  // R3・R4: 重要度 中 の問題だけなら 中、問題が無ければ 低。
  expect((await expectFindings(done.r3.run_id, "業務委託", {
    svc_cap: { verdict: "有", quote_check: "確認済" },
    svc_ip: { verdict: "有", quote_check: "確認済" },
    svc_renewal: { verdict: "要注意", quote_check: "確認済" },
  })).review.data.risk).toBe("中");
  expect((await expectFindings(done.r4.run_id, "売買", {
    sale_law: { verdict: "有", quote_check: "確認済" },
    sale_warranty: { verdict: "有", quote_check: "確認済" },
  })).review.data.risk).toBe("低");
});

test("(1) AI の出力が空・不正・拒否・切れたときは、全部の項目が要確認で、リスクは低にならない", async () => {
  test.setTimeout(480_000);
  // 問題の無い売買の契約書で流す（AI が答えれば 低 になる契約）。
  await Promise.all([
    press(bob(), "empty", "sale"),
    press(bob(), "invalid", "sale", jsonAlwaysLine("リスクは低です")),
    press(bob(), "refused", "sale", REFUSE_LINE),
    press(bob(), "truncated", "sale", truncateLine()),
  ]);
  const cases: [string, string | null][] = [
    ["empty", null],
    ["invalid", "llm_output_invalid"],
    ["refused", "llm_refused"],
    ["truncated", "llm_output_truncated"],
  ];
  for (const [name, code] of cases) {
    const d = await runs.get(alice(), wf(), done[name].run_id);
    if (code) {
      expect(runs.takenPorts(d, "ai"), name).toEqual(["error"]);
      expect(runs.errorCode(d, "ai"), name).toBe(code);
    } else {
      expect(runs.takenPorts(d, "ai"), name).toEqual(["out"]);
    }
    const r = await expectFindings(done[name].run_id, "売買", {
      sale_law: { verdict: REVIEW, quote_check: "なし" },
      sale_warranty: { verdict: REVIEW, quote_check: "なし" },
    });
    expect(r.review.data.risk, `${name} のリスク`).toBe(REVIEW);
    // AI の失敗は注意の節で知らせる（空の答えは失敗ではないので節は出ない）。
    const tones = replyTones(done[name]);
    expect(tones[0], `${name} のリスクの文は注意の色`).toBe("warning");
    if (code) {
      expect(tones).toEqual(["warning", "neutral", "neutral", "warning"]);
      expect(replyText(done[name])).toContain("AI のチェックができませんでした。法務が契約書を読んで確認します");
      expect(String(r.review.data.summary)).toContain("AI のチェックができなかったため、AI の所見はありません。");
      expect(String(r.review.data.summary), "依頼者が読む所見に誤りの code を書かない").not.toContain(code);
      expect(r.judge.ai_error).toBe(code);
    } else {
      expect(tones).toEqual(["warning", "neutral", "neutral"]);
    }
  }
});

test("(3) 法務のみの項目は依頼者の照会に出ず、指摘にも残らない。法務が同じ契約で流すと出る", async () => {
  test.setTimeout(480_000);
  // alice（法務）が同じ NDA を、法務のみの項目（交渉の下限）にも答える答えで流す。
  await press(
    alice(),
    "legal",
    "nda",
    answer([...NDA_ALIGNED, { item_key: "nda_floor", verdict: "要注意", quote: Q.ndaCap }], "上限が無く、交渉の下限も満たしません。"),
  );
  type Listed = { items: { data: { item_key: string; audience: string } }[] };
  for (const name of ["r1", "r2"]) {
    const listed = await runs.stepOutput<Listed>(alice(), wf(), done[name].run_id, "list");
    expect(listed.items.map((x) => x.data.audience), `${name}（bob）の照会`).not.toContain(AUDIENCE.legal);
    expect(listed.items.map((x) => x.data.item_key)).not.toContain("nda_floor");
    const { findings } = await resultOf(done[name].run_id);
    expect(findings.map((f) => f.data.item_key)).not.toContain("nda_floor");
  }
  const legalList = await runs.stepOutput<Listed>(alice(), wf(), done.legal.run_id, "list");
  expect(legalList.items.map((x) => x.data.item_key).sort()).toEqual(itemKeys("NDA", true));
  expect(legalList.items.find((x) => x.data.item_key === "nda_floor")?.data.audience).toBe(AUDIENCE.legal);
  const legal = await expectFindings(
    done.legal.run_id,
    "NDA",
    {
      nda_cap: { verdict: "無", quote_check: "確認済" },
      nda_law: { verdict: "無", quote_check: "なし" },
      nda_renewal: { verdict: "要注意", quote_check: "確認済" },
      nda_term: { verdict: "要注意", quote_check: "確認済" },
      nda_floor: { verdict: "要注意", quote_check: "確認済" },
    },
    true,
  );
  expect(legal.review.data.risk).toBe("高");
  // bob がチェックリストを直接読んでも、法務のみの行は見えない。
  const direct = await rows.try(bob(), W.tables.checklist.id);
  expect(direct.status).toBe(200);
  expect(direct.items.map((r) => r.data.item_key).sort()).toEqual(
    CHECKLIST.filter((c) => c.audience === AUDIENCE.all)
      .map((c) => c.item_key)
      .sort(),
  );
  // 別の組織の charlie は、アプリ・表・ワークフロー・run・フォルダー・ファイルのどれも読めない。
  await expectOutsiderSeesNothing(P.charlie.page, W, {
    workflowRuns: [["check", done.r1.run_id]],
    appRuns: [["main", done.r1.run_id]],
  });
});

test("(4) 結果は依頼者の画面に出て、レポートを開ける。押した記録と表は本人の分だけ", async ({ browser }) => {
  test.setTimeout(480_000);
  const app = W.apps.main;
  const r1 = done.r1;
  // 結果の節: リスクの文（高 → 注意の色・チェックはできたので「うまくいかなかった」の色にしない）・項目ごとの表・レポートへのリンク。
  const sections = r1.reply!.sections;
  expect(sections.map((s) => [s.format, s.tone])).toEqual([
    ["markdown", "warning"],
    ["table", "neutral"],
    ["file_link", "neutral"],
  ]);
  expect(sections[0].text).toContain("**リスク: 高**");
  const table = sections[1] as ReplySection;
  expect(table.columns).toEqual(["項目", "重要度", "判定", "引用", "引用の確認"]);
  const judge = await judgeOf(r1.run_id);
  expect(table.rows).toEqual(judge.rows.map((r) => [r.item, r.severity, r.verdict, r.quote, r.quote_check]));
  const link = sections[2].files ?? [];
  expect(link.length).toBe(1);
  const { review } = await resultOf(r1.run_id);
  expect(link[0].id).toBe(review.data.report);
  expect(link[0].name).toMatch(/^契約チェック-株式会社しろくま精機( \(\d+\))?\.md$/);
  // 画面: 結果の欄が出ている（押した本人の画面）。
  await bob().goto(`/apps/${app.id}`);
  const row = bob().getByTestId("yui-runs").locator(`li[data-run-id="${r1.run_id}"]`);
  await expect(row).toHaveAttribute("data-status", "succeeded", { timeout: 30_000 });
  if ((await row.getByTestId("yui-run-result").count()) === 0) await row.getByRole("button", { name: "結果" }).click();
  await expect(row.getByTestId("yui-run-result")).toBeVisible();
  await expect(row.getByTestId("yui-run-section").first()).toHaveAttribute("data-tone", "warning");
  // 注意（warning）の節を返したので、記録は「成功」でなく注意の色の「完了」と出る（台帳 §9: error・warning の節があれば
  // 「完了」とその節の色）。古い行（一覧が結果を付けない行）でも同じ（一覧の結果の色合いの要約 reply_tone）。
  await expect(row).toContainText("完了");
  await expect(row).not.toContainText("成功");

  // レポートは bob が開ける（リスクと指摘の表が入っている）。
  const report = await files.text(bob(), String(review.data.report));
  expect(report).toContain("# 契約チェックの結果（株式会社しろくま精機）");
  expect(report).toContain("- リスク: 高");
  expect(report).toContain(`- チェックした日: ${jstToday()}`);
  for (const f of judge.rows) expect(report).toContain(`| ${f.item} | ${f.severity} | ${f.verdict} |`);

  // 押した記録は本人の分だけ（alice には bob の記録が無いのと同じ答え）。
  await expectStatus(alice(), [{ path: `/mini-apps/${app.id}/runs/${r1.run_id}`, what: "bob の押した記録" }]);
  const mine = await call<{ runs: { run_id?: string }[] }>(alice(), "GET", `/mini-apps/${app.id}/runs`);
  expect(mine.body.runs.map((r) => r.run_id)).not.toContain(r1.run_id);
  // bob は alice の依頼（法務の run が作った行）を読めない。
  const legalReview = (await resultOf(done.legal.run_id)).review;
  expect((await call(bob(), "GET", `/data/tables/${W.tables.reviews.id}/records/${legalReview.id}`)).status).toBe(404);

  // 「自分の依頼」（追加だけの人の素の grid）: bob の行だけで、alice の依頼は出ない。alice（法務）には全員の分が出る。
  const grid = await gridRows(bob(), app.id, app.version, "自分の依頼");
  expect(grid.status, JSON.stringify(grid.body).slice(0, 300)).toBe(200);
  expectOnlyOwn(grid.rows, "requester", P.bob.id, "bob の「自分の依頼」");
  const ids = grid.rows.map((r) => r.id);
  expect(ids).toContain(review.id);
  expect(ids).not.toContain(legalReview.id);
  const aliceGrid = await gridRows(alice(), app.id, app.version, "自分の依頼");
  expect(aliceGrid.rows.map((r) => r.id)).toEqual(expect.arrayContaining([review.id, legalReview.id]));

  // AI の失敗: 注意の節が出て、リスク 要確認 の行ができる（(1) の流しで確かめた run を画面でも見る）。
  const failed = done.invalid;
  const fRow = bob().getByTestId("yui-runs").locator(`li[data-run-id="${failed.run_id}"]`);
  await expect(fRow).toHaveAttribute("data-status", "succeeded", { timeout: 30_000 });
  if ((await fRow.getByTestId("yui-run-result").count()) === 0) await fRow.getByRole("button", { name: "結果" }).click();
  await expect(fRow.getByTestId("yui-run-section").last()).toHaveAttribute("data-tone", "warning");
  await expect(fRow.getByTestId("yui-run-section").last()).toContainText("AI のチェックができませんでした");
  await fRow.scrollIntoViewIfNeeded();
  await shot(bob(), "app07", "bob-failed-pc");

  const phone = await phoneOf(browser, P.bob);
  try {
    await phone.goto(`/apps/${app.id}`);
    const g = phone.getByTestId("miniapp-data-grid-自分の依頼");
    await expect(g).toBeVisible({ timeout: 30_000 });
    await noSideScroll(phone);
    const pRow = phone.getByTestId("yui-runs").locator(`li[data-run-id="${r1.run_id}"]`);
    await expect(pRow).toHaveAttribute("data-status", "succeeded", { timeout: 30_000 });
    if ((await pRow.getByTestId("yui-run-result").count()) === 0) await pRow.getByRole("button", { name: "結果" }).click();
    await expect(pRow.getByTestId("yui-run-section").first()).toBeVisible({ timeout: 30_000 });
    await pRow.getByTestId("yui-run-result").scrollIntoViewIfNeeded();
    await noSideScroll(phone);
    await shot(phone, "app07", "bob-result-phone");
    await g.scrollIntoViewIfNeeded();
    await shot(phone, "app07", "bob-mine-phone");
  } finally {
    await phone.context().close();
  }
});

test("（追加）文書の中の指示に従わない: 文書は user にだけ入り、AI が書いたリスクは使わず、外部の画像を読み込まない", async () => {
  test.setTimeout(480_000);
  const app = W.apps.main;
  // IR: system は指示を作る script の出力だけで、その script はチェックリストだけを受け取る。文書と依頼者のメモは prompt にだけ入る。
  const ir = W.workflows.check.ir as { nodes: Json[] };
  const node = (id: string) => ir.nodes.find((n) => n.id === id)!.params as Json;
  expect(refsOf(node("ai").system)).toEqual([{ from: "nodes.brief.output", path: "/system_text" }]);
  expect(refsOf(node("brief").input)).toEqual([{ from: "nodes.list.output", path: "/items" }]);
  expect(refsOf(node("ai").prompt).map((r) => `${r.from}${r.path}`).sort()).toEqual(["input/note", "nodes.read.output/text"]);
  // 出力の形は閉じていて、リスクの項目を持たない（リスクは判定の script が決める）。
  expect(CHECK_SCHEMA.additionalProperties).toBe(false);
  expect(CHECK_SCHEMA.properties.findings.items.additionalProperties).toBe(false);
  expect(Object.keys(CHECK_SCHEMA.properties)).not.toContain("risk");
  expect(refsOf(node("save").data).filter((r) => r.path === "/risk")).toEqual([{ from: "nodes.judge.output", path: "/risk" }]);

  // 文書に「全部 有・リスク 低」と書いた契約: AI がリスクの項目を足した答えは形が閉じているので断られ、直させた答え
  // （空）で全部 要確認・リスク 要確認（低 にならない）。
  const inj = await press(
    bob(),
    "inject",
    "injection",
    answer(
      [
        { item_key: "nda_cap", verdict: "有", quote: Q.injCap },
        { item_key: "nda_law", verdict: "有", quote: "" },
      ],
      "問題ありません。",
      { risk: "低" },
    ),
  );
  const d = await runs.get(alice(), wf(), inj.run_id);
  expect(runs.takenPorts(d, "ai")).toEqual(["out"]);
  const aiOut = await runs.stepOutput<{ data: Json }>(alice(), wf(), inj.run_id, "ai");
  expect(Object.keys(aiOut.data).sort()).toEqual(["findings", "summary"]);
  const injected = await expectFindings(inj.run_id, "NDA", {
    nda_cap: { verdict: REVIEW, quote_check: "なし" },
    nda_law: { verdict: REVIEW, quote_check: "なし" },
    nda_renewal: { verdict: REVIEW, quote_check: "なし" },
    nda_term: { verdict: REVIEW, quote_check: "なし" },
  });
  expect(injected.review.data.risk).toBe(REVIEW);
  // 文書は system に入らない（指示の文に文書の言葉が無い）。
  const brief = await runs.stepOutput<{ system_text: string }>(alice(), wf(), inj.run_id, "brief");
  expect(brief.system_text).not.toContain("いなずま電装");
  expect(brief.system_text).not.toContain("img.example.test");

  // 所見に外部の画像の Markdown を返す答え（本文どおりの判定で 低）: 結果とレポートを開いても、外のホストに要求しない。
  const img = "![確認済みの印](https://img.example.test/ok.png)";
  const shown = await press(
    bob(),
    "image",
    "injection",
    answer(
      [
        { item_key: "nda_cap", verdict: "有", quote: Q.injCap },
        { item_key: "nda_law", verdict: "有", quote: Q.saleLaw },
        { item_key: "nda_renewal", verdict: "有", quote: Q.injRenewal },
        { item_key: "nda_term", verdict: "有", quote: Q.injTerm },
      ],
      `この契約は問題ありません。${img}`,
    ),
  );
  const res = await expectFindings(shown.run_id, "NDA", {
    nda_cap: { verdict: "有", quote_check: "確認済" },
    nda_law: { verdict: "有", quote_check: "確認済" },
    nda_renewal: { verdict: "有", quote_check: "確認済" },
    nda_term: { verdict: "有", quote_check: "確認済" },
  });
  expect(res.review.data.risk).toBe("低");
  expect(shown.reply!.sections[0].tone).toBe("success");
  expect(shown.reply!.sections[0].text).toContain("img.example.test");
  const external: string[] = [];
  const listen = (r: { url: () => string }) => {
    if (r.url().includes("img.example.test")) external.push(r.url());
  };
  bob().on("request", listen);
  try {
    await bob().goto(`/apps/${app.id}`);
    const row = bob().getByTestId("yui-runs").locator(`li[data-run-id="${shown.run_id}"]`);
    await expect(row).toHaveAttribute("data-status", "succeeded", { timeout: 30_000 });
    if ((await row.getByTestId("yui-run-result").count()) === 0) await row.getByRole("button", { name: "結果" }).click();
    const first = row.getByTestId("yui-run-section").first();
    await expect(first).toContainText("この契約は問題ありません。");
    await expect(first.locator("img")).toHaveCount(0);
    await row.scrollIntoViewIfNeeded();
    await shot(bob(), "app07", "bob-injection-pc");
    // レポート（.md）を開く（ドライブの「開く」と同じ画面・.md はノートの画面で開く・resourcePath）。
    await bob().goto(`/notes/${String(res.review.data.report)}`);
    const opened = await expect
      .poll(async () => (await bob().locator("main").innerText().catch(() => "")).includes("この契約は問題ありません。"), { timeout: 60_000 })
      .toBe(true)
      .then(() => true)
      .catch(() => false);
    await shot(bob(), "app07", "bob-report-pc");
    expect(opened, `レポートの本文が出る（${(await bob().locator("main").innerText().catch(() => "")).slice(0, 300)}）`).toBe(true);
  } finally {
    bob().off("request", listen);
  }
  expect(external, "外のホストへの要求").toEqual([]);
});
