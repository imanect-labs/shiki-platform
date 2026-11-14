import { expect, test } from "@playwright/test";

import { app04World, eventRunDone, placeViaDrive, ragEnabled, textPdf, type TriggerInput } from "./campaign/apps/app04";
import { expectDenied } from "./campaign/checks";
import { call } from "./campaign/http";
import { people, type People } from "./campaign/people";
import { shot } from "./campaign/shots";
import { cmp, expectSingleRow, rows, runs } from "./campaign/truth";
import { buildWorld, cleanupWorld, type World } from "./campaign/world";

/// APP-04 請求書の読み取り → 支払台帳（台帳 §7.13 の APP-04・RAG の版: 請求書は PDF）。PDF の本文は社内文書の検索の
/// 取り込み（worker）が作るので、検索が無い構成（CI の web-e2e・手元の検証のスタック）では skip する（`/api/search` が
/// 503・workflow-storage-read-document.spec.ts と同じ見分け方・D-226 (4)）。LLM は stub。
/// PDF の本文は頭が「〔p.1〕」になり、stub の駆動の語を置けないので、抽出は stub の既定の最小値（文字は空・金額は 0）になる。
/// そのため、確かめるのは仕組みだけ（§7.13）: (1) bob がドライブの画面から PDF を置くと、storage.read の step が取り込みを
/// 待って（index_pending で失敗せず・試す回数を使わず）成功し、取り込みが終わってからの読み直しのうちに payables に
/// file = ファイル ID の行がちょうど 1 件できる。読み取れない項目ばかりなので 要確認 で、理由が入る。run は
/// trigger_kind=event・input.op=create。値の正しさ（精度）は見ない。

test.describe.configure({ mode: "serial" });

let P: People;
let W: World;

test.beforeAll(async ({ browser }) => {
  test.setTimeout(300_000);
  P = await people(browser);
  test.skip(!(await ragEnabled(P.alice.page)), "社内文書の検索が無い構成（RAG の worker を立てた構成でだけ走る）");
  W = await buildWorld(P, app04World());
});

test.afterAll(async () => {
  test.setTimeout(180_000);
  try {
    if (W) await cleanupWorld(W);
  } finally {
    await P?.close();
  }
});

test("(1) PDF の請求書を置くと、取り込みを待って読み、取り込みの後の読み直しのうちに 要確認 の行が 1 件できる", async () => {
  test.setTimeout(900_000);
  const alice = P.alice.page;
  const wf = W.workflows.read.id;
  const token = `inv${Date.now().toString(36)}`;
  const pdf = textPdf(["INVOICE HK-2611-09", "Harukaze Printing", "Registration T9100000000011", "Total 55000 JPY", `Ref ${token}`]);
  const name = "請求書-ハルカゼ印刷-HK-2611-09.pdf";
  const id = await placeViaDrive(P.bob.page, W.folders.inbox.id, name, pdf, "application/pdf");
  await shot(P.bob.page, "app04-rag", "bob-drive-pc");

  // 取り込みが終わる（目印で検索に出る）まで待ち、その時刻を覚える。
  await expect
    .poll(
      async () => {
        const r = await call<{ results?: { file_id?: string }[] }>(alice, "POST", "/search", { query: token });
        return r.status === 200 && (r.body?.results ?? []).some((x) => x.file_id === id);
      },
      { timeout: 600_000, intervals: [3_000, 5_000], message: "PDF の取り込み" },
    )
    .toBe(true);
  const indexedAt = Date.now();

  // 取り込みの後の読み直し（間隔は 1 回あたり最大 2 分・engine.md §7.5）のうちに行ができる。
  await expect
    .poll(async () => (await rows.query(alice, W.tables.payables.id, cmp("file", "eq", id))).length, {
      timeout: 180_000,
      intervals: [2_000, 5_000],
      message: "取り込みの後に行ができる",
    })
    .toBe(1);
  expect(Date.now() - indexedAt, "取り込みが終わってから 2 分 30 秒以内").toBeLessThan(150_000);

  const d = await eventRunDone(alice, wf, id, "create", 60_000);
  expect(d.status, JSON.stringify(d.failure)).toBe("succeeded");
  expect(d.trigger_kind).toBe("event");
  expect(d.input as TriggerInput).toMatchObject({ file_id: id, op: "create", actor: P.bob.id });
  // storage.read は取り込みを待っても失敗にならず、試す回数を使わない（GAP-50）。本文には取り込んだ文字が入る。
  const read = runs.step(d, "read");
  expect(read.status).toBe("succeeded");
  expect(read.taken_ports).toEqual(["out"]);
  expect(read.attempt).toBe(1);
  const text = (await runs.stepOutput<{ text: string }>(alice, wf, d.run_id, "read")).text;
  expect(text).toContain(token);
  expect(text.startsWith("〔p.1〕")).toBe(true);
  // 抽出は stub の既定の最小値（形には合う）なので、読み取れない項目ばかりの 要確認 の行になる。
  expect(runs.takenPorts(d, "extract")).toEqual(["out"]);
  expect(runs.takenPorts(d, "ok")).toEqual(["false"]);
  const row = await expectSingleRow(alice, W.tables.payables.id, cmp("file", "eq", id), "PDF の行");
  expect(row.data).toMatchObject({ dedupe_key: `file:${id}`, status: "要確認", file: id });
  expect(String(row.data.review_reason)).toContain("登録番号を読み取れませんでした");

  // charlie は置いた PDF も行も読めない。
  await expectDenied(P.charlie.page, [
    { path: `/files/${id}/text`, what: "置いた PDF" },
    { path: `/data/tables/${W.tables.payables.id}/records/${row.id}`, what: "PDF の行" },
  ]);
});
