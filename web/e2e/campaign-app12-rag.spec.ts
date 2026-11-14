import { expect, test, type Page } from "@playwright/test";

import {
  APPROVAL_TITLE,
  ASK_RAG_NODES,
  DOC_KEYS,
  FAQ,
  SECRET_MARK,
  SHOW_FIELDS,
  UNKNOWN,
  app12RagWorld,
  approvalDetail,
  docMark,
  ragEnabled,
  waitIndexed,
  waitRecordStep,
} from "./campaign/apps/app12";
import { expectDenied, expectOnlyOwn, expectOutsiderSeesNothing, expectStatus } from "./campaign/checks";
import { call } from "./campaign/http";
import { people, type People } from "./campaign/people";
import { gridRows, pressApiDone, pressInApp, type AppRun, type ReplySection } from "./campaign/press";
import { shot } from "./campaign/shots";
import { echoOf } from "./campaign/stub";
import { approvals, cmp, expectSingleRow, rows, runs, type DataRecord } from "./campaign/truth";
import { buildWorld, cleanupWorld, type World } from "./campaign/world";

/// APP-12 社内 FAQ ヘルプデスク（台帳 §7.13 の APP-12・V2: ドライブの FAQ の文書を rag.search で探す版）。社内文書の
/// 検索（取り込みの worker・Qdrant）が要るので、検索が無い構成（CI の web-e2e・手元の検証のスタック）では skip する
/// （`/api/search` が 503・workflow-storage-read-document.spec.ts と同じ見分け方・D-226 (4)）。LLM は stub。
/// 成功判定の言い換え（§7.13）:
/// (1) 既知の質問は「自動回答」で、出典のファイル（source_file）はその run の検索で本人に返った資料（関連の強いもの）の
///     先頭。回答（stub の echo）には選んだ資料の本文だけが入る。bob の画面に回答と出典のリンクが出る。
/// (2) 機密の文書（alice の私用のフォルダー）は bob の検索の出力に無く、回答に目印が無い。alice の検索には出る。
///     承認の前は bob に下書きが見えない。bob は質問のワークフローとその run を読めない。
/// (3) 関連の強い資料が無い質問はエスカレーションになり、情シスの受信箱に 1 件・bob には出ない。引き継ぎは委譲した
///     FAQ のフォルダーだけを検索して下書きを作る（ワークフロー主体の rag.search・初めて通す道）。承認で完了・差し戻しで
///     対応中。
/// (4) 結果は押した本人の記録にだけ出る。承認の後に開き直した表に回答が入っている。
/// （追加）文書の中のボタンの図は結果でボタンにならず、外部のリンクにはホスト名が付く。
/// 範囲の外: rag.search のフォルダーの範囲（D-226 (6)・押した人が読める全文書から探す）・通知（GAP-06）。

test.describe.configure({ mode: "serial" });

let P: People;
let W: World;

