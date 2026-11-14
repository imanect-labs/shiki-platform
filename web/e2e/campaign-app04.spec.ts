import { expect, test, type Page } from "@playwright/test";

import {
  AI_FAILS,
  AI_REASON,
  INVOICES,
  REASON,
  TINY_PNG,
  VENDORS,
  app04World,
  eventRunDone,
  eventRunsFor,
  invoiceText,
  placeManyViaDrive,
  placeViaDrive,
  resaveText,
  vendorId,
  type TriggerInput,
} from "./campaign/apps/app04";
import { expectDenied, expectOutsiderSeesNothing, type Probe } from "./campaign/checks";
import { call, ok } from "./campaign/http";
import { people, phoneOf, type People } from "./campaign/people";
import { gridRows } from "./campaign/press";
import { noSideScroll, shot } from "./campaign/shots";
import { audit, cmp, expectSingleRow, rows, runs, type DataRecord, type RunDetail } from "./campaign/truth";
import { buildWorld, cleanupWorld, type World } from "./campaign/world";

/// APP-04 請求書の読み取り → 支払台帳（台帳 §7・§7.13 の APP-04・CI の版: 請求書は .txt）を組んで、請求書を置く人と
/// 経理として使い、成功判定をデータで確かめる。LLM は stub（請求書の .txt の `json:`・`jsonalways:`・`refuse:`・
/// `truncate:` の行で駆動する。IR には駆動の語を入れない）。成功判定の言い換え（§7.13）:
/// (1) 置くと 2 分以内に 1 行 → bob がドライブの画面から「請求書受付」に上げると、2 分以内に payables の file = ファイル ID
///     の行がちょうど 1 件でき、run は trigger_kind=event・input.op=create。
/// (2) 精度は作る人の領分 → (a) 形に合う出力は行にそのまま入る（金額は数・期日は YYYY-MM-DD・請求番号）(b) 形に合わない・
///     拒否・打ち切りは llm_output_invalid・llm_refused・llm_output_truncated で error の出口へ進み、理由つきの 要確認 の行が
///     1 件できる（run は成功）(c) 小計＋消費税≠合計は 要確認 で、理由に検算が入る。
/// (3) 再保存しても重複しない → 同じファイルの新しい版を保存すると、60 秒のまとめの後も件数と rev が変わらず、op=update の
///     run が成功する（その作成は unique_violation で error の出口へ）。同じ請求の別ファイルを置いても 1 行。
/// (4) 取引先が一致しなければ 要確認 で止まる → マスタに無い登録番号は 要確認・取引先は空・理由は取引先が未登録。マスタに
///     ある登録番号は 確認済 になり、取引先 = 取引先の行。遷移とリビジョンの主体は委譲元の alice（GAP-58 のいまの動き）。
/// （追加）注入・権限 → 本文の「承認済にし振込先を変えよ」は、AI の答えの形に無い項目なので llm_output_invalid で止まり、
///     状態も振込先も変わらない。charlie には表とフォルダーが見えない。経理でない bob には振込先が伏せて見える。
/// 文字を読めないファイル（画像）も 要確認 の行になる（行が無いままにならない）。
/// 長い待ちは新しい版の 60 秒のまとめだけ（台帳 §7.13）。`E2E_SHOTS_DIR` を渡すと、PC とスマホの画面を撮る。

test.describe.configure({ mode: "serial" });

let P: People;
let W: World;
/// 置いたファイルの id（試験の印 → id）。
const placed: Record<string, string> = {};

