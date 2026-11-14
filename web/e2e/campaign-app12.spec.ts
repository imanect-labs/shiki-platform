import { expect, test, type Page } from "@playwright/test";

import {
  APPROVAL_TITLE,
  ASK_NODES,
  FAQ,
  NO_DRAFT,
  SECRET_MARK,
  SHOW_FIELDS,
  UNKNOWN,
  app12World,
  approvalDetail,
  waitRecordStep,
  type Faq,
} from "./campaign/apps/app12";
import { expectDenied, expectOnlyOwn, expectOutsiderSeesNothing, expectStatus } from "./campaign/checks";
import { call } from "./campaign/http";
import { people, phoneOf, type People } from "./campaign/people";
import { gridRows, pressApiDone, pressInApp, type AppRun, type ReplySection } from "./campaign/press";
import { noSideScroll, shot } from "./campaign/shots";
import { echoOf } from "./campaign/stub";
import { approvals, cmp, expectSingleRow, rows, runs, type DataRecord } from "./campaign/truth";
import { buildWorld, cleanupWorld, type World } from "./campaign/world";

/// APP-12 社内 FAQ ヘルプデスク（台帳 §7・§7.13 の APP-12・V1: FAQ を表に置く版）を組んで、使う人として使い、
/// 成功判定をデータで確かめる。成功判定の言い換え（§7.13）:
/// (1) 既知の質問（FAQ の質問の語を含む）は「自動回答」になり、出典はその run の照会で本人に返った FAQ の行、回答（stub の
///     echo）にはその資料の本文だけが入る。bob の画面に回答と出典が出る。何問当たったかは参考として残すだけ。
/// (2) 情シス向けの FAQ に当たる質問を bob が送っても、照会の出力にその行は無く、出典にも回答にも入らない。alice では入る。
///     エスカレーションの承認の前は、bob に下書き（draft・draft_source）が見えない（列の決まり）。bob は質問の
///     ワークフローとその run を読めない（ワークフローを共有しない）。
/// (3) FAQ に無い質問（照会が 0 件）はエスカレーションになり、情シス（alice）の受信箱に 1 件・bob には出ない。承認の
///     見せる列に下書きが出る。承認で完了になって回答に下書きが写り、差し戻しで対応中になる。追加の分かれ道（合否には
///     使わない）: 答えが途中で切れた（stop_reason=max_tokens・F4）・AI が失敗した（error の出口）ときも、run は落ちずに
///     エスカレーションへ進む。
/// (4) 自動回答の結果の節は bob の画面と bob の記録に出て、alice の記録には無い。エスカレーションの回答は、承認の後に
///     bob が開き直した「自分の問い合わせ」に入っている。通知（GAP-06）は判定の外。
/// （追加）FAQ の本文のボタンの図は結果の節でボタンにならず、外部のリンクにはホスト名が付く。step は IR のノードだけ。
/// 作ったものは afterAll で片付ける。`E2E_SHOTS_DIR` を渡すと、PC とスマホの画面を撮る。

test.describe.configure({ mode: "serial" });

let P: People;
let W: World;