test.beforeAll(async ({ browser }) => {
  test.setTimeout(600_000);
  P = await people(browser);
  test.skip(!(await ragEnabled(P.alice.page)), "社内文書の検索が無い構成（RAG の worker を立てた構成でだけ走る）");
  W = await buildWorld(P, app12RagWorld());
  // 取り込みが終わるまで待つ（目印で検索に出る）。
  for (const k of [...DOC_KEYS, "secret"]) await waitIndexed(P.alice.page, W.fill(docMark(k)), W.files[k].id);
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

type Result = { file_id: string; file_name: string; content: string; relevance: string };
type Pick = { count: number; text: string; names: string; first: string | null; sources: { file_id: string; file_name: string }[] };

async function ask(page: Page, question: string, category = "PC"): Promise<AppRun & { run_id: string }> {
  const app = W.apps.main;
  return pressApiDone(page, app.id, { version: app.version, actionId: app.actions["質問する"], params: { question, category } });
}

async function step<T>(runId: string, node: string): Promise<T> {
  return runs.stepOutput<T>(alice(), W.workflows.ask.id, runId, node);
}

function sections(run: AppRun): ReplySection[] {
  return run.reply?.sections ?? [];
}

/// 目印（どの文書の本文が入ったか）。
function marks(text: string): string[] {
  return [...DOC_KEYS, "secret"].filter((k) => text.includes(W.fill(docMark(k))));
}

const KNOWN: { key: (typeof DOC_KEYS)[number]; q: string }[] = [
  { key: "vpn", q: "社外から VPN につながらない" },
  { key: "password", q: "パスワードを忘れたときの再設定" },
  { key: "closing", q: "経費の精算の締め日" },
  { key: "duplex", q: "複合機で両面印刷したい" },
];

const autoRuns: string[] = [];
let aliceRun = "";
let approvedTicket = "";

test("(1) 既知の質問は、本人の検索に返った関連の強い資料を出典にして自動回答する", async () => {
  test.setTimeout(300_000);
  const done = await Promise.all(KNOWN.map((k) => ask(bob(), k.q)));
  let right = 0;
  for (const [i, k] of KNOWN.entries()) {
    const run = done[i];
    expect(run.status, k.q).toBe("succeeded");
    autoRuns.push(run.run_id);
    const search = await step<{ results: Result[] }>(run.run_id, "search");
    const pick = await step<Pick>(run.run_id, "pick");
    const strong = search.results.filter((r) => r.relevance === "high" || r.relevance === "medium").map((r) => r.file_id);
    expect(pick.count, `「${k.q}」の関連の強い資料`).toBeGreaterThan(0);
    const t = await expectSingleRow(alice(), W.tables.tickets.id, cmp("question", "eq", k.q), k.q);
    expect(t.data).toMatchObject({ status: "自動回答", source_file: pick.first, asker: P.bob.id });
    expect(strong).toContain(t.data.source_file);
    // 回答（stub の echo）は質問と選んだ資料の本文だけ。機密の目印は入らない。
    const answer = String(t.data.answer);
    expect(answer.startsWith(echoOf(k.q))).toBe(true);
    const picked = new Set(pick.sources.map((s) => Object.entries(W.files).find(([, f]) => f.id === s.file_id)?.[0]));
    for (const m of marks(answer)) expect(picked, `回答に入った ${m} は選んだ資料`).toContain(m);
    expect(answer).not.toContain(SECRET_MARK);
    // 画面の結果: 回答と、出典のファイルへのリンク（選んだ資料）。
    expect(sections(run).map((s) => [s.label, s.format])).toEqual([
      ["回答", "markdown"],
      ["出典", "file_link"],
    ]);
    expect(sections(run)[1].files?.map((f) => f.id)).toEqual(pick.sources.map((s) => s.file_id));
    if (t.data.source_file === W.files[k.key].id) right += 1;
  }
  console.log(`APP-12 V2 (1) 参考: 既知の質問 ${KNOWN.length} 問中 ${right} 問が正しい文書を出典にした`);
  const grid = await gridRows(bob(), W.apps.main.id, W.apps.main.version, "自分の問い合わせ");
  expectOnlyOwn(grid.rows, "asker", P.bob.id, "bob の「自分の問い合わせ」");
});

test("(2) 機密の文書は bob の検索に出ず、回答に入らない。alice には出る。下書きは承認の前の bob に見えない", async () => {
  test.setTimeout(300_000);
  const q = "管理者権限を一時的に付ける手順";
  const secret = W.files.secret.id;
  const b = await ask(bob(), q);
  expect(b.status).toBe("succeeded");
  const bs = await step<{ results: Result[] }>(b.run_id, "search");
  expect(bs.results.map((r) => r.file_id), "bob の検索に機密の文書は出ない").not.toContain(secret);
  expect(JSON.stringify(b.reply)).not.toContain(SECRET_MARK);
  // bob の検索の結果は、どれも bob が読めるファイル。
  for (const r of bs.results) expect((await call(bob(), "GET", `/files/${r.file_id}/text`)).status, r.file_name).toBe(200);

  const a = await ask(alice(), q);
  aliceRun = a.run_id;
  expect(a.status).toBe("succeeded");
  const as = await step<{ results: Result[] }>(a.run_id, "search");
  expect(as.results.map((r) => r.file_id), "alice（持ち主）の検索には出る").toContain(secret);

  const mine = (await rows.query(alice(), W.tables.tickets.id, cmp("question", "eq", q))).find((r) => r.data.asker === P.bob.id)!;
  expect(String(mine.data.answer ?? "")).not.toContain(SECRET_MARK);
  if (mine.data.status !== "自動回答") {
    // 引き継ぎ（委譲した FAQ のフォルダーだけを探す）の下書きは、承認の前の bob に見えない。
    const h = await waitRecordStep(alice(), W.workflows.handoff.id, mine.id, "check", ["waiting_event", "waiting"], 120_000);
    const seen = await call<DataRecord>(bob(), "GET", `/data/tables/${W.tables.tickets.id}/records/${mine.id}`);
    expect(seen.status).toBe(200);
    expect(Object.keys(seen.body.data)).not.toContain("draft");
    expect(Object.keys(seen.body.data)).not.toContain("draft_source");
    const look = await runs.stepOutput<{ results: Result[] }>(alice(), W.workflows.handoff.id, h.run_id, "look");
    expect(look.results.map((r) => r.file_id), "引き継ぎは委譲したフォルダーの外を探さない").not.toContain(secret);
  }
  await expectDenied(bob(), [
    { path: `/workflows/${W.workflows.ask.id}/runs`, what: "質問の run" },
    { path: `/workflows/${W.workflows.ask.id}/runs/${a.run_id}`, what: "alice の質問の run" },
  ]);
});

test("(3) 関連の強い資料が無い質問はエスカレーションになり、情シスが承認すると完了・差し戻すと対応中", async () => {
  test.setTimeout(300_000);
  const [ra, rr] = await Promise.all([ask(bob(), UNKNOWN.approve, "総務"), ask(bob(), UNKNOWN.ret, "総務")]);
  for (const r of [ra, rr]) {
    expect(r.status).toBe("succeeded");
    const d = await runs.get(alice(), W.workflows.ask.id, r.run_id);
    expect(runs.takenPorts(d, "found")).toEqual(["false"]);
    expect((await step<Pick>(r.run_id, "pick")).count).toBe(0);
    expect(sections(r).map((s) => s.tone)).toEqual(["warning"]);
  }
  const ta = await expectSingleRow(alice(), W.tables.tickets.id, cmp("question", "eq", UNKNOWN.approve));
  const tr = await expectSingleRow(alice(), W.tables.tickets.id, cmp("question", "eq", UNKNOWN.ret));
  approvedTicket = ta.id;
  for (const t of [ta, tr]) {
    const d = await waitRecordStep(alice(), W.workflows.handoff.id, t.id, "check", ["waiting_event", "waiting"], 120_000);
    expect(d.trigger_kind).toBe("event");
    expect(runs.stepStatus(d, "look")).toBe("succeeded");
  }
  const ga = await approvals.approvalFor(alice(), ta.id, APPROVAL_TITLE);
  const gr = await approvals.approvalFor(alice(), tr.id, APPROVAL_TITLE);
  expect((await approvals.inbox(bob())).filter((x) => [ta.id, tr.id].includes(x.record.id))).toEqual([]);
  const detail = await approvalDetail(alice(), ga.approval_id);
  const fields = Object.fromEntries(detail.body.record.fields.map((x) => [x.key, x.value]));
  expect(Object.keys(fields).sort()).toEqual([...SHOW_FIELDS].sort());
  const draft = String(fields.draft);
  expect(draft.startsWith(echoOf(UNKNOWN.approve))).toBe(true);
  expect(draft).not.toContain(SECRET_MARK);

  await approvals.decide(alice(), ga, "approve");
  await expect
    .poll(async () => {
      const r = await rows.get(alice(), W.tables.tickets.id, ta.id);
      return [r.data.status, r.data.answer ?? null];
    }, { timeout: 60_000 })
    .toEqual(["完了", draft]);
  await approvals.decide(alice(), gr, "return");
  const back = await rows.waitForStatus(alice(), W.tables.tickets.id, tr.id, ["対応中"], { timeout: 60_000 });
  expect(back.data.answer ?? null).toBeNull();
});

test("(4) 結果は押した本人の記録にだけ出る。承認の後に開き直した表に回答が入っている", async () => {
  test.setTimeout(180_000);
  const app = W.apps.main;
  await expectStatus(alice(), [{ path: `/mini-apps/${app.id}/runs/${autoRuns[0]}`, what: "bob の run" }]);
  await expectStatus(bob(), [{ path: `/mini-apps/${app.id}/runs/${aliceRun}`, what: "alice の run" }]);
  const aliceList = await call<{ runs: { run_id?: string }[] }>(alice(), "GET", `/mini-apps/${app.id}/runs`);
  for (const id of autoRuns) expect(aliceList.body.runs.map((r) => r.run_id)).not.toContain(id);
  const grid = await gridRows(bob(), app.id, app.version, "自分の問い合わせ");
  const row = grid.rows.find((r) => r.id === approvedTicket);
  expect(row?.data.status).toBe("完了");
  expect(String(row?.data.answer ?? "")).not.toBe("");
  await bob().goto(`/apps/${app.id}`);
  await expect(bob().getByTestId("miniapp-data-grid-自分の問い合わせ").locator("figcaption")).toContainText(`${grid.rows.length} 件`, {
    timeout: 30_000,
  });
  await shot(bob(), "app12rag", "escalated-answer-pc");
});

test("（追加）文書の中のボタンの図は結果でボタンにならず、外部のリンクにはホスト名が付く", async () => {
  test.setTimeout(180_000);
  const app = W.apps.main;
  await bob().goto(`/apps/${app.id}`);
  await bob().getByTestId("ui-figure").first().getByRole("radio", { name: "PC", exact: true }).click();
  const r = await pressInApp(bob(), app.id, { open: false, fill: { 質問: FAQ.install.question }, button: "質問する" });
  expect(r.run.status).toBe("succeeded");
  const d = await runs.get(alice(), W.workflows.ask.id, r.runId);
  for (const s of d.steps) expect(ASK_RAG_NODES).toContain(s.node_id);
  const result = bob().getByTestId("yui-runs").locator(`li[data-run-id="${r.runId}"]`).getByTestId("yui-run-result");
  await result.scrollIntoViewIfNeeded();
  await expect(result.getByRole("button", { name: "承認する" })).toHaveCount(0);
  const md = sections(r.run)[0];
  if (md.format === "markdown" && String(md.text).includes("申請の手引き")) {
    expect(md.text).not.toContain("```ui");
    await expect(result).toContainText("申請の手引き（manual.example.com）");
  }
  await shot(bob(), "app12rag", "injection-pc");
});

test("権限: charlie はどれも読めず存在も分からない", async () => {
  test.setTimeout(120_000);
  await expectOutsiderSeesNothing(P.charlie.page, W, {
    workflowRuns: [["ask", autoRuns[0]]],
    appRuns: [["main", autoRuns[0]]],
  });
});
