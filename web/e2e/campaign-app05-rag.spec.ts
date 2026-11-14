import { expect, test } from "@playwright/test";

import { trashFileApi } from "./helpers";
import {
  PASS_SCORE,
  PEOPLE,
  REQUIREMENTS,
  app05World,
  expectedScore,
  ragEnabled,
  resumeText,
  waitIndexed,
  type Applicant,
} from "./campaign/apps/app05";
import { expectOutsiderSeesNothing } from "./campaign/checks";
import { call } from "./campaign/http";
import { people, type People } from "./campaign/people";
import { pressApiDone } from "./campaign/press";
import { jsonLine } from "./campaign/stub";
import { audit, cmp, expectSingleRow, rows, runs } from "./campaign/truth";
import { buildWorld, cleanupWorld, uploadFile, type World } from "./campaign/world";

/// APP-05 採用応募の書類スクリーニング・PDF の履歴書の版（台帳 §7.13 の APP-05）。PDF の本文は社内文書の検索に取り込まれた
/// 本文を読む（storage.read・D-111）ので、検索が無い構成（CI の web-e2e・手元の検証のスタック）では skip する
/// （構成で切っているかで見分け、有効なのに検索が答えなければ skip せずに落とす・apps/app12.ts の ragEnabled・D-226 (4)）。
/// LLM は stub（形を指定した llm.invoke・駆動の語は入力の志望動機にだけ置く）。
/// 確かめること（CI の版 campaign-app05.spec.ts の (1)(4) を PDF で）:
/// - PDF の履歴書の本文が「履歴書を読む」に届き（種類は PDF・切れていない）、伏せる script を通った後の本文と指示の文に
///   氏名・連絡先・住所・生年月日が無い（prompt の $template に入る step 出力・(4)(a)）。
/// - 点数は検証済みの構造化出力から決まった計算で出て、要件の key ごとに判定が 1 つ（(1)）。行の履歴書の列は PDF の id。
/// - PDF の監査に氏名と本文が残らない（(4)(c)）。charlie はどれも読めない。
/// PDF は Chromium の印刷（page.pdf）で、.md の履歴書から作る（文字の情報〔ToUnicode〕が付くので本文を取り出せる）。

test.describe.configure({ mode: "serial" });

let P: People;
let W: World;
let pdfId = "";

/// .md の履歴書を、印刷できる HTML にする（見出し・箇条書き・段落）。
function resumeHtml(md: string, mark: string): string {
  const esc = (s: string) => s.replace(/&/g, "&amp;").replace(/</g, "&lt;").replace(/>/g, "&gt;");
  const body = md
    .split("\n")
    .map((line) => {
      if (line.startsWith("# ")) return `<h1>${esc(line.slice(2))}</h1>`;
      if (line.startsWith("## ")) return `<h2>${esc(line.slice(3))}</h2>`;
      if (line.startsWith("- ")) return `<p>・${esc(line.slice(2))}</p>`;
      return line.trim() ? `<p>${esc(line)}</p>` : "";
    })
    .join("\n");
  return `<!doctype html><html lang="ja"><head><meta charset="utf-8"><style>
body { font-family: "Noto Sans CJK JP", "Noto Sans JP", sans-serif; font-size: 12pt; margin: 24mm; }
h1 { font-size: 18pt; } h2 { font-size: 14pt; margin-top: 12pt; } p { margin: 4pt 0; }
</style></head><body>${body}<p>書類の整理番号: ${esc(mark)}</p></body></html>`;
}

