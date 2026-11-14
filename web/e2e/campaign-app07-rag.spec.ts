import { expect, test, type Page } from "@playwright/test";

import {
  CONTRACTS,
  REVIEW,
  app07RagWorld,
  docxOf,
  expectedRisk,
  fixtureText,
  paragraphsOf,
  sweepReports,
  type CheckInput,
} from "./campaign/apps/app07";
import { ragEnabled, waitIndexed } from "./campaign/apps/app05";
import { expectOutsiderSeesNothing } from "./campaign/checks";
import { call } from "./campaign/http";
import { people, type People } from "./campaign/people";
import { pressApiDone, type AppRun } from "./campaign/press";
import { jsonLine } from "./campaign/stub";
import { cmp, rows, runs, type DataRecord } from "./campaign/truth";
import { buildWorld, cleanupWorld, uploadFile, type World } from "./campaign/world";

/// APP-07 契約書の条項チェック・Word の契約書と文書のチェックリストの版（台帳 §7.13 の APP-07）。Word の本文は社内文書の
/// 検索に取り込まれた本文を読み（storage.read・D-111）、チェックリストはドライブの文書を rag.search で引くので、検索が
/// 無い構成（CI の web-e2e・手元の検証のスタック）では skip する
/// （構成で切っているかで見分け、有効なのに検索が答えなければ skip せずに落とす・apps/app12.ts の ragEnabled・D-226 (4)）。
/// LLM は stub
/// （形を指定した llm.invoke・駆動の語は入力の「気になる点」にだけ置く）。
/// 確かめること:
/// - (3) 法務だけが見られるフォルダーのチェックリスト文書の file_id が、bob の rag.search の step 出力に無く、alice の出力には
///   ある。bob の指摘に法務のみの項目（nda_floor）が無く、alice の指摘にはある。charlie はアプリも表も読めない。
/// - (1)(2) Word の本文から読んだ契約で、全社の項目ごとに指摘がちょうど 1 行。本文にある引用は「確認済」、本文に無い引用は
///   「確かめられない」で要確認に落ちる。リスクは判定 × 重要度の決まった規則どおり。
/// 範囲の外: rag.search のフォルダーの範囲（D-226 (6)。interactive の run は押した人が読める全文書から探すので、検索の語に
/// チェックリストの照合の印を入れ、項目は「種別 / key / 重要度 / 項目 / 観点」の行だけを読む）。

test.describe.configure({ mode: "serial" });

let P: People;
let W: World;
let contractId = "";

