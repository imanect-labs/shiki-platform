import { expect, test, type Page } from "@playwright/test";

import { eventRunDone, eventRunsFor, placeManyViaDrive, placeViaDrive, resaveText, type TriggerInput } from "./campaign/apps/app04";
import { AI_FAILS, AI_REASON, MINUTES, NO_TASKS, REVIEW, app09World, minutesText } from "./campaign/apps/app09";
import { expectDenied, expectOnlyOwn, expectOutsiderSeesNothing, type Probe } from "./campaign/checks";
import { call, type Json } from "./campaign/http";
import { people, phoneOf, type People } from "./campaign/people";
import { gridRows } from "./campaign/press";
import { noSideScroll, shot } from "./campaign/shots";
import { addDays } from "./campaign/time";
import { cmp, expectSingleRow, rows, runs, type DataRecord, type RunDetail } from "./campaign/truth";
import { buildWorld, cleanupWorld, type World } from "./campaign/world";

/// APP-09 議事録 → タスク化（台帳 §7・§7.13 の APP-09・議事録は .md）を組んで、議事録を置く人・担当者・PMO として使い、
/// 成功判定をデータで確かめる。LLM は stub（議事録の .md の `json:`・`jsonalways:`・`refuse:`・`truncate:` の行で駆動する。
/// IR には駆動の語を入れない）。成功判定の言い換え（§7.13）:
/// (1) 抽出漏れは作る人の領分 → AI の出力の tasks が N 件なら、meeting = ファイル ID の行がちょうど N 件でき、題名と抜粋は
///     出力のまま。担当や期日を決められない件も行になり 要確認 が付く。1 件の書き込みが失敗してもほかの件は書かれ、失敗した
///     件は map の errors に出る。0 件なら行ができず、議事録の処理が 要確認。形に合わない・拒否・打ち切りでは途中までの行が
///     できず、議事録の処理が 失敗。
/// (2) 担当が実在ユーザーに解決 → 「アリス」「Bob さん」「ボブ様」は alice・bob の id。名簿に無い名前・別の組織の charlie・
///     AI が返した生の UUID は空欄＋要確認。user_ref の検証で落ちる行は出ない。bob に見えるのは自分の行だけで、PMO の alice
///     には全部見える。
/// (3) 期日が date 型 → 「2026-11-20」「明日」「来週金曜」は run の日付（transform.date の出力）を基準にした YYYY-MM-DD。
///     「未定」や読めない値は空欄＋要確認。暦に無い日付（2026-02-30）は日付の列が断る（その件だけが errors に出る）。
/// (4) 同じファイルを保存し直しても増えない → bob が自分のタスクを 進行中 にしてから同じ .md を保存し直すと、件数は
///     変わらず、進行中 は残り、op=update の run が成功する。1 件足した版を保存すると 1 件増える。同じ題名は 1 行。
/// （追加）担当者だけが状態を進められる。PMO はどの行も進められる。charlie にはどれも見えない。
/// 作成者: 自動実行が作った行の作成者は、いまは委譲元の alice（GAP-58 のいまの動きを書き留める）。
/// 長い待ちは新しい版の 60 秒のまとめだけ（台帳 §7.13）。`E2E_SHOTS_DIR` を渡すと、PC とスマホの画面を撮る。

test.describe.configure({ mode: "serial" });

let P: People;
let W: World;
/// 置いた議事録の id（試験の印 → id）。
const placed: Record<string, string> = {};