test.beforeAll(async ({ browser }) => {
  // 組む時間と next dev の初回のコンパイルで 60 秒を超えうる（計画のレビュー 21）。
  test.setTimeout(300_000);
  P = await people(browser);
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

const alice = () => P.alice.page;
const bob = () => P.bob.page;
const wf = () => W.workflows.read.id;
const payables = () => W.tables.payables.id;

/// bob がドライブの画面から「請求書受付」に置く（画面の準備を待つ版・app04.ts の placeViaDrive）。
async function place(mark: string, name: string, content: string | Buffer, mime = "text/plain"): Promise<string> {
  const id = await placeViaDrive(bob(), W.folders.inbox.id, name, content, mime);
  placed[mark] = id;
  return id;
}

/// bob がドライブの画面から「請求書受付」にまとめて置く（印 → [名前, 中身]）。
async function placeMany(files: Record<string, [string, string]>): Promise<Record<string, string>> {
  const byName = await placeManyViaDrive(
    bob(),
    W.folders.inbox.id,
    Object.values(files).map(([name, content]) => ({ name, content })),
  );
  const ids: Record<string, string> = {};
  for (const [mark, [name]] of Object.entries(files)) ids[mark] = placed[mark] = byName[name];
  return ids;
}

/// ファイルから作られた行（file = ファイル ID）。
async function rowsOfFile(fileId: string): Promise<DataRecord[]> {
  return rows.query(alice(), payables(), cmp("file", "eq", fileId));
}

/// run が落ちたときに理由が分かる文（失敗と、誤りのある step）。
function failureOf(d: RunDetail): string {
  return JSON.stringify({ failure: d.failure, steps: d.steps.filter((x) => x.error).map((x) => [x.step_path, x.error]) });
}

/// bob が置いた作成の run（trigger_kind=event・op=create・置いた人は bob）が成功して終わる。
async function createdRun(fileId: string): Promise<RunDetail> {
  const d = await eventRunDone(alice(), wf(), fileId, "create");
  expect(d.status, failureOf(d)).toBe("succeeded");
  expect(d.trigger_kind).toBe("event");
  expect(d.input as TriggerInput).toMatchObject({ file_id: fileId, op: "create", actor: P.bob.id, parent_id: W.folders.inbox.id });
  return d;
}

test("(1)(4) 登録済みの取引先の請求書を bob がドライブの画面から置くと、2 分以内に 1 行でき、確認済になる", async ({ browser }) => {
  test.setTimeout(240_000);
  const inv = INVOICES.ok;
  const started = Date.now();
  const id = await place("ok", "請求書-ハルカゼ印刷-HK-2611-01.txt", invoiceText(inv.file));
  // 置いてから 2 分以内に、そのファイルの行がちょうど 1 件できる。
  await expect.poll(async () => (await rowsOfFile(id)).length, { timeout: 120_000, intervals: [1_000, 2_000] }).toBe(1);
  expect(Date.now() - started, "置いてから 2 分以内").toBeLessThan(120_000);

  const d = await createdRun(id);
  expect(runs.takenPorts(d, "read")).toEqual(["out"]);
  expect(runs.takenPorts(d, "extract")).toEqual(["out"]);
  expect(runs.takenPorts(d, "create")).toEqual(["out"]);
  expect(runs.takenPorts(d, "ok")).toEqual(["true"]);
  expect(runs.takenPorts(d, "confirm")).toEqual(["out"]);
  for (const n of ["why", "not_saved", "unreadable", "ai_why", "ai_failed", "review_dup", "review_check"]) expect(runs.takenPorts(d, n), `${n} は通らない`).toBeNull();

  // (2)(a) 形に合う出力は、そろえた値のまま行に入る（金額は数・期日は YYYY-MM-DD・登録番号は半角にそろえる）。
  const row = await expectSingleRow(alice(), payables(), cmp("file", "eq", id), "請求書の行");
  expect(row.data).toMatchObject({
    dedupe_key: inv.key,
    invoice_no: inv.invoice_no,
    registration_no: VENDORS.harukaze.registration_no,
    vendor_name_raw: VENDORS.harukaze.name,
    subtotal: inv.subtotal,
    tax: inv.tax,
    amount: inv.amount,
    due_date: inv.due_date,
    file: id,
    status: "確認済",
  });
  expect(typeof row.data.amount).toBe("number");
  expect(row.data.review_reason ?? null).toBeNull();
  // (4) 取引先 = マスタの行（record_ref は要素 1 件の配列）。
  expect(row.data.vendor).toEqual([vendorId(W, "harukaze")]);
  const extracted = await runs.stepOutput<{ data: Record<string, unknown> }>(alice(), wf(), d.run_id, "extract");
  expect(extracted.data).toMatchObject({ invoice_no: inv.invoice_no, total: inv.amount, due_date: inv.due_date });

  // 自動実行が作った行の作成者と、作成・遷移のリビジョンの主体は、いまは委譲元（有効化した alice）（GAP-58・docs は
  // ワークフローと書いている）。遷移の監査の主体も alice。
  expect((row as { owner?: string }).owner).toBe(P.alice.id);
  const revs = await rows.revisions(alice(), payables(), row.id);
  expect(revs.map((r) => [r.change_kind, r.changed_by])).toEqual(
    expect.arrayContaining([
      ["create", P.alice.id],
      ["transition", P.alice.id],
    ]),
  );
  let moved: { actor?: string; decision?: string; metadata?: { to?: string } } | undefined;
  await expect
    .poll(
      async () => {
        const ev = await audit.events(alice(), { action: "data.record.transition", object_id: row.id, limit: 20 });
        moved = ev.find((e) => e.decision === "allow");
        return Boolean(moved);
      },
      { timeout: 15_000 },
    )
    .toBe(true);
  expect(moved!.actor).toBe(P.alice.id);

  // bob の画面（置いたドライブ）: 置いた請求書がフォルダーに出ている。
  await expect(bob().getByText("請求書-ハルカゼ印刷-HK-2611-01.txt", { exact: true })).toBeVisible({ timeout: 15_000 });
  await shot(bob(), "app04", "bob-drive-pc");
  const phone = await phoneOf(browser, P.bob);
  try {
    await phone.goto(`/drive?folder=${W.folders.inbox.id}`);
    await expect(phone.getByText("請求書-ハルカゼ印刷-HK-2611-01.txt", { exact: true }).first()).toBeVisible({ timeout: 60_000 });
    await noSideScroll(phone);
    await shot(phone, "app04", "bob-drive-phone");
  } finally {
    await phone.context().close();
  }
});

test("(4)(2)(c) マスタに無い登録番号は取引先が空の 要確認、検算が合わない請求書も 要確認 で、理由が入る", async () => {
  test.setTimeout(300_000);
  const { unknown: unknownId, badSum: badId } = await placeMany({
    unknown: ["請求書-ユウナギ物流-YN-88.txt", invoiceText(INVOICES.unknown.file)],
    badSum: ["請求書-コモレビ文具-KB-0457.txt", invoiceText(INVOICES.badSum.file)],
  });

  // (4) マスタに無い登録番号: 要確認・取引先は空・理由は取引先が未登録。確認済には進めない。
  const du = await createdRun(unknownId);
  expect(runs.takenPorts(du, "create")).toEqual(["out"]);
  expect(runs.takenPorts(du, "ok")).toEqual(["false"]);
  expect(runs.takenPorts(du, "confirm")).toBeNull();
  const unknown = await expectSingleRow(alice(), payables(), cmp("file", "eq", unknownId), "未登録の取引先の行");
  expect(unknown.data).toMatchObject({
    dedupe_key: INVOICES.unknown.key,
    invoice_no: INVOICES.unknown.invoice_no,
    amount: INVOICES.unknown.amount,
    due_date: INVOICES.unknown.due_date,
    status: "要確認",
  });
  expect(unknown.data.vendor ?? null, "取引先は空").toBeNull();
  expect(String(unknown.data.review_reason)).toMatch(new RegExp(`^${REASON.unknownVendor}（登録番号 T9300000000033）`));

  // (2)(c) 検算が合わない: 取引先は引けても 要確認 で、理由に小計・消費税・合計が入る。
  const db = await createdRun(badId);
  expect(runs.takenPorts(db, "ok")).toEqual(["false"]);
  const bad = await expectSingleRow(alice(), payables(), cmp("file", "eq", badId), "検算が合わない行");
  expect(bad.data).toMatchObject({ dedupe_key: INVOICES.badSum.key, amount: 13000, subtotal: 12000, tax: 1200, status: "要確認" });
  expect(bad.data.vendor).toEqual([vendorId(W, "komorebi")]);
  expect(String(bad.data.review_reason)).toBe(`${REASON.badSum}（12000 円 ＋ 1200 円 ≠ 13000 円）`);
});

test("(2)(b) 形に合わない・拒否・打ち切り・注入は error の出口へ進み、理由つきの 要確認 の行が 1 件でき、run は成功する", async () => {
  test.setTimeout(420_000);
  const komorebi = await rows.get(alice(), W.tables.vendors.id, vendorId(W, "komorebi"));
  const ids = await placeMany(
    Object.fromEntries(Object.entries(AI_FAILS).map(([mark, c]) => [mark, [`請求書-読み取りの確かめ-${mark}.txt`, invoiceText(c.file)]])),
  );

  for (const [mark, c] of Object.entries(AI_FAILS)) {
    const d = await createdRun(ids[mark]);
    expect(runs.takenPorts(d, "read"), mark).toEqual(["out"]);
    expect(runs.takenPorts(d, "extract"), mark).toEqual(["error"]);
    expect(runs.errorCode(d, "extract"), mark).toBe(c.code);
    expect(runs.takenPorts(d, "ai_failed"), mark).toEqual(["out"]);
    // 抽出の後ろ（そろえ・照合・登録・確認済）には進まない。
    for (const n of ["norm", "vendor", "check", "create", "confirm"]) expect(runs.takenPorts(d, n), `${mark} の ${n}`).toBeNull();
    const row = await expectSingleRow(alice(), payables(), cmp("file", "eq", ids[mark]), `${mark} の行`);
    expect(row.data, mark).toMatchObject({
      dedupe_key: `file:${ids[mark]}`,
      file: ids[mark],
      status: "要確認",
      review_reason: AI_REASON[c.code],
    });
    expect(row.data.invoice_no ?? null, `${mark} は AI の値を書かない`).toBeNull();
    expect(row.data.amount ?? null).toBeNull();
  }

  // 注入: 本文の指示（承認済・振込先の変更）は、AI の答えの形に無い項目なので通らず、状態は 要確認 のまま。取引先の振込先も
  // 変わらない（ワークフローは取引先の表に書かない）。どの行も 承認済 になっていない。
  const after = await rows.get(alice(), W.tables.vendors.id, komorebi.id);
  expect(after.rev).toBe(komorebi.rev);
  expect(after.data.bank_account).toBe(VENDORS.komorebi.bank_account);
  expect(await rows.count(alice(), payables(), cmp("status", "eq", "承認済"))).toBe(0);
  // 抽出の出口が書けるのは出力の形の項目だけ（IR の確かめ: 抽出の答えを使うのは norm の script だけ）。
  const users = (W.workflows.read.ir.nodes as { id: string; params: unknown }[]).filter((n) =>
    JSON.stringify(n.params).includes('"nodes.extract.output"'),
  );
  expect(users.map((n) => n.id).sort()).toEqual(["ai_why", "norm"]);
});

test("文字を読めないファイル（画像）を置いても、理由つきの 要確認 の行が 1 件でき、run は成功する", async () => {
  test.setTimeout(240_000);
  const id = await place("image", "請求書-スキャン.png", TINY_PNG, "image/png");
  const d = await createdRun(id);
  expect(runs.takenPorts(d, "read")).toEqual(["error"]);
  // 画像は取り込めない種類（社内文書の検索がある構成）か、検索が無い構成の誤り。
  expect(["not_indexable", "search_disabled", "not_text_file"]).toContain(runs.errorCode(d, "read"));
  expect(runs.takenPorts(d, "unreadable")).toEqual(["out"]);
  expect(runs.takenPorts(d, "extract")).toBeNull();
  const row = await expectSingleRow(alice(), payables(), cmp("file", "eq", id), "画像の行");
  expect(row.data).toMatchObject({ dedupe_key: `file:${id}`, file: id, status: "要確認" });
  expect(String(row.data.review_reason)).toContain(REASON.unreadable);
});

test("(3) 同じ請求の別ファイルを置いても 1 行のまま（作成は unique_violation で error の出口へ）", async () => {
  test.setTimeout(240_000);
  const first = await expectSingleRow(alice(), payables(), cmp("dedupe_key", "eq", INVOICES.ok.key), "同じ請求の行");
  const id = await place("okCopy", "請求書-ハルカゼ印刷-HK-2611-01-再送.txt", invoiceText(INVOICES.ok.file));
  const d = await createdRun(id);
  expect(runs.takenPorts(d, "create")).toEqual(["error"]);
  expect(runs.errorCode(d, "create")).toBe("unique_violation");
  expect(runs.takenPorts(d, "why")).toEqual(["dup"]);
  expect(runs.takenPorts(d, "not_saved")).toBeNull();
  expect(runs.takenPorts(d, "confirm")).toBeNull();
  const same = await expectSingleRow(alice(), payables(), cmp("dedupe_key", "eq", INVOICES.ok.key), "同じ請求の行");
  expect(same.id).toBe(first.id);
  expect(same.rev, "前の行は書き換えない").toBe(first.rev);
  expect(same.data.file).toBe(placed.ok);
  expect(await rowsOfFile(id)).toEqual([]);
});

test("(3) 同じファイルの新しい版を保存しても、60 秒のまとめの後に op=update の run が成功し、件数と rev は変わらない", async () => {
  test.setTimeout(420_000);
  const before = await rows.query(alice(), payables());
  const revs = Object.fromEntries(before.map((r) => [r.id, r.rev]));
  // bob がテキストの編集と同じ入口で保存し直す（中身は同じ・新しい版）。読み取れた請求書と、AI が断った請求書の 2 つ。
  const [vOk, vRefuse] = await Promise.all([resaveText(bob(), placed.ok), resaveText(bob(), placed.refuse)]);
  expect([vOk, vRefuse]).toEqual([2, 2]);

  // 新しい版は、同じファイルの保存が 60 秒途切れてから起きる（D-38）。
  const [dOk, dRefuse] = await Promise.all([
    eventRunDone(alice(), wf(), placed.ok, "update", 180_000),
    eventRunDone(alice(), wf(), placed.refuse, "update", 180_000),
  ]);
  expect(dOk.status, failureOf(dOk)).toBe("succeeded");
  expect(dOk.input as TriggerInput).toMatchObject({ op: "update", version: 2, actor: P.bob.id });
  expect(runs.takenPorts(dOk, "create")).toEqual(["error"]);
  expect(runs.errorCode(dOk, "create")).toBe("unique_violation");
  expect(runs.takenPorts(dOk, "why")).toEqual(["dup"]);
  // 読み取れなかった請求書も「file:<id>」の鍵で 1 行のまま（要確認の行の作成が unique_violation で error の出口へ）。
  expect(dRefuse.status, failureOf(dRefuse)).toBe("succeeded");
  expect(runs.errorCode(dRefuse, "extract")).toBe("llm_refused");
  expect(runs.takenPorts(dRefuse, "ai_failed")).toEqual(["error"]);
  expect(runs.errorCode(dRefuse, "ai_failed")).toBe("unique_violation");
  expect(runs.takenPorts(dRefuse, "review_check")).toEqual(["out"]);
  expect(await runs.stepOutput(alice(), wf(), dRefuse.run_id, "review_check")).toEqual({ duplicate: true });

  const after = await rows.query(alice(), payables());
  expect(after.length, "件数は変わらない").toBe(before.length);
  for (const r of after) expect(r.rev, `${String(r.data.dedupe_key)} の rev は変わらない`).toBe(revs[r.id]);
  await expectSingleRow(alice(), payables(), cmp("file", "eq", placed.ok), "読み取れた請求書の行");
  await expectSingleRow(alice(), payables(), cmp("file", "eq", placed.refuse), "AI が断った請求書の行");
  // 補助の証拠: 新しい版の run はファイルごとに 1 本。
  expect((await eventRunsFor(alice(), wf(), placed.ok, "update")).length).toBe(1);
});

/// 画面が引いたファイルの名前（`GET /api/files/{id}` が成功した id）。表のファイルの列はファイル名で出す（id のまま
/// 出さない）。表は canvas に描くので、名前を引いた要求で確かめ、引き終わってから撮る（読み込み中の「…」で撮らない）。
function fileNameReads(page: Page): Set<string> {
  const ids = new Set<string>();
  page.on("response", (r) => {
    const m = /\/api\/files\/([0-9a-f-]{36})$/.exec(new URL(r.url()).pathname);
    if (m && r.request().method() === "GET" && r.ok()) ids.add(m[1]);
  });
  return ids;
}

test("経理の画面: 要確認の請求書と支払予定の表。承認すると支払予定に移る", async ({ browser }) => {
  test.setTimeout(180_000);
  const app = W.apps.main;
  const review = await gridRows(alice(), app.id, app.version, "要確認の請求書");
  expect(review.status).toBe(200);
  // 表では請求書の列（画面ではファイル名で出る）で見分ける。重複を防ぐ鍵は隠す（隠した列は表の行に載らない）。
  const reviewFiles = review.rows.map((r) => r.data.file);
  for (const mark of ["unknown", "badSum", "invalid", "refuse", "truncate", "injection", "image"]) {
    expect(reviewFiles, `${mark} は要確認の表に出る`).toContain(placed[mark]);
  }
  expect(reviewFiles).not.toContain(placed.ok);
  expect(review.rows.every((r) => r.data.dedupe_key === undefined), "隠した列は表の行に載らない").toBe(true);
  // 経理が確認済の請求書を承認すると、支払予定の表に出る（承認は表の状態メニューと同じ遷移の入口・GAP-26）。
  const okRow = await expectSingleRow(alice(), payables(), cmp("file", "eq", placed.ok), "読み取れた請求書の行");
  const moved = await call(alice(), "POST", `/data/tables/${payables()}/records/${okRow.id}/transition`, { to: "承認済", expected_rev: okRow.rev });
  expect(moved.status, moved.text).toBe(200);
  const scheduled = await gridRows(alice(), app.id, app.version, "支払予定");
  expect(scheduled.rows.map((r) => r.data.file)).toEqual([placed.ok]);

  const shown = [...reviewFiles, placed.ok].map(String);
  const pcReads = fileNameReads(alice());
  await alice().goto(`/apps/${app.id}`);
  await expect(alice().getByTestId("miniapp-data-grid-要確認の請求書")).toBeVisible({ timeout: 30_000 });
  // 表の行は canvas に描くので、画面では件数で確かめる（行の中身は上の gridRows で裏取りした）。
  await expect(alice().getByTestId("miniapp-data-grid-要確認の請求書")).toContainText(`${review.rows.length} 件`, { timeout: 90_000 });
  await expect(alice().getByTestId("miniapp-data-grid-支払予定")).toContainText("1 件", { timeout: 90_000 });
  await expect.poll(() => shown.filter((id) => !pcReads.has(id)), { timeout: 30_000, message: "表の請求書の名前を引いた" }).toEqual([]);
  await shot(alice(), "app04", "alice-app-pc");
  const phone = await phoneOf(browser, P.alice);
  try {
    const phoneReads = fileNameReads(phone);
    await phone.goto(`/apps/${app.id}`);
    await expect(phone.getByTestId("miniapp-data-grid-要確認の請求書")).toContainText(`${review.rows.length} 件`, { timeout: 90_000 });
    await expect(phone.getByTestId("miniapp-data-grid-支払予定")).toContainText("1 件", { timeout: 90_000 });
    await expect.poll(() => shown.filter((id) => !phoneReads.has(id)), { timeout: 30_000, message: "表の請求書の名前を引いた" }).toEqual([]);
    await noSideScroll(phone);
    await shot(phone, "app04", "alice-app-phone");
  } finally {
    await phone.context().close();
  }
});

test("権限: 経理でない bob には振込先が伏せて見え、支払台帳とワークフローは見えない。charlie はどれも見えない", async () => {
  test.setTimeout(240_000);
  // bob（取引先の閲覧者・経理でない）: 振込先の列が伏せられる。alice（経理）には見える。
  const seen = await rows.query(bob(), W.tables.vendors.id);
  expect(seen.length).toBe(Object.keys(VENDORS).length);
  for (const r of seen) expect(Object.keys(r.data), `${String(r.data.name)} の振込先は伏せる`).not.toContain("bank_account");
  const one = await ok<DataRecord>(bob(), "GET", `/data/tables/${W.tables.vendors.id}/records/${vendorId(W, "harukaze")}`);
  expect(one.data.bank_account).toBeUndefined();
  expect((await rows.get(alice(), W.tables.vendors.id, vendorId(W, "harukaze"))).data.bank_account).toBe(VENDORS.harukaze.bank_account);

  // bob は請求書を置けるが、支払台帳・ワークフロー・経理の画面は共有されていない（存在しない id と同じ答え）。
  const okRow = await expectSingleRow(alice(), payables(), cmp("file", "eq", placed.ok), "読み取れた請求書の行");
  const runId = (await eventRunsFor(alice(), wf(), placed.ok, "create"))[0].run_id;
  const hidden: Probe[] = [
    { path: `/data/tables/${payables()}`, what: "支払台帳" },
    { method: "POST", path: `/data/tables/${payables()}/query`, data: { limit: 1 }, what: "支払台帳の行" },
    { path: `/data/tables/${payables()}/records/${okRow.id}`, what: "支払台帳の 1 行" },
    { path: `/workflows/${wf()}`, what: "請求書の読み取り" },
    { path: `/workflows/${wf()}/runs/${runId}`, what: "読み取りの run" },
    { path: `/mini-apps/${W.apps.main.id}/resolved`, what: "経理の画面" },
  ];
  await expectDenied(bob(), hidden);

  // charlie（別の組織）: 世界のどれも、置いた請求書も、行も run も読めず、存在しない id と同じ答え。
  await expectOutsiderSeesNothing(P.charlie.page, W, { workflowRuns: [["read", runId]] });
  const extra: Probe[] = [
    ...Object.entries(placed).map(([mark, id]) => ({ path: `/files/${id}/text`, what: `置いた請求書 ${mark}` })),
    { path: `/data/tables/${payables()}/records/${okRow.id}`, what: "支払台帳の 1 行" },
    { path: `/data/tables/${W.tables.vendors.id}/records/${vendorId(W, "harukaze")}`, what: "取引先の 1 行" },
  ];
  await expectDenied(P.charlie.page, extra);
});