test.beforeAll(async ({ browser }) => {
  test.setTimeout(600_000);
  P = await people(browser);
  test.skip(!(await ragEnabled(P.alice.page)), "社内文書の検索が無い構成（RAG の worker を立てた構成でだけ走る）");
  for (const p of [P.alice.page, P.bob.page]) await sweepReports(p);
  W = await buildWorld(P, app07RagWorld());
  const mark = `ctr${W.uniq}`;
  const docx = await docxOf([...paragraphsOf(fixtureText(CONTRACTS.nda.file)), `整理番号: ${mark}`]);
  contractId = await uploadFile(
    P.alice.page,
    W.folders.inbox.id,
    "契約書-nda.docx",
    docx,
    "application/vnd.openxmlformats-officedocument.wordprocessingml.document",
  );
  W.defer(async () => {
    await call(P.alice.page, "DELETE", `/files/${contractId}`);
    for (const r of await rows.query(P.alice.page, W.tables.reviews.id)) {
      const id = String(r.data.report ?? "");
      if (id) for (const p of [P.bob.page, P.alice.page]) await call(p, "DELETE", `/files/${id}`);
    }
  });
  await waitIndexed(P.alice.page, mark, contractId);
  for (const k of ["list_all", "list_legal"]) await waitIndexed(P.alice.page, `ckl${W.uniq}`, W.files[k].id);
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
const wf = () => W.workflows.check.id;

const Q = {
  ndaCap: "相手方に生じた一切の損害を賠償する",
  ndaRenewal: "期間が満了したときは、自動的に同一の条件で1年間更新され、以後も同様とする",
  fabricated: "秘密保持の義務は、本契約の終了後3年間存続する",
};

async function check(page: Page): Promise<AppRun & { run_id: string }> {
  const app = W.apps.main;
  const note = jsonLine({
    findings: [
      { item_key: "nda_cap", verdict: "無", quote: Q.ndaCap },
      { item_key: "nda_renewal", verdict: "要注意", quote: Q.ndaRenewal },
      { item_key: "nda_term", verdict: "有", quote: Q.fabricated },
      { item_key: "nda_floor", verdict: "要注意", quote: Q.ndaCap },
    ],
    summary: "損害賠償の上限が無く、更新の条項に注意が要ります。",
  });
  const params: CheckInput = { contract_type: "NDA", counterparty: CONTRACTS.nda.counterparty, contract_file: contractId, note };
  const run = await pressApiDone(page, app.id, { version: app.version, actionId: app.actions["AI チェック"], params });
  expect(run.status, JSON.stringify(run)).toBe("succeeded");
  return run;
}

async function findingsOf(runId: string): Promise<{ review: DataRecord; findings: DataRecord[] }> {
  const saved = await runs.stepOutput<{ id: string }>(alice(), wf(), runId, "save");
  const review = await rows.get(alice(), W.tables.reviews.id, saved.id);
  return { review, findings: await rows.query(alice(), W.tables.findings.id, cmp("review_id", "eq", saved.id)) };
}

test("(3)(1)(2) 法務だけのチェックリスト文書は依頼者の検索に出ず、Word の本文の引用を確かめて項目ごとに 1 行の指摘を残す", async () => {
  test.setTimeout(600_000);
  const legalDoc = W.files.list_legal.id;
  const allDoc = W.files.list_all.id;
  const [b, a] = [await check(bob()), await check(alice())];

  type Search = { results: { file_id: string; content: string }[] };
  const bobSearch = await runs.stepOutput<Search>(alice(), wf(), b.run_id, "search");
  const aliceSearch = await runs.stepOutput<Search>(alice(), wf(), a.run_id, "search");
  expect(bobSearch.results.map((r) => r.file_id), "bob の検索に全社のチェックリストが出る").toContain(allDoc);
  expect(bobSearch.results.map((r) => r.file_id), "bob の検索に法務だけの文書が出ない").not.toContain(legalDoc);
  expect(aliceSearch.results.map((r) => r.file_id), "alice の検索には法務だけの文書が出る").toContain(legalDoc);

  // Word の本文が届いた（取り込まれた本文・切れていない）。
  const read = await runs.stepOutput<{ text: string; truncated: boolean }>(alice(), wf(), b.run_id, "read");
  expect(read.truncated).toBe(false);
  expect(read.text.normalize("NFKC")).toContain("秘密保持契約書");

  const bobRes = await findingsOf(b.run_id);
  const aliceRes = await findingsOf(a.run_id);
  const keys = (r: { findings: DataRecord[] }) => r.findings.map((f) => String(f.data.item_key)).sort();
  expect(keys(bobRes)).toEqual(["nda_cap", "nda_law", "nda_renewal", "nda_term"]);
  expect(keys(aliceRes)).toEqual(["nda_cap", "nda_floor", "nda_law", "nda_renewal", "nda_term"]);
  const byKey = Object.fromEntries(bobRes.findings.map((f) => [String(f.data.item_key), { verdict: f.data.verdict, quote_check: f.data.quote_check }]));
  expect(byKey).toEqual({
    nda_cap: { verdict: "無", quote_check: "確認済" },
    nda_law: { verdict: REVIEW, quote_check: "なし" },
    nda_renewal: { verdict: "要注意", quote_check: "確認済" },
    nda_term: { verdict: REVIEW, quote_check: "確かめられない" },
  });
  for (const r of [bobRes, aliceRes]) {
    const rule = expectedRisk(r.findings.map((f) => ({ verdict: String(f.data.verdict), severity: String(f.data.severity) })));
    expect(r.review.data.risk).toBe(rule);
    expect(r.review.data.risk).toBe("高");
  }

  await expectOutsiderSeesNothing(P.charlie.page, W, { workflowRuns: [["check", b.run_id]], appRuns: [["main", b.run_id]] });
});
