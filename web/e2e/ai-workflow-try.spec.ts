import { expect, test, type Page } from "@playwright/test";

import { cleanupQueue, deleteTableApi, loginAs, loginViaKeycloak, removeWorkflowApi, uniqueName } from "./helpers";

/// 会話の AI の試運転（try_workflow・T04・D-10・D-197）の E2E（前提: LLM=stub）。
///
/// - 承認カードに、試すワークフローの名前・版・作成者・入力・書くテーブル・「結果は実データとして残ります」が出る。
///   キーボードで承認すると、話した本人の権限で版を固定した run が 1 本でき、テーブルに【試運転】の行が増える。
/// - 別の会話で却下すると、run も行も増えない。
/// - 閲覧だけ共有された人のカードは、作成者の名前と「閲覧のみのワークフローは試運転できません」を出し、承認しても
///   run はできない（書く先のテーブルも名前を出さない）。
///
/// チャットはツールの結果を描画しないので、結果は会話の文でなく、実行履歴（API と画面）とテーブルの本当の状態で
/// 確かめる。stub 駆動: `calltool:<ツール名> <入力 JSON>` でツールを決定的に呼ぶ。

const SHOTS = process.env.E2E_SHOTS_DIR;

const cleanup = cleanupQueue();
test.afterEach(() => cleanup.run());

async function csrfHeaders(page: Page): Promise<Record<string, string>> {
  const csrf = (await page.context().cookies()).find((c) => c.name === "shiki_csrf")?.value ?? "";
  return { "x-csrf-token": csrf, "content-type": "application/json" };
}

type Setup = { tableId: string; tableName: string; workflowId: string; workflowName: string };

/// テーブルと、そのテーブルに入力の件名でレコードを足すワークフロー（v1）を作る。
async function setup(page: Page): Promise<Setup> {
  const tableName = uniqueName("e2e-試運転の記録");
  const t = await page.request.post("/api/data/tables", {
    headers: await csrfHeaders(page),
    data: { name: tableName, schema: { fields: [{ key: "title", display_name: "件名", type: "text" }] } },
  });
  expect(t.status(), await t.text()).toBe(201);
  const tableId = ((await t.json()) as { id: string }).id;
  cleanup.add(() => deleteTableApi(page, tableId));
  // name は英小文字の参照名（カードはこの名前を出す）。
  const workflowName = uniqueName("e2e-try-flow");
  const w = await page.request.post("/api/workflows", {
    headers: await csrfHeaders(page),
    data: {
      ir: {
        ir_version: 1,
        name: workflowName,
        display_name: "試運転の確かめ",
        triggers: [{ kind: "interactive" }],
        nodes: [
          {
            id: "add_trial_record",
            type: "data.record.create",
            label: "記録する",
            params: { table: tableId, data: { title: { $from: "input", path: "/title" } } },
          },
        ],
        edges: [],
      },
    },
  });
  expect(w.ok(), await w.text()).toBeTruthy();
  const workflowId = ((await w.json()) as { id: string }).id;
  cleanup.add(() => removeWorkflowApi(page, workflowId));
  return { tableId, tableName, workflowId, workflowName };
}

type RunRow = { run_id: string; status: string; version: number; trigger_kind: string };

async function runsOf(page: Page, workflowId: string): Promise<RunRow[]> {
  const res = await page.request.get(`/api/workflows/${workflowId}/runs`);
  expect(res.ok(), await res.text()).toBeTruthy();
  return ((await res.json()) as { items: RunRow[] }).items;
}

async function titlesOf(page: Page, tableId: string): Promise<string[]> {
  const res = await page.request.get(`/api/data/tables/${tableId}/records`);
  expect(res.ok(), await res.text()).toBeTruthy();
  return ((await res.json()) as { items: { data: { title?: string } }[] }).items.map((r) => r.data.title ?? "");
}

/// 新しい会話で試運転を頼む（`/` から送る）。
async function askTry(page: Page, workflowId: string, title: string) {
  await page.goto("/");
  const input = page.getByLabel("メッセージを入力");
  await input.fill(`calltool:try_workflow {"workflow_id":"${workflowId}","version":1,"input":{"title":"${title}"}}`);
  await page.getByRole("button", { name: "送信" }).click();
  await page.waitForURL(/\/c\/[0-9a-f-]+/i, { timeout: 20_000 });
}

/// 生成が終わるまで待つ（停止のボタンが消える）。
async function waitAnswered(page: Page) {
  await expect(page.getByRole("button", { name: "生成を停止" })).toHaveCount(0, { timeout: 60_000 });
}