test.beforeAll(async ({ browser }) => {
  test.setTimeout(600_000);
  P = await people(browser);
  test.skip(!(await ragEnabled(P.alice.page)), "社内文書の検索が無い構成（RAG の worker を立てた構成でだけ走る）");
  W = await buildWorld(P, app05World());
  const mark = `rsm${W.uniq}`;
  const printer = await P.alice.ctx.newPage();
  const pdf = await printer
    .setContent(resumeHtml(resumeText(PEOPLE.sorano.file), mark), { waitUntil: "load" })
    .then(() => printer.pdf({ format: "A4", printBackground: false }))
    .finally(() => printer.close());
  pdfId = await uploadFile(P.alice.page, W.folders.resumes.id, "履歴書-pdf.pdf", pdf, "application/pdf");
  W.defer(() => trashFileApi(P.alice.page, pdfId));
  await waitIndexed(P.alice.page, mark, pdfId);
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

test("PDF の履歴書: 本文が届き、伏せた後の材料に氏名・連絡先が無く、点数は要件ごとの判定から決まる", async () => {
  test.setTimeout(600_000);
  const app = W.apps.main;
  const levels = { keiri_bookkeeping: "満たす", keiri_closing: "満たす", keiri_sheet: "一部" };
  const params: Applicant = {
    applicant_no: "P-001",
    name: PEOPLE.sorano.name,
    position: "経理",
    years: 8,
    motivation: `${jsonLine({
      assessments: Object.entries(levels).map(([key, level]) => ({ key, level, evidence: "履歴書の職歴" })),
      summary: "経理の実務が長い",
    })}\n月次決算の経験を生かしたいと考えています。`,
    resume_file: pdfId,
  };
  // 取り込みを待ってから押すので、読むのは待たない（取り込み前に押すと最大 30 分「実行中」になる・GAP-62）。
  const run = await pressApiDone(alice(), app.id, { version: app.version, actionId: app.actions["登録して評価"], params });
  expect(run.status, JSON.stringify(run)).toBe("succeeded");
  const wf = W.workflows.screen.id;
  const d = await runs.get(alice(), wf, run.run_id);
  expect(runs.takenPorts(d, "read")).toEqual(["out"]);
  const read = await runs.stepOutput<{ content_type: string; text: string; truncated: boolean }>(alice(), wf, run.run_id, "read");
  expect(read.content_type).toBe("application/pdf");
  expect(read.truncated).toBe(false);
  expect(read.text.normalize("NFKC")).toContain("月次決算");
  expect(read.text).toContain(`rsm${W.uniq}`);
  // （参考・GAP-149・判定には使わない）取り込んだ本文に康熙部首（⽉ など）が残るか、キーワードの検索でふつうの字と
  // 部首の字のどちらで引けるかを残す（「月次決算」は部首の無い「決算」でも当たるので、1 語の「生年月日」も見る）。
  const radicals = [...new Set([...read.text].filter((c) => c >= "\u2f00" && c <= "\u2fdf"))].join("");
  console.log(`APP-05 RAG 参考（GAP-149）: 取り込んだ本文の康熙部首「${radicals}」`);
  for (const q of ["月次決算", "\u2f49次決算", "生年月日", "\u2f63年\u2f49\u2f47"]) {
    const r = await call<{ results?: { file_id?: string }[] }>(alice(), "POST", "/search", { query: q, mode: "keyword" });
    const hit = (r.body?.results ?? []).some((x) => x.file_id === pdfId);
    console.log(`APP-05 RAG 参考（GAP-149）: キーワード「${q}」の検索に PDF の履歴書が${hit ? "出た" : "出ない"}（${r.status}）`);
  }

  // (4)(a) prompt の $template に入る step 出力（伏せた後の本文）と指示の文に、氏名・連絡先が無い。
  const masked = (await runs.stepOutput<{ text: string }>(alice(), wf, run.run_id, "mask")).text;
  const brief = (await runs.stepOutput<{ system_text: string }>(alice(), wf, run.run_id, "brief")).system_text;
  // PDF の本文には見た目が同じ別の文字（康熙部首の「⽉」など）が混ざりうるので、NFKC でそろえて比べる。
  for (const secret of [PEOPLE.sorano.name, ...PEOPLE.sorano.secrets]) {
    expect(masked.normalize("NFKC"), `伏せた本文に「${secret}」が無い`).not.toContain(secret);
    expect(brief.normalize("NFKC"), `指示に「${secret}」が無い`).not.toContain(secret);
  }
  expect(masked.normalize("NFKC")).toContain("月次決算");

  // (1) 点数は要件ごとの判定から決まった計算で出る。行の履歴書は PDF。
  const s = await runs.stepOutput<{ score: number; rows: { key: string; level: string }[]; needs_review: string }>(alice(), wf, run.run_id, "score");
  expect(s.rows.map((r) => r.key).sort()).toEqual(REQUIREMENTS.filter((r) => r.position === "経理").map((r) => r.req_key).sort());
  expect(Object.fromEntries(s.rows.map((r) => [r.key, r.level]))).toEqual(levels);
  expect(s.score).toBe(expectedScore("経理", levels));
  // 評価済への遷移で「書類の判定」（event の run）が動き、点数が基準以上で要確認が無ければ書類通過に進める。その決まり
  // で決まる状態になるまで待つ（評価済のまま読むと判定の run と競い、間に何かが挟まると書類通過を読む）。
  const settled = s.score >= PASS_SCORE && s.needs_review === "なし" ? "書類通過" : "評価済";
  const found = await expectSingleRow(alice(), W.tables.applicants.id, cmp("applicant_no", "eq", "P-001"), "応募者 P-001");
  const row = await rows.waitForStatus(alice(), W.tables.applicants.id, found.id, [settled], { timeout: 60_000 });
  expect(row.data).toMatchObject({ score: s.score, needs_review: s.needs_review, resume: pdfId, status: settled });

  // (4)(c) PDF の監査（読んだ記録）に、氏名も本文も残らない。本文の読み出し（rag.doc_read）の記録があることを
  // 確かめてから、その記録と、この PDF のほかの記録（置いた・名前を引いた など）を見る。
  const reads = await audit.events(alice(), { action: "rag.doc_read", object_id: pdfId, limit: 50 });
  expect(reads.length, "履歴書の本文を読んだ監査がある").toBeGreaterThan(0);
  const events = [...reads, ...(await audit.events(alice(), { object_id: pdfId, limit: 100 }))];
  for (const e of events) {
    const text = JSON.stringify(e);
    for (const secret of [PEOPLE.sorano.name, ...PEOPLE.sorano.secrets, "月次決算"]) {
      expect(text.normalize("NFKC"), `監査に「${secret}」が無い`).not.toContain(secret);
    }
  }

  await expectOutsiderSeesNothing(P.charlie.page, W, { workflowRuns: [["screen", run.run_id]], appRuns: [["main", run.run_id]] });
});