test.beforeAll(async ({ browser }) => {
  test.setTimeout(300_000);
  P = await people(browser);
  W = await buildWorld(P, app12World());
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

/// 質問のボタンを API から押して、終わった記録を返す。
async function ask(page: Page, question: string, category = "PC"): Promise<AppRun & { run_id: string }> {
  const app = W.apps.main;
  return pressApiDone(page, app.id, { version: app.version, actionId: app.actions["質問する"], params: { question, category } });
}

/// 質問の問い合わせの行（ちょうど 1 行・alice は情シスなので全員の分を読める）。
async function ticket(question: string): Promise<DataRecord> {
  return expectSingleRow(alice(), W.tables.tickets.id, cmp("question", "eq", question), `「${question}」の問い合わせ`);
}

/// 質問の run の照会の出力に返った FAQ の番号（alice は質問のワークフローの持ち主なので、bob の run の step も読める
/// ・持ち主の読み取りで裏取りするだけで、bob の run の否定はミニアプリの入口で見る）。
async function foundNos(runId: string): Promise<string[]> {
  const o = await runs.stepOutput<{ items: { data: { faq_no: string } }[] }>(alice(), W.workflows.ask.id, runId, "find");
  return o.items.map((x) => x.data.faq_no);
}

/// 質問の prompt（IR の $template と同じ形・stub はこれを「回答: 」つきで折り返す）。
function promptOf(question: string, f: Faq): string {
  return `${question}\n\n資料（${f.faq_no}）:\n${f.question}\n${f.answer}`;
}

function sections(run: AppRun): ReplySection[] {
  return run.reply?.sections ?? [];
}

/// 結果の節が、その run の reply の step 出力と同じであること（画面に出たものと記録が一致する・GAP-09）。
async function expectSectionsMatchSteps(run: AppRun & { run_id: string }, nodeIds: string[]): Promise<void> {
  const got = sections(run);
  expect(got.length, `結果の節の数（${JSON.stringify(got)}）`).toBe(nodeIds.length);
  for (const [i, id] of nodeIds.entries()) {
    const step = await runs.stepOutput<{ tone: string; format: string; label?: string; text?: string }>(alice(), W.workflows.ask.id, run.run_id, id);
    expect(got[i], `節 ${i + 1}（${id}）`).toMatchObject({ tone: step.tone, format: step.format, label: step.label });
    expect(got[i].text?.trim(), `節 ${i + 1}（${id}）の文`).toBe(step.text?.trim());
  }
}

/// 既知の質問（FAQ の質問の一部）。どれも 1 つの FAQ の質問にだけ含まれる。
const KNOWN: { key: keyof typeof FAQ; q: string }[] = [
  { key: "vpn", q: "VPN につながらない" },
  { key: "password", q: "パスワードを忘れた" },
  { key: "closing", q: "経費の精算の締め日" },
  { key: "cards", q: "名刺を追加で発注" },
  { key: "room", q: "会議室の予約を取り消す" },
  { key: "monitor", q: "外部モニターに映らない" },
  { key: "duplex", q: "両面印刷する方法" },
  { key: "badge", q: "社員証をなくした" },
];

const autoRuns: Record<string, string> = {};

test("(1) 既知の質問は、本人の照会に返った FAQ を出典にして、その資料だけで自動回答する", async ({ browser }) => {
  test.setTimeout(240_000);
  const app = W.apps.main;
  const [first, ...rest] = KNOWN;

  // 1 問目は画面から質問する（カテゴリはラジオ・質問は文章の欄）。
  await bob().goto(`/apps/${app.id}`);
  await bob().getByTestId("ui-figure").first().getByRole("radio", { name: "PC", exact: true }).click();
  const ui = await pressInApp(bob(), app.id, { open: false, fill: { 質問: first.q }, button: "質問する" });
  expect(ui.actionId).toBe(app.actions["質問する"]);
  expect(ui.params).toEqual({ question: first.q, category: "PC" });
  const done: (AppRun & { run_id: string })[] = [{ ...ui.run, run_id: ui.runId }];
  // 残りは API から（同じ入口・押した本人の権限）。
  done.push(...(await Promise.all(rest.map((k) => ask(bob(), k.q, "アカウント")))));

  let matched = 0;
  for (const [i, k] of KNOWN.entries()) {
    const run = done[i];
    const f = FAQ[k.key];
    if (run.status !== "succeeded") {
      // 落ちた step を本文に出す（原因が分かるように）。
      const d = await runs.get(alice(), W.workflows.ask.id, run.run_id);
      const bad = d.steps.filter((x) => x.status === "failed").map((x) => [x.node_id, x.error?.code, x.error?.message]);
      expect(run.status, `「${k.q}」の run（${JSON.stringify(bad)}）`).toBe("succeeded");
    }
    autoRuns[k.q] = run.run_id;
    // 照会に返った FAQ（本人に読める行だけ・全社向けだけ）。
    const nos = await foundNos(run.run_id);
    expect(nos, `「${k.q}」の照会`).toEqual([f.faq_no]);
    const t = await ticket(k.q);
    expect(t.data).toMatchObject({ status: "自動回答", source: nos[0], asker: P.bob.id, question: k.q });
    // 回答（stub の echo）には、質問とその資料の本文だけが入る。
    const answer = String(t.data.answer);
    expect(answer.startsWith(echoOf(k.q)), answer).toBe(true);
    expect(answer).toContain(f.answer);
    for (const other of Object.values(FAQ).filter((x) => x.faq_no !== f.faq_no)) {
      expect(answer, `${other.faq_no} の本文は入らない`).not.toContain(other.answer);
    }
    expect(answer.replace(/\s+/g, "")).toBe(echoOf(promptOf(k.q, f)).replace(/\s+/g, ""));
    // 止まり方: 最後まで答えた（F4）。
    const d = await runs.get(alice(), W.workflows.ask.id, run.run_id);
    expect(runs.takenPorts(d, "found")).toEqual(["true"]);
    expect(runs.takenPorts(d, "ok")).toEqual(["true"]);
    const llm = await runs.stepOutput<{ stop_reason: string; text: string }>(alice(), W.workflows.ask.id, run.run_id, "answer");
    expect(llm.stop_reason).toBe("end_turn");
    // 画面の結果: 回答（success）と出典。記録の reply の step と同じ。
    expect(sections(run).map((s) => [s.label, s.tone])).toEqual([
      ["回答", "success"],
      ["出典", "neutral"],
    ]);
    expect(sections(run)[1].text).toBe(`${f.faq_no}「${f.question}」`);
    await expectSectionsMatchSteps(run, ["r_answer", "r_source"]);
    if (t.data.source === f.faq_no) matched += 1;
  }
  // 参考（精度は作る人の領分・合否に使わない）: 既知の質問のうち正しい FAQ を出典にした数。
  console.log(`APP-12 (1) 参考: 既知の質問 ${KNOWN.length} 問中 ${matched} 問が正しい出典`);

  // 画面: 回答と出典が結果の欄に出る（PC とスマホ）。
  const row = bob().getByTestId("yui-runs").locator(`li[data-run-id="${ui.runId}"]`);
  const result = row.getByTestId("yui-run-result");
  await expect(result).toContainText(FAQ[first.key].answer);
  await expect(result).toContainText(`${FAQ[first.key].faq_no}「${FAQ[first.key].question}」`);
  await shot(bob(), "app12", "auto-answer-pc");

  // bob の「自分の問い合わせ」に、今の質問がどれも出て、自分の分だけ。
  const grid = await gridRows(bob(), app.id, app.version, "自分の問い合わせ");
  expect(grid.status).toBe(200);
  expectOnlyOwn(grid.rows, "asker", P.bob.id, "bob の「自分の問い合わせ」");
  for (const k of KNOWN) expect(grid.rows.map((r) => r.data.question)).toContain(k.q);

  const phone = await phoneOf(browser, P.bob);
  try {
    await phone.goto(`/apps/${app.id}`);
    await expect(phone.getByTestId("ui-figure").getByRole("button", { name: "質問する", exact: true })).toBeVisible({ timeout: 30_000 });
    await expect(phone.getByTestId("miniapp-data-grid-自分の問い合わせ")).toBeVisible({ timeout: 30_000 });
    await noSideScroll(phone);
    await shot(phone, "app12", "bob-phone");
  } finally {
    await phone.context().close();
  }
});

let adminTicket: DataRecord | undefined;
let aliceAdminRun = "";

test("(2) 情シス向けの FAQ は bob の照会に返らず、出典にも回答にも入らない。alice には返る。下書きは承認の前の bob に見えない", async () => {
  test.setTimeout(180_000);
  const q = "管理者権限を一時的に付ける";
  const f = FAQ.admin;
  const app = W.apps.main;

  const b = await ask(bob(), q);
  expect(b.status).toBe("succeeded");
  expect(await foundNos(b.run_id), "bob の照会に情シス向けの行は返らない").toEqual([]);
  expect(sections(b).map((s) => s.tone)).toEqual(["warning"]);
  expect(JSON.stringify(b.reply)).not.toContain(SECRET_MARK);

  const a = await ask(alice(), q);
  aliceAdminRun = a.run_id;
  expect(a.status).toBe("succeeded");
  expect(await foundNos(a.run_id), "alice（情シス）の照会には返る").toEqual([f.faq_no]);
  expect(sections(a)[0].text).toContain(SECRET_MARK);

  // 行: 同じ質問の行が 2 つ（bob はエスカレーション・alice は自動回答で出典つき）。
  const both = await rows.query(alice(), W.tables.tickets.id, cmp("question", "eq", q));
  expect(both.length).toBe(2);
  const mine = both.find((r) => r.data.asker === P.bob.id)!;
  const hers = both.find((r) => r.data.asker === P.alice.id)!;
  expect(hers.data).toMatchObject({ status: "自動回答", source: f.faq_no });
  expect(String(hers.data.answer)).toContain(SECRET_MARK);
  expect(["エスカレーション", "回答案"]).toContain(mine.data.status);
  expect(mine.data.source ?? null).toBeNull();
  expect(mine.data.answer ?? null).toBeNull();

  // 引き継ぎ（alice の委譲）は情シス向けの FAQ も読んで下書きを作り、回答案で承認を待つ。
  await waitRecordStep(alice(), W.workflows.handoff.id, mine.id, "check", ["waiting_event", "waiting"]);
  adminTicket = await rows.waitForStatus(alice(), W.tables.tickets.id, mine.id, ["回答案"]);
  expect(adminTicket.data.draft_source).toBe(f.faq_no);
  expect(String(adminTicket.data.draft)).toContain(SECRET_MARK);

  // 承認の前の bob には、下書きと下書きの資料が見えない（行・表のどちらでも）。
  const seen = await call<DataRecord>(bob(), "GET", `/data/tables/${W.tables.tickets.id}/records/${mine.id}`);
  expect(seen.status).toBe(200);
  expect(seen.body.data.status).toBe("回答案");
  expect(Object.keys(seen.body.data)).not.toContain("draft");
  expect(Object.keys(seen.body.data)).not.toContain("draft_source");
  expect(seen.text).not.toContain(SECRET_MARK);
  const grid = await gridRows(bob(), app.id, app.version, "自分の問い合わせ");
  const inGrid = grid.rows.find((r) => r.id === mine.id);
  expect(inGrid, "bob の表に自分の問い合わせが出る").toBeDefined();
  expect(Object.keys(inGrid!.data)).not.toContain("draft");
  expect(JSON.stringify(grid.body)).not.toContain(SECRET_MARK);
  // bob は情シス向けの FAQ の行を直接も読めない。
  const direct = await rows.try(bob(), W.tables.faq.id);
  expect(direct.status).toBe(200);
  expect(direct.items.map((r) => r.data.faq_no)).not.toContain(f.faq_no);
  // bob は質問のワークフローとその run を読めない（アプリに束ねただけで共有していない・存在しない id と同じ答え）。
  await expectDenied(bob(), [
    { path: `/workflows/${W.workflows.ask.id}/runs`, what: "質問の run" },
    { path: `/workflows/${W.workflows.ask.id}/runs/${a.run_id}`, what: "alice の質問の run" },
    { path: `/workflows/${W.workflows.handoff.id}`, what: "エスカレーション対応" },
  ]);
});

let approvedTicket = "";

test("(3) FAQ に無い質問はエスカレーションになり、情シスの受信箱に 1 件・bob には出ない。承認で完了・差し戻しで対応中", async () => {
  test.setTimeout(240_000);
  const [ra, rr] = await Promise.all([ask(bob(), UNKNOWN.approve, "総務"), ask(bob(), UNKNOWN.ret, "総務")]);
  for (const [q, r] of [
    [UNKNOWN.approve, ra],
    [UNKNOWN.ret, rr],
  ] as const) {
    expect(r.status, q).toBe("succeeded");
    // 照会が 0 件の分岐（found の false の出口）。AI は呼ばない。
    const d = await runs.get(alice(), W.workflows.ask.id, r.run_id);
    expect(runs.takenPorts(d, "found")).toEqual(["false"]);
    expect(runs.stepStatus(d, "answer") ?? "skipped").toBe("skipped");
    expect(runs.stepStatus(d, "rec") ?? "skipped").toBe("skipped");
    expect(await foundNos(r.run_id)).toEqual([]);
    expect(sections(r).map((s) => [s.label, s.tone])).toEqual([["引き継ぎ", "warning"]]);
    expect(sections(r)[0].text).toContain("情シスに引き継ぎました");
    await expectSectionsMatchSteps(r, ["r_esc"]);
  }
  const ta = await ticket(UNKNOWN.approve);
  const tr = await ticket(UNKNOWN.ret);
  approvedTicket = ta.id;

  // 引き継ぎが下書きを作って回答案にし、情シスの受信箱に 1 件ずつ出る。bob の受信箱には出ない。
  for (const t of [ta, tr]) {
    const d = await waitRecordStep(alice(), W.workflows.handoff.id, t.id, "check", ["waiting_event", "waiting"]);
    expect(d.trigger_kind).toBe("event");
    expect(runs.takenPorts(d, "draft")).toEqual(["out"]);
  }
  const ga = await approvals.approvalFor(alice(), ta.id, APPROVAL_TITLE);
  const gr = await approvals.approvalFor(alice(), tr.id, APPROVAL_TITLE);
  const inbox = await approvals.inbox(alice());
  expect(inbox.filter((x) => x.record.id === ta.id).length, "情シスの受信箱に 1 件").toBe(1);
  expect(ga.from_state).toBe("回答案");
  const bobInbox = await approvals.inbox(bob());
  expect(bobInbox.filter((x) => [ta.id, tr.id].includes(x.record.id)), "bob の受信箱には出ない").toEqual([]);
  await expectDenied(bob(), [{ path: `/approvals/${ga.approval_id}`, what: "情シスの承認" }]);

  // 承認の見せる列に下書きが出る（FAQ に無いので資料は「なし」・stub の echo）。
  const detail = await approvalDetail(alice(), ga.approval_id);
  expect(detail.status).toBe(200);
  const fields = Object.fromEntries(detail.body.record.fields.map((x) => [x.key, x.value]));
  expect(Object.keys(fields).sort()).toEqual([...SHOW_FIELDS].sort());
  const draft = String(fields.draft);
  expect(draft.startsWith(echoOf(UNKNOWN.approve)), draft).toBe(true);
  expect(fields.draft_source).toBe("なし");
  expect(detail.body.decisions.map((x) => [x.kind, x.to])).toEqual(
    expect.arrayContaining([
      ["approve", "完了"],
      ["return", "対応中"],
    ]),
  );
  // 下書きは承認の前の bob に見えない。回答もまだ無い。
  const before = await rows.get(bob(), W.tables.tickets.id, ta.id);
  expect(before.data.draft).toBeUndefined();
  expect(before.data.answer ?? null).toBeNull();

  // 承認: 完了になり、回答に下書きが写る（出典は下書きの資料）。
  await approvals.decide(alice(), ga, "approve");
  let done: DataRecord | undefined;
  await expect
    .poll(
      async () => {
        done = await rows.get(alice(), W.tables.tickets.id, ta.id);
        return [done.data.status, done.data.answer ?? null];
      },
      { timeout: 60_000, intervals: [1_000, 2_000], message: "承認の後に回答が写る" },
    )
    .toEqual(["完了", draft]);
  expect(done!.data.source).toBe("なし");
  const hd = await waitRecordStep(alice(), W.workflows.handoff.id, ta.id, "publish", ["succeeded"]);
  expect(runs.takenPorts(hd, "check")).toEqual(["approved"]);
  expect((await runs.waitEnd(alice(), W.workflows.handoff.id, hd.run_id)).status).toBe("succeeded");

  // 差し戻し（担当者が対応する）: 対応中になり、回答は写さない。
  await approvals.decide(alice(), gr, "return");
  const back = await rows.waitForStatus(alice(), W.tables.tickets.id, tr.id, ["対応中"], { timeout: 60_000 });
  expect(back.data.answer ?? null).toBeNull();
  const hr = (await runs.waitEnd(alice(), W.workflows.handoff.id, (await waitRecordStep(alice(), W.workflows.handoff.id, tr.id, "check", ["succeeded"])).run_id));
  expect(hr.status).toBe("succeeded");
  expect(runs.takenPorts(hr, "check")).not.toContain("approved");
  expect(runs.stepStatus(hr, "publish") ?? "skipped").toBe("skipped");
  // 受信箱から消える。
  const after = await approvals.inbox(alice());
  expect(after.filter((x) => [ta.id, tr.id].includes(x.record.id))).toEqual([]);
});

test("(3) 追加の分かれ道: 答えが途中で切れた・AI が失敗したときも、run は落ちずにエスカレーションへ進む", async () => {
  test.setTimeout(180_000);
  const cutQ = FAQ.cut.question;
  const brokenQ = "プリンターのドライバーを入れ直す";
  const [rc, rb] = await Promise.all([ask(bob(), cutQ), ask(bob(), brokenQ)]);

  // 途中で切れた（stub の truncate:）: 答えは成功のまま返るが、止まり方が max_tokens なので自動回答にしない。
  expect(rc.status).toBe("succeeded");
  expect(await foundNos(rc.run_id)).toEqual([FAQ.cut.faq_no]);
  const dc = await runs.get(alice(), W.workflows.ask.id, rc.run_id);
  expect(runs.takenPorts(dc, "answer")).toEqual(["out"]);
  const cut = await runs.stepOutput<{ stop_reason: string }>(alice(), W.workflows.ask.id, rc.run_id, "answer");
  expect(cut.stop_reason).toBe("max_tokens");
  expect(runs.takenPorts(dc, "ok")).toEqual(["false"]);
  expect(runs.stepStatus(dc, "rec") ?? "skipped").toBe("skipped");
  expect(sections(rc).map((s) => s.tone)).toEqual(["warning"]);
  const tc = await ticket(cutQ);
  expect(["エスカレーション", "回答案"]).toContain(tc.data.status);
  expect(tc.data.answer ?? null).toBeNull();

  // AI が失敗した（資料の行の頭の fail:）: error の出口からエスカレーションへ。run は落ちない。
  expect(rb.status).toBe("succeeded");
  expect(await foundNos(rb.run_id)).toEqual([FAQ.broken.faq_no]);
  const db = await runs.get(alice(), W.workflows.ask.id, rb.run_id);
  expect(runs.takenPorts(db, "answer")).toEqual(["error"]);
  const code = runs.errorCode(db, "answer");
  expect(code, "AI の失敗の code").toBeTruthy();
  expect(runs.stepStatus(db, "ok") ?? "skipped").toBe("skipped");
  expect(sections(rb).map((s) => s.tone)).toEqual(["warning"]);
  const tb = await ticket(brokenQ);
  expect(["エスカレーション", "回答案"]).toContain(tb.data.status);
  // 引き継ぎの下書きも同じ資料で失敗し、担当者が書くように残す（run は承認を待つ）。
  await waitRecordStep(alice(), W.workflows.handoff.id, tb.id, "check", ["waiting_event", "waiting"]);
  const drafted = await rows.waitForStatus(alice(), W.tables.tickets.id, tb.id, ["回答案"]);
  expect(drafted.data.draft).toBe(NO_DRAFT);
  expect(drafted.data.draft_source).toBe(FAQ.broken.faq_no);
  console.log(`APP-12 (3) 追加: AI の失敗の code = ${code}`);
});

test("(4) 自動回答の結果は bob の記録にだけ出る。エスカレーションの回答は、承認の後に開き直した表に入っている", async () => {
  test.setTimeout(180_000);
  const app = W.apps.main;
  const bobRun = autoRuns[KNOWN[0].q];
  expect(bobRun, "(1) の run").toBeTruthy();
  // bob の記録には自動回答の run と結果がある。alice の記録には bob の run が無く、詳細も読めない（存在を区別しない）。
  const bobList = await call<{ runs: { run_id?: string; has_reply?: boolean }[] }>(bob(), "GET", `/mini-apps/${app.id}/runs`);
  expect(bobList.status).toBe(200);
  const listed = bobList.body.runs.find((r) => r.run_id === bobRun);
  expect(listed, "bob の記録に自動回答の run がある").toBeDefined();
  const aliceList = await call<{ runs: { run_id?: string }[] }>(alice(), "GET", `/mini-apps/${app.id}/runs`);
  expect(aliceList.status).toBe(200);
  const aliceIds = aliceList.body.runs.map((r) => r.run_id);
  for (const id of Object.values(autoRuns)) expect(aliceIds).not.toContain(id);
  expect(bobList.body.runs.map((r) => r.run_id)).not.toContain(aliceAdminRun);
  await expectStatus(alice(), [{ path: `/mini-apps/${app.id}/runs/${bobRun}`, what: "bob の run" }]);
  await expectStatus(bob(), [{ path: `/mini-apps/${app.id}/runs/${aliceAdminRun}`, what: "alice の run" }]);

  // エスカレーションの回答: 承認の後に bob が開き直すと、表の行に回答と「完了」が入っている。
  expect(approvedTicket, "(3) で承認した問い合わせ").toBeTruthy();
  const t = await rows.get(alice(), W.tables.tickets.id, approvedTicket);
  // 表の中身はキャンバスに描くので、画面と同じ入口（data_grid の別名・bob の権限）で読む。
  const grid = await gridRows(bob(), app.id, app.version, "自分の問い合わせ");
  expect(grid.status).toBe(200);
  const row = grid.rows.find((r) => r.id === approvedTicket);
  expect(row?.data).toMatchObject({ status: "完了", answer: t.data.answer, question: UNKNOWN.approve });
  expect(Object.keys(row!.data)).not.toContain("draft");
  expectOnlyOwn(grid.rows, "asker", P.bob.id, "bob の「自分の問い合わせ」");
  await bob().goto(`/apps/${app.id}`);
  const caption = bob().getByTestId("miniapp-data-grid-自分の問い合わせ").locator("figcaption");
  await expect(caption).toContainText(`${grid.rows.length} 件`, { timeout: 30_000 });
  await shot(bob(), "app12", "escalated-answer-pc");
});

test("（追加）FAQ の本文のボタンの図は結果でボタンにならず、外部のリンクにはホスト名が付く。step は IR のノードだけ", async () => {
  test.setTimeout(180_000);
  const app = W.apps.main;
  const q = "業務用のソフトの導入を頼む";
  await bob().goto(`/apps/${app.id}`);
  await bob().getByTestId("ui-figure").first().getByRole("radio", { name: "PC", exact: true }).click();
  const r = await pressInApp(bob(), app.id, { open: false, fill: { 質問: q }, button: "質問する" });
  expect(r.run.status).toBe("succeeded");
  expect(await foundNos(r.runId)).toEqual([FAQ.install.faq_no]);
  const t = await ticket(q);
  expect(t.data.status).toBe("自動回答");
  // 記録の回答には FAQ の本文（図のフェンス）が文字のまま入る。画面の結果では図のフェンスを表示専用にする（D-28）。
  expect(String(t.data.answer)).toContain("```ui");
  const md = sections(r.run)[0];
  expect(md.format).toBe("markdown");
  expect(md.text).not.toContain("```ui");
  expect(md.text).toContain("```text");
  const result = bob().getByTestId("yui-runs").locator(`li[data-run-id="${r.runId}"]`).getByTestId("yui-run-result");
  await expect(result).toBeVisible();
  await expect(result.getByRole("button", { name: "承認する" })).toHaveCount(0);
  await expect(result).toContainText("申請の手引き（manual.example.com）");
  await expect(result.getByRole("link", { name: "申請の手引き" })).toHaveAttribute("target", "_blank");
  await result.scrollIntoViewIfNeeded();
  await shot(bob(), "app12", "injection-pc");
  // AI は道具を持たないので、run の step は IR の決まったノードだけ。
  const d = await runs.get(alice(), W.workflows.ask.id, r.runId);
  for (const s of d.steps) expect(ASK_NODES, `step ${s.node_id}`).toContain(s.node_id);
});

test("権限: charlie はどれも読めず存在も分からない", async () => {
  test.setTimeout(120_000);
  const runIds = Object.values(autoRuns);
  await expectOutsiderSeesNothing(P.charlie.page, W, {
    workflowRuns: [["ask", runIds[0]]],
    appRuns: [
      ["main", runIds[0]],
      ["main", aliceAdminRun],
    ],
  });
  if (adminTicket) {
    await expectDenied(P.charlie.page, [
      { path: `/data/tables/${W.tables.tickets.id}/records/${adminTicket.id}`, what: "問い合わせの行" },
    ]);
  }
});