test("承認カードに試すものと起きることが出て、承認すると run が 1 本でき、却下すると増えない", async ({ page }) => {
  test.setTimeout(240_000);
  await page.setViewportSize({ width: 1440, height: 900 });
  await loginViaKeycloak(page);
  const s = await setup(page);
  const title = `【試運転】${uniqueName("件名")}`;

  await askTry(page, s.workflowId, title);
  const card = page.getByTestId("approval-card");
  const summary = card.getByTestId("try-workflow-summary");
  await expect(summary).toHaveAttribute("data-state", "ready", { timeout: 30_000 });
  await expect(summary).toContainText(`ワークフロー「${s.workflowName}」の v1 を試運転します。`);
  await expect(summary).toContainText(s.workflowId);
  await expect(summary.getByTestId("try-workflow-owner")).toHaveText("自分");
  // 入力と、書くテーブル（本人が見られるので名前で出す）。
  await expect(summary).toContainText(title);
  await expect(summary.getByTestId("try-workflow-effects")).toContainText(`テーブル「${s.tableName}」`);
  await expect(summary.getByTestId("try-workflow-effects")).toContainText("データを追加");
  await expect(summary.getByTestId("try-workflow-real-data")).toContainText("結果は実データとして残ります");
  await expect(summary.getByTestId("try-workflow-view-only")).toHaveCount(0);
  // 内部の名前（ブロックの ID）は出さない。生の JSON は畳んで残す。
  await expect(summary).not.toContainText("add_trial_record");
  await expect(card.getByText("送る内容（JSON）")).toBeVisible();
  if (SHOTS) await card.screenshot({ path: `${SHOTS}/try-approval-card-pc.png` });

  // 承認するまで run はできない。
  expect(await runsOf(page, s.workflowId)).toHaveLength(0);
  // キーボードで承認する（畳んだ JSON の次に「承認して続行」へ届く）。
  const approve = card.getByRole("button", { name: "承認して続行" });
  await card.locator("summary").focus();
  await page.keyboard.press("Tab");
  await expect(approve).toBeFocused();
  await page.keyboard.press("Enter");

  // 本当の状態で確かめる: 版を固定した手動の run が 1 本成功し、テーブルに【試運転】の行が 1 つ増える。
  await expect
    .poll(async () => (await runsOf(page, s.workflowId)).map((r) => r.status), { timeout: 90_000 })
    .toEqual(["succeeded"]);
  const [run] = await runsOf(page, s.workflowId);
  expect(run).toMatchObject({ version: 1, trigger_kind: "interactive" });
  expect(await titlesOf(page, s.tableId)).toEqual([title]);
  await expect(page.getByText("ワークフローの v1 を試運転しました").first()).toBeVisible({ timeout: 60_000 });
  await waitAnswered(page);

  // 実行履歴の画面にも 1 本の成功した run が出る。
  await page.goto(`/workflows/${s.workflowId}/runs`);
  await expect(page.locator("table tbody tr")).toHaveCount(1, { timeout: 30_000 });
  await expect(page.locator("table tbody tr").first()).toContainText("成功");

  // 別の会話で却下すると、run も行も増えない。
  await askTry(page, s.workflowId, title);
  const card2 = page.getByTestId("approval-card");
  await expect(card2.getByTestId("try-workflow-summary")).toHaveAttribute("data-state", "ready", { timeout: 30_000 });
  await card2.getByRole("button", { name: "却下" }).click();
  await expect(page.getByText("ワークフローの v1 の試運転を却下しました").first()).toBeVisible({ timeout: 60_000 });
  await waitAnswered(page);
  expect(await runsOf(page, s.workflowId)).toHaveLength(1);
  expect(await titlesOf(page, s.tableId)).toEqual([title]);
});

test("閲覧だけ共有された人のカードは作成者の名前と閲覧のみを出し、承認しても run はできない", async ({ page, browser }) => {
  test.setTimeout(240_000);
  await page.setViewportSize({ width: 1440, height: 900 });
  await loginViaKeycloak(page);
  const s = await setup(page);

  const bobCtx = await browser.newContext({ viewport: { width: 1440, height: 900 } });
  cleanup.add(() => bobCtx.close());
  const bob = await bobCtx.newPage();
  await loginAs(bob, "bob");
  const bobId = ((await (await bob.request.get("/api/me")).json()) as { id: string }).id;
  const share = await page.request.put(`/api/artifacts/${s.workflowId}/shares`, {
    headers: await csrfHeaders(page),
    data: { target: { type: "user", id: bobId }, role: "viewer" },
  });
  expect(share.ok(), await share.text()).toBeTruthy();

  await askTry(bob, s.workflowId, `【試運転】${uniqueName("閲覧のみ")}`);
  const card = bob.getByTestId("approval-card");
  const summary = card.getByTestId("try-workflow-summary");
  await expect(summary).toHaveAttribute("data-state", "ready", { timeout: 30_000 });
  // 作成者は自分ではない（表示名が引けなければ id のまま）。
  await expect(summary.getByTestId("try-workflow-owner")).not.toHaveText("自分");
  await expect(summary.getByTestId("try-workflow-owner")).not.toHaveText("");
  await expect(summary.getByTestId("try-workflow-view-only")).toHaveText(
    "閲覧のみのワークフローは試運転できません。承認しても実行されません。",
  );
  // bob はテーブルを見られないので、名前を出さない。
  await expect(summary.getByTestId("try-workflow-effects")).not.toContainText(s.tableName);
  await expect(summary.getByTestId("try-workflow-effects")).toContainText("読めないテーブル");
  if (SHOTS) await card.screenshot({ path: `${SHOTS}/try-approval-card-view-only.png` });

  await card.getByRole("button", { name: "承認して続行" }).click();
  await expect(bob.getByText("ワークフローの v1 を試運転できませんでした").first()).toBeVisible({ timeout: 60_000 });
  await waitAnswered(bob);
  expect(await runsOf(page, s.workflowId)).toHaveLength(0);
  expect(await titlesOf(page, s.tableId)).toEqual([]);
});
