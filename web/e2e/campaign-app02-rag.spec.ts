import { expect, test, type Page } from "@playwright/test";

import { RAG_DOCS, app02RagWorld, ragMark } from "./campaign/apps/app02";
import { ragEnabled, waitIndexed } from "./campaign/apps/app12";
import { expectOutsiderSeesNothing } from "./campaign/checks";
import { call } from "./campaign/http";
import { people, type People } from "./campaign/people";
import { pressApiDone, type AppRun } from "./campaign/press";
import { shot } from "./campaign/shots";
import { rows, runs } from "./campaign/truth";
import { buildWorld, cleanupWorld, type World } from "./campaign/world";

/// APP-02 問い合わせの一次仕分け・(4) 下書きに社内 FAQ の出典が付く（台帳 §7.13 の APP-02）。社内文書の検索（取り込みの
/// worker・Qdrant）が要るので、検索が無い構成（CI の web-e2e・手元の検証のスタック）では skip する
/// （構成で切っているかで見分け、有効なのに検索が答えなければ skip せずに落とす・apps/app12.ts の ragEnabled・D-226 (4)）。
/// LLM は stub（下書きは prompt の折り返し）。成功判定の言い換え:
/// (4) 付く出典は、押した人が読める rag.search の結果だけ。faq の step 出力の results の file_id は bob に読める文書だけで、
///     alice だけの社内メモが無い。行の下書きの出典（draft_sources）と結果の出典のリンクは、その部分集合。下書き（stub の
///     echo）に抜粋が入っている。alice が同じ問い合わせを押すと、社内メモも検索に出る（人で結果が変わる）。
/// 範囲の外: rag.search のフォルダーの範囲（D-226 (6)。interactive の run は押した人が読める全文書から探す）。

test.describe.configure({ mode: "serial" });

let P: People;
let W: World;

test.beforeAll(async ({ browser }) => {
  test.setTimeout(600_000);
  P = await people(browser);
  test.skip(!(await ragEnabled(P.alice.page)), "社内文書の検索が無い構成（RAG の worker を立てた構成でだけ走る）");
  W = await buildWorld(P, app02RagWorld());
  for (const k of Object.keys(RAG_DOCS) as (keyof typeof RAG_DOCS)[]) {
    await waitIndexed(P.alice.page, W.fill(ragMark(k)), W.files[k].id);
  }
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

type Result = { file_id: string; file_name: string; content: string };
type Src = { count: number; excerpts: string; names: string; files: { file_id: string; file_name: string }[] };

const INQUIRY = {
  sender: "つばめ物流",
  subject: "請求書の再発行",
  body: "billing の相談です。先月の請求書の再発行をお願いできますか。宛名も変えたいです。",
};

async function submit(page: Page): Promise<AppRun & { run_id: string }> {
  const app = W.apps.main;
  return pressApiDone(page, app.id, { version: app.version, actionId: app.actions["仕分ける"], params: INQUIRY });
}

test("(4) 下書きの出典は、押した人が読める社内 FAQ の検索の結果だけ（alice だけの社内メモは出ない）", async () => {
  test.setTimeout(300_000);
  const wf = W.workflows.triage.id;
  const memo = W.files.memo.id;
  const run = await submit(bob());
  expect(run.status).toBe("succeeded");
  const detail = await runs.get(alice(), wf, run.run_id);
  expect(runs.takenPorts(detail, "faq")).toEqual(["out"]);
  const faq = await runs.stepOutput<{ results: Result[] }>(alice(), wf, run.run_id, "faq");
  const found = faq.results.map((r) => r.file_id);
  expect(found.length, "社内 FAQ が検索に出る").toBeGreaterThan(0);
  expect(found, "alice だけの社内メモは bob の検索に出ない").not.toContain(memo);
  // 検索の結果は、どれも bob が読めるファイル。
  for (const r of faq.results) expect((await call(bob(), "GET", `/files/${r.file_id}/text`)).status, r.file_name).toBe(200);

  // 出典（行の draft_sources・結果のリンク）は検索の結果の部分集合。下書き（stub の echo）に抜粋が入る。
  const src = await runs.stepOutput<Src>(alice(), wf, run.run_id, "src");
  for (const f of src.files) expect(found).toContain(f.file_id);
  const created = await runs.stepOutput<{ id: string }>(alice(), wf, run.run_id, "create");
  const rec = await rows.get(alice(), W.tables.inquiries.id, created.id);
  const names = String(rec.data.draft_sources ?? "").split("、").filter(Boolean);
  for (const n of names) expect(faq.results.map((r) => r.file_name)).toContain(n);
  expect(String(rec.data.draft_reply)).toContain(INQUIRY.body);
  expect(String(rec.data.draft_reply)).toContain(faq.results[0].content.slice(0, 20));
  expect(String(rec.data.draft_reply)).not.toContain(W.fill(ragMark("memo")));
  const link = (run.reply?.sections ?? []).find((s) => s.format === "file_link");
  expect(link, "結果に出典のリンクがある").toBeDefined();
  for (const f of link!.files ?? []) expect(found).toContain(f.id);

  // alice（社内メモの持ち主）が同じ問い合わせを押すと、社内メモも検索に出る（人で結果が変わる）。
  const a = await submit(alice());
  const af = await runs.stepOutput<{ results: Result[] }>(alice(), wf, a.run_id, "faq");
  expect(af.results.map((r) => r.file_id)).toContain(memo);

  await bob().goto(`/apps/${W.apps.main.id}`);
  const row = bob().getByTestId("yui-runs").locator(`li[data-run-id="${run.run_id}"]`);
  await row.scrollIntoViewIfNeeded();
  if ((await row.getByTestId("yui-run-result").count()) === 0) await row.getByRole("button", { name: "結果" }).click();
  await shot(bob(), "app02rag", "sources-pc");

  await expectOutsiderSeesNothing(P.charlie.page, W, { workflowRuns: [["triage", run.run_id]], appRuns: [["main", run.run_id]] });
});