test.beforeAll(async ({ browser }) => {
  // 組む時間と next dev の初回のコンパイルで 60 秒を超えうる（計画のレビュー 21）。
  test.setTimeout(300_000);
  P = await people(browser);
  W = await buildWorld(P, app09World());
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
const wf = () => W.workflows.tasks.id;
const tasks = () => W.tables.tasks.id;
const log = () => W.tables.log.id;

type AiTask = { title: string; assignee_name: string | null; due_text: string | null; excerpt: string };
type MapOut = { items: Json[]; errors: { index: number; error: { code?: string; message?: string } }[] };

/// 議事録の .md の `json:` の行（stub が返す AI の出力）のタスク。
function aiTasks(text: string): AiTask[] {
  const line = text.split("\n").find((l) => l.startsWith("json:"));
  if (!line) throw new Error("json: の行が無い議事録");
  return (JSON.parse(line.slice("json:".length)) as { tasks: AiTask[] }).tasks;
}

/// bob がドライブの画面から「議事録」に置く。
async function place(mark: string, name: string, content: string): Promise<string> {
  const id = await placeViaDrive(bob(), W.folders.minutes.id, name, content, "text/markdown");
  placed[mark] = id;
  return id;
}

/// bob がドライブの画面から「議事録」にまとめて置く（印 → [名前, 中身]）。
async function placeMany(files: Record<string, [string, string]>): Promise<Record<string, string>> {
  const byName = await placeManyViaDrive(
    bob(),
    W.folders.minutes.id,
    Object.values(files).map(([name, content]) => ({ name, content, mimeType: "text/markdown" })),
  );
  const ids: Record<string, string> = {};
  for (const [mark, [name]] of Object.entries(files)) ids[mark] = placed[mark] = byName[name];
  return ids;
}

/// run が落ちたときに理由が分かる文（失敗と、誤りのある step）。
function failureOf(d: RunDetail): string {
  return JSON.stringify({ failure: d.failure, steps: d.steps.filter((x) => x.error).map((x) => [x.step_path, x.error]) });
}

/// bob が置いた作成の run が成功して終わる。
async function createdRun(fileId: string): Promise<RunDetail> {
  const d = await eventRunDone(alice(), wf(), fileId, "create");
  expect(d.status, failureOf(d)).toBe("succeeded");
  expect(d.input as TriggerInput).toMatchObject({ file_id: fileId, op: "create", actor: P.bob.id, parent_id: W.folders.minutes.id });
  return d;
}

/// 議事録から作られたタスク（題名 → 行・PMO の alice の見え方）。
async function tasksOf(fileId: string): Promise<Record<string, DataRecord>> {
  const found = await rows.query(alice(), tasks(), cmp("meeting", "eq", fileId));
  const byTitle: Record<string, DataRecord> = {};
  for (const r of found) {
    const t = String(r.data.title);
    expect(byTitle[t], `題名「${t}」の行は 1 つ`).toBeUndefined();
    byTitle[t] = r;
  }
  return byTitle;
}

/// 議事録の処理の行（ちょうど 1 行）。
async function logOf(fileId: string): Promise<DataRecord> {
  return expectSingleRow(alice(), log(), cmp("file_id", "eq", fileId), "議事録の処理の行");
}

/// run の日付（transform.date の出力から）: 今日・明日・来週の金曜。
async function datesOf(d: RunDetail): Promise<{ today: string; tomorrow: string; nextFriday: string }> {
  const today = (await runs.stepOutput<{ date: string }>(alice(), wf(), d.run_id, "today")).date;
  const tomorrow = (await runs.stepOutput<{ date: string }>(alice(), wf(), d.run_id, "tomorrow")).date;
  const nextMonday = (await runs.stepOutput<{ start: string }>(alice(), wf(), d.run_id, "next_week")).start;
  return { today, tomorrow, nextFriday: addDays(nextMonday, 4) };
}

/// 状態を進める（表の状態メニューと同じ入口）。
async function move(page: Page, rec: DataRecord, to: string) {
  return call(page, "POST", `/data/tables/${tasks()}/records/${rec.id}/transition`, { to, expected_rev: rec.rev });
}

test("(1)(2)(3) AI の出力が 3 件・1 件・0 件の議事録で、行はちょうどその数でき、担当と期日が決まる", async () => {
  test.setTimeout(360_000);
  const {
    weekly,
    oneOnOne: one,
    infoOnly: none,
  } = await placeMany({
    weekly: [MINUTES.weekly.name, minutesText(MINUTES.weekly.file)],
    oneOnOne: [MINUTES.oneOnOne.name, minutesText(MINUTES.oneOnOne.file)],
    infoOnly: [MINUTES.infoOnly.name, minutesText(MINUTES.infoOnly.file)],
  });

  // 3 件: どれも名簿の人と読める期日 → 3 行・要確認なし・議事録の処理は 登録済。
  const dw = await createdRun(weekly);
  expect(runs.takenPorts(dw, "extract")).toEqual(["out"]);
  expect(runs.takenPorts(dw, "each")).toEqual(["out"]);
  const mw = await runs.stepOutput<MapOut>(alice(), wf(), dw.run_id, "each");
  expect(mw.items.length).toBe(3);
  expect(mw.errors).toEqual([]);
  const { today, tomorrow, nextFriday } = await datesOf(dw);
  expect(tomorrow).toBe(addDays(today, 1));
  const ai = aiTasks(minutesText(MINUTES.weekly.file));
  const extracted = await runs.stepOutput<{ data: { tasks: AiTask[] } }>(alice(), wf(), dw.run_id, "extract");
  expect(extracted.data.tasks).toEqual(ai);
  const w = await tasksOf(weekly);
  expect(Object.keys(w).sort()).toEqual(ai.map((t) => t.title).sort());
  for (const t of ai) expect(w[t.title].data.excerpt, `${t.title} の抜粋は出力のまま`).toBe(t.excerpt);
  expect(w["見積書のひな形を直す"].data).toMatchObject({ assignee: P.alice.id, due: "2026-11-20", status: "未着手" });
  expect(w["展示会の見本の数を倉庫に確かめる"].data).toMatchObject({ assignee: P.bob.id, due: tomorrow, status: "未着手" });
  expect(w["展示会の配布資料の案を出す"].data).toMatchObject({ assignee: P.bob.id, due: nextFriday, status: "未着手" });
  for (const r of Object.values(w)) {
    expect(r.data.check ?? null, `${String(r.data.title)} は要確認でない`).toBeNull();
    expect(r.data.meeting).toBe(weekly);
    // 自動実行が作った行の作成者は、いまは委譲元（有効化した alice）（GAP-58・docs はワークフローと書いている）。
    expect((r as { owner?: string }).owner, `${String(r.data.title)} の作成者`).toBe(P.alice.id);
  }
  expect((await logOf(weekly)).data).toMatchObject({ result: "登録済", task_count: 3, review_count: 0, failed_count: 0 });

  // 1 件: 名簿に無い人と「未定」→ 空欄＋要確認の行が 1 つ・議事録の処理は 要確認。
  await createdRun(one);
  const o = await tasksOf(one);
  expect(Object.keys(o)).toEqual(["目標のたたき台を書く"]);
  const kenta = o["目標のたたき台を書く"];
  expect(kenta.data.assignee ?? null, "名簿に無い人は空欄").toBeNull();
  expect(kenta.data.due ?? null, "「未定」は空欄").toBeNull();
  expect(kenta.data.check).toBe(REVIEW);
  expect(String(kenta.data.note)).toBe("担当「ケンタさん」が名簿にいません。期日「未定」を日付にできません");
  expect((await logOf(one)).data).toMatchObject({ result: REVIEW, task_count: 1, review_count: 1, failed_count: 0 });

  // 0 件: 行はできず、議事録の処理は 要確認（宿題が無いか、書き方を確かめる）。
  const dn = await createdRun(none);
  const mn = await runs.stepOutput<MapOut>(alice(), wf(), dn.run_id, "each");
  expect(mn).toMatchObject({ items: [], errors: [] });
  expect(await tasksOf(none)).toEqual({});
  expect((await logOf(none)).data).toMatchObject({ result: REVIEW, reason: NO_TASKS, task_count: 0, review_count: 0, failed_count: 0 });
});

test("(1)(2)(3) 名前と期日のいろいろ: 名簿に無い人・別の組織の人・生の UUID は空欄＋要確認、暦に無い日付の 1 件だけが errors に出る、同じ題名は 1 行", async () => {
  test.setTimeout(180_000);
  const text = W.fill(minutesText(MINUTES.mixed.file));
  const id = await place("mixed", MINUTES.mixed.name, text);
  const d = await createdRun(id);
  const { tomorrow } = await datesOf(d);
  const ai = aiTasks(text);
  expect(ai.length).toBe(6);

  // 1 件の書き込みの失敗（暦に無い日付を日付の列が断る）は、ほかの件を止めず、map の errors に出る。担当の列（user_ref）の
  // 検証で落ちた件は無い（決められない担当は空欄にしてから書く）。
  const m = await runs.stepOutput<MapOut>(alice(), wf(), d.run_id, "each");
  expect(m.errors.map((e) => e.index), JSON.stringify(m.errors)).toEqual([5]);
  expect(ai[5].title).toBe("会場を予約する");
  expect(m.errors[0].error.code).toBe("invalid");
  expect(String(m.errors[0].error.message ?? "")).not.toMatch(/ユーザー|assignee|担当/);

  const t = await tasksOf(id);
  // 同じ題名の 2 件は 1 行（同じ鍵で探して書き換える）。暦に無い日付の件は行にならない。
  expect(Object.keys(t).sort()).toEqual(["取引先の一覧を更新する", "提案書の目次を作る", "見積の前提を確かめる", "議事録を全員に共有する"].sort());
  expect(t["取引先の一覧を更新する"].data).toMatchObject({ due: "2026-11-25", check: REVIEW });
  expect(t["取引先の一覧を更新する"].data.assignee ?? null, "別の組織の人は空欄").toBeNull();
  expect(String(t["取引先の一覧を更新する"].data.note)).toBe("担当「Charlie さん」が名簿にいません");
  expect(t["見積の前提を確かめる"].data).toMatchObject({ due: tomorrow, check: REVIEW });
  expect(t["見積の前提を確かめる"].data.assignee ?? null, "AI が返した生の UUID は使わない").toBeNull();
  expect(t["提案書の目次を作る"].data).toMatchObject({ assignee: P.bob.id, check: REVIEW, note: "期日「来月のどこか」を日付にできません" });
  expect(t["提案書の目次を作る"].data.due ?? null).toBeNull();
  expect(t["議事録を全員に共有する"].data).toMatchObject({ assignee: P.alice.id, due: "2026-11-16" });
  expect(t["議事録を全員に共有する"].data.check ?? null).toBeNull();
  // 期日の列は日付の型で、値はいつも YYYY-MM-DD（空欄を除く）。
  const dueField = (W.tables.tasks.schema.fields as { key: string; type: string }[]).find((f) => f.key === "due");
  expect(dueField?.type).toBe("date");
  for (const r of Object.values(t)) if (r.data.due != null) expect(String(r.data.due)).toMatch(/^\d{4}-\d{2}-\d{2}$/);

  const l = await logOf(id);
  expect(l.data).toMatchObject({ result: REVIEW, task_count: 6, failed_count: 1, review_count: 3 });
  expect(String(l.data.reason)).toContain("1 件のタスクを登録できませんでした（「会場を予約する」）");
});

test("(1) 形に合わない・拒否・打ち切りでは途中までの行を作らず、議事録の処理が 失敗 になる（run は成功）", async () => {
  test.setTimeout(360_000);
  const ids = await placeMany(Object.fromEntries(Object.entries(AI_FAILS).map(([mark, c]) => [mark, [c.name, minutesText(c.file)]])));
  for (const [mark, c] of Object.entries(AI_FAILS)) {
    const d = await createdRun(ids[mark]);
    expect(runs.takenPorts(d, "extract"), mark).toEqual(["error"]);
    expect(runs.errorCode(d, "extract"), mark).toBe(c.code);
    for (const n of ["plan", "each", "tally", "log"]) expect(runs.takenPorts(d, n), `${mark} の ${n}`).toBeNull();
    expect(runs.takenPorts(d, "log_ai"), mark).toEqual(["out"]);
    expect(await tasksOf(ids[mark]), `${mark} は行を作らない`).toEqual({});
    expect((await logOf(ids[mark])).data, mark).toMatchObject({ result: "失敗", reason: AI_REASON[c.code], task_count: 0 });
  }
});

test("(2) bob に見えるのは自分のタスクだけで、PMO の alice には全部見える（アプリの表も同じ）", async ({ browser }) => {
  test.setTimeout(180_000);
  const everyone = await rows.query(alice(), tasks());
  const owners = new Set(everyone.map((r) => r.data.assignee ?? null));
  expect(owners).toEqual(new Set([P.alice.id, P.bob.id, null]));
  const bobs = everyone.filter((r) => r.data.assignee === P.bob.id);
  // bob が表を直接読んでも、行の決まりで自分の分だけ（担当の無い行も見えない）。
  const direct = await rows.query(bob(), tasks());
  expectOnlyOwn(direct, "assignee", P.bob.id, "bob が読んだタスク");
  expect(direct.map((r) => r.id).sort()).toEqual(bobs.map((r) => r.id).sort());

  // アプリの「自分のタスク」「要確認のタスク」も本人の分だけ。
  const app = W.apps.main;
  const mine = await gridRows(bob(), app.id, app.version, "自分のタスク");
  expect(mine.status).toBe(200);
  expectOnlyOwn(mine.rows, "assignee", P.bob.id, "bob の「自分のタスク」");
  expect(mine.rows.map((r) => r.data.title)).toEqual(expect.arrayContaining(["展示会の見本の数を倉庫に確かめる", "展示会の配布資料の案を出す"]));
  const review = await gridRows(bob(), app.id, app.version, "要確認のタスク");
  expect(review.rows.map((r) => r.data.title)).toEqual(["提案書の目次を作る"]);
  // PMO の alice の「要確認のタスク」には、担当の無い行も出る。
  const pmo = await gridRows(alice(), app.id, app.version, "要確認のタスク");
  expect(pmo.rows.map((r) => r.data.title)).toEqual(
    expect.arrayContaining(["目標のたたき台を書く", "取引先の一覧を更新する", "見積の前提を確かめる", "提案書の目次を作る"]),
  );

  // bob の画面（PC・スマホ）。
  await bob().goto(`/apps/${app.id}`);
  const grid = bob().getByTestId("miniapp-data-grid-自分のタスク");
  await expect(grid).toBeVisible({ timeout: 30_000 });
  // 表の行は canvas に描くので、画面では件数（本人の分の数）で確かめる。行の中身は上の gridRows で裏取りした。
  await expect(grid).toContainText(`${mine.rows.length} 件`, { timeout: 90_000 });
  await shot(bob(), "app09", "bob-app-pc");
  const phone = await phoneOf(browser, P.bob);
  try {
    await phone.goto(`/apps/${app.id}`);
    // 表の最初のページは、表の定義と見え方を読んでから描く（負荷が高いと 30 秒を超えることがあった）。
    await expect(phone.getByTestId("miniapp-data-grid-自分のタスク")).toContainText(`${mine.rows.length} 件`, { timeout: 90_000 });
    await expect(phone.getByTestId("miniapp-data-grid-要確認のタスク")).toContainText(`${review.rows.length} 件`, { timeout: 90_000 });
    await noSideScroll(phone);
    await shot(phone, "app09", "bob-app-phone");
  } finally {
    await phone.context().close();
  }
});

test("(4) bob が 進行中 にしたタスクの議事録を保存し直しても、件数は変わらず 進行中 が残る。1 件足した版では 1 件増える", async () => {
  test.setTimeout(420_000);
  const weekly = placed.weekly;
  const one = placed.oneOnOne;
  const before = await tasksOf(weekly);
  const mineRow = before["展示会の見本の数を倉庫に確かめる"];
  const moved = await move(bob(), mineRow, "進行中");
  expect(moved.status, moved.text).toBe(200);
  const kentaBefore = (await tasksOf(one))["目標のたたき台を書く"];

  // bob がテキストの編集と同じ入口で保存し直す: 営業定例は同じ中身、面談メモは宿題を 1 件足した版。
  const [v1, v2] = await Promise.all([
    resaveText(bob(), weekly),
    resaveText(bob(), one, minutesText(MINUTES.oneOnOneV2.file)),
  ]);
  expect([v1, v2]).toEqual([2, 2]);
  // 新しい版は、同じファイルの保存が 60 秒途切れてから起きる（D-38）。
  const [dw, d1] = await Promise.all([
    eventRunDone(alice(), wf(), weekly, "update", 180_000),
    eventRunDone(alice(), wf(), one, "update", 180_000),
  ]);
  for (const d of [dw, d1]) {
    expect(d.status, failureOf(d)).toBe("succeeded");
    expect(d.input as TriggerInput).toMatchObject({ op: "update", version: 2, actor: P.bob.id });
  }
  // 同じ中身: 3 件とも同じ鍵で見つけて書き換え（新しい行を作らない）。件数は 3 のまま、bob の 進行中 は残る。
  const mw = await runs.stepOutput<MapOut>(alice(), wf(), dw.run_id, "each");
  expect(mw.errors).toEqual([]);
  expect(mw.items.map((x) => (x as { created?: boolean }).created)).toEqual([false, false, false]);
  const after = await tasksOf(weekly);
  expect(Object.keys(after).sort()).toEqual(Object.keys(before).sort());
  for (const [title, r] of Object.entries(after)) expect(r.id, `${title} は同じ行`).toBe(before[title].id);
  expect(after["展示会の見本の数を倉庫に確かめる"].data.status, "bob の 進行中 は残る").toBe("進行中");
  expect(after["見積書のひな形を直す"].data.status).toBe("未着手");

  // 1 件足した版: 前の 1 件は同じ行のまま、足した 1 件だけが増える。
  const o = await tasksOf(one);
  expect(Object.keys(o).sort()).toEqual(["目標のたたき台を書く", "研修の申し込みを済ませる"].sort());
  expect(o["目標のたたき台を書く"].id).toBe(kentaBefore.id);
  expect(o["研修の申し込みを済ませる"].data).toMatchObject({ assignee: P.alice.id, due: "2026-11-28", status: "未着手" });
  expect((await logOf(one)).data).toMatchObject({ result: REVIEW, task_count: 2, review_count: 1 });
  // 補助の証拠: 新しい版の run はファイルごとに 1 本。
  expect((await eventRunsFor(alice(), wf(), weekly, "update")).length).toBe(1);
});

test("（追加）担当者は自分のタスクだけを進められ、PMO はどのタスクも進められる", async () => {
  test.setTimeout(180_000);
  const w = await tasksOf(placed.weekly);
  // bob: 自分の行を 進行中 → 完了。
  const mine = w["展示会の見本の数を倉庫に確かめる"];
  const done = await move(bob(), mine, "完了");
  expect(done.status, done.text).toBe(200);
  expect((await rows.get(alice(), tasks(), mine.id)).data.status).toBe("完了");
  const revs = await rows.revisions(alice(), tasks(), mine.id);
  expect(revs.filter((r) => r.change_kind === "transition").map((r) => r.changed_by)).toEqual([P.bob.id, P.bob.id]);

  // bob: alice の行と担当の無い行は、読めず進められない（存在しない id と同じ答え）。
  const alices = w["見積書のひな形を直す"];
  const nobody = (await tasksOf(placed.oneOnOne))["目標のたたき台を書く"];
  const denied: Probe[] = [
    { path: `/data/tables/${tasks()}/records/${alices.id}`, what: "alice のタスク" },
    {
      method: "POST",
      path: `/data/tables/${tasks()}/records/${alices.id}/transition`,
      data: { to: "進行中", expected_rev: alices.rev },
      what: "alice のタスクを進める",
    },
    {
      method: "POST",
      path: `/data/tables/${tasks()}/records/${nobody.id}/transition`,
      data: { to: "進行中", expected_rev: nobody.rev },
      what: "担当の無いタスクを進める",
    },
  ];
  await expectDenied(bob(), denied);
  expect((await rows.get(alice(), tasks(), alices.id)).rev).toBe(alices.rev);

  // PMO の alice は、bob のタスクも担当の無いタスクも進められる。
  const bobOther = w["展示会の配布資料の案を出す"];
  expect((await move(alice(), bobOther, "進行中")).status).toBe(200);
  expect((await move(alice(), nobody, "進行中")).status).toBe(200);
  expect((await rows.get(alice(), tasks(), nobody.id)).data.status).toBe("進行中");
});

test("権限: charlie はどれも読めず存在も分からない。bob はワークフロー・名簿・議事録の処理を読めない", async () => {
  test.setTimeout(240_000);
  const w = await tasksOf(placed.weekly);
  const runId = (await eventRunsFor(alice(), wf(), placed.weekly, "create"))[0].run_id;
  await expectDenied(bob(), [
    { path: `/workflows/${wf()}`, what: "議事録からタスク" },
    { path: `/workflows/${wf()}/runs/${runId}`, what: "その run" },
    { method: "POST", path: `/data/tables/${W.tables.members.id}/query`, data: { limit: 1 }, what: "名簿" },
    { method: "POST", path: `/data/tables/${log()}/query`, data: { limit: 1 }, what: "議事録の処理" },
  ]);
  await expectOutsiderSeesNothing(P.charlie.page, W, { workflowRuns: [["tasks", runId]] });
  await expectDenied(P.charlie.page, [
    ...Object.entries(placed).map(([mark, id]) => ({ path: `/files/${id}/text`, what: `置いた議事録 ${mark}` })),
    ...Object.values(w).map((r) => ({ path: `/data/tables/${tasks()}/records/${r.id}`, what: `タスク ${String(r.data.title)}` })),
    {
      method: "POST",
      path: `/data/tables/${tasks()}/records/${w["見積書のひな形を直す"].id}/transition`,
      data: { to: "進行中", expected_rev: w["見積書のひな形を直す"].rev },
      what: "タスクを進める",
    },
  ]);
});
