import { expect, test, type Locator, type Page } from "@playwright/test";

import { cleanupQueue, deleteTableApi, loginAs, loginViaKeycloak, removeWorkflowApi, uniqueName } from "./helpers";

/// 会話の AI の試運転（try_workflow・T04・D-10・D-197）の E2E（前提: LLM=stub）。
///
/// - 承認カードに、試すワークフローの名前・版・作成者・入力・書くテーブル・「結果は実データとして残ります」が出る。
///   キーボードで承認すると、話した本人の権限で版を固定した run が 1 本でき、テーブルに【試運転】の行が増える。
/// - 別の会話で却下すると、run も行も増えない。
/// - 閲覧だけ共有された人のカードは、作成者の名前と「今は閲覧のみのため、試運転できません」を出し、承認しても
///   run はできない（書く先のテーブルも名前を出さない）。承認までに編集できるようになれば実行されるので、言い切らず、
///   実データの注意も出す。
/// - スクリプトを含む版のカードは、スクリプトの中でできること（宣言スコープ）を出す。スキルは「名前（v版）」で出す。
/// - 名前で指定した・版が無い・入力が読めない・下見を読み込めない、をそれぞれ言い分ける。
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
  await askTryRaw(page, { workflow_id: workflowId, version: 1, input: { title } });
}

/// 新しい会話で、道具の入力をそのまま渡して試運転を頼む（読めない入力・名前の指定などを作る）。
async function askTryRaw(page: Page, args: Record<string, unknown>) {
  await page.goto("/");
  const input = page.getByLabel("メッセージを入力");
  await input.fill(`calltool:try_workflow ${JSON.stringify(args)}`);
  await page.getByRole("button", { name: "送信" }).click();
  await page.waitForURL(/\/c\/[0-9a-f-]+/i, { timeout: 20_000 });
}

/// 生成が終わるまで待つ（停止のボタンが消える）。
async function waitAnswered(page: Page) {
  await expect(page.getByRole("button", { name: "生成を停止" })).toHaveCount(0, { timeout: 60_000 });
}

/// 画面に戻ってきたことにして（focus）、カードの今の文を返す（読み直しは 1 秒に 1 回なので、poll で繰り返す）。
async function refocus(page: Page, summary: Locator): Promise<string> {
  await page.evaluate(() => window.dispatchEvent(new Event("focus")));
  return (await summary.textContent()) ?? "";
}

/// カードを却下して、会話が終わるまで待つ（承認を待つ生成を残さない）。
async function decline(page: Page) {
  await page.getByTestId("approval-card").getByRole("button", { name: "却下" }).click();
  await expect(page.getByText(/試運転を却下しました/).first()).toBeVisible({ timeout: 60_000 });
  await waitAnswered(page);
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
  // スクリプトもスキルも無いので、その中でできることは出さない。
  await expect(summary.getByTestId("try-workflow-scopes")).toHaveCount(0);
  // 内部の名前（ブロックの ID）は出さない。生の JSON は畳んで残す。
  await expect(summary).not.toContainText("add_trial_record");
  await expect(card.getByText("送る内容（JSON）")).toBeVisible();
  if (SHOTS) {
    await card.screenshot({ path: `${SHOTS}/try-approval-card-pc.png` });
    // 狭い幅（pr スキルの 768px）と暗い表示でも撮る。
    await page.setViewportSize({ width: 768, height: 900 });
    await card.screenshot({ path: `${SHOTS}/try-approval-card-768.png` });
    await page.emulateMedia({ colorScheme: "dark" });
    await card.screenshot({ path: `${SHOTS}/try-approval-card-768-dark.png` });
    await page.emulateMedia({ colorScheme: "light" });
    await page.setViewportSize({ width: 1440, height: 900 });
  }

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
  // 承認の時点の権限で決まるので、「実行されない」とは言い切らない。実データの注意も出す。
  const viewOnly = summary.getByTestId("try-workflow-view-only");
  await expect(viewOnly).toContainText("今は閲覧のみのため、試運転できません。却下してください。");
  await expect(viewOnly).toContainText("承認するまでに編集できるようになった場合は、承認すると実行されます。");
  await expect(viewOnly).not.toContainText("承認しても実行されません");
  await expect(summary.getByTestId("try-workflow-real-data")).toContainText("結果は実データとして残ります");
  // bob はテーブルを見られないので、名前を出さない。
  await expect(summary.getByTestId("try-workflow-effects")).not.toContainText(s.tableName);
  await expect(summary.getByTestId("try-workflow-effects")).toContainText("読めないテーブル");
  if (SHOTS) await card.screenshot({ path: `${SHOTS}/try-approval-card-view-only.png` });

  // 画面に戻ってきたら読み直す: 開いている間に編集できるようになれば、試運転すると出す。
  const edit = await page.request.put(`/api/artifacts/${s.workflowId}/shares`, {
    headers: await csrfHeaders(page),
    data: { target: { type: "user", id: bobId }, role: "editor" },
  });
  expect(edit.ok(), await edit.text()).toBeTruthy();
  await expect
    .poll(async () => (await refocus(bob, summary)).includes(`ワークフロー「${s.workflowName}」の v1 を試運転します。`), {
      timeout: 15_000,
    })
    .toBe(true);
  await expect(summary.getByTestId("try-workflow-view-only")).toHaveCount(0);
  // 編集の共有を外して閲覧のみに戻してから承認する（入口は承認の時点の権限で断る）。
  const back = await page.request.delete(`/api/artifacts/${s.workflowId}/shares`, {
    headers: await csrfHeaders(page),
    data: { target: { type: "user", id: bobId }, role: "editor" },
  });
  expect(back.ok(), await back.text()).toBeTruthy();
  await expect
    .poll(async () => (await refocus(bob, summary)).includes("今は閲覧のみのため"), { timeout: 15_000 })
    .toBe(true);

  await card.getByRole("button", { name: "承認して続行" }).click();
  await expect(bob.getByText("ワークフローの v1 を試運転できませんでした").first()).toBeVisible({ timeout: 60_000 });
  await waitAnswered(bob);
  expect(await runsOf(page, s.workflowId)).toHaveLength(0);
  expect(await titlesOf(page, s.tableId)).toEqual([]);
});

test("スクリプトを含む版のカードは中でできることを出し、スキルは名前と版で出す", async ({ page }) => {
  test.setTimeout(240_000);
  await page.setViewportSize({ width: 1440, height: 900 });
  await loginViaKeycloak(page);
  const name = uniqueName("e2e-try-script");
  const w = await page.request.post("/api/workflows", {
    headers: await csrfHeaders(page),
    data: {
      ir: {
        ir_version: 1,
        name,
        display_name: "スクリプトの試運転",
        declared_scopes: ["data.write", "http.egress"],
        triggers: [{ kind: "interactive" }],
        nodes: [
          { id: "tidy", type: "script.run", label: "整える", params: { source: { inline: "function main(i){ return i; }" } } },
        ],
        edges: [],
      },
    },
  });
  expect(w.ok(), await w.text()).toBeTruthy();
  const id = ((await w.json()) as { id: string }).id;
  cleanup.add(() => removeWorkflowApi(page, id));

  // スクリプトの中の書き込み・送信は一覧に出ないので、宣言スコープを「中でできること」として出す。
  await askTryRaw(page, { workflow_id: id, version: 1 });
  const summary = page.getByTestId("approval-card").getByTestId("try-workflow-summary");
  await expect(summary).toHaveAttribute("data-state", "ready", { timeout: 30_000 });
  await expect(summary.getByTestId("try-workflow-effects")).toContainText("スクリプトを実行する");
  const scopes = summary.getByTestId("try-workflow-scopes");
  await expect(scopes).toContainText("スクリプト・スキルの中でできること");
  await expect(scopes).toContainText("スクリプトは、あなたの権限で次の操作を行えます。");
  await expect(scopes).toContainText("業務データを書く");
  await expect(scopes).toContainText("外部サービスへ送信する");
  await expect(scopes).not.toContainText("data.write");
  if (SHOTS) await page.getByTestId("approval-card").screenshot({ path: `${SHOTS}/try-approval-card-script.png` });
  await decline(page);

  // スキルは内部の参照の書き方（skill:<名前>@<版>）でなく、エディタと同じ「名前（v版）」で出す。保存するには
  // スキルを入れる必要があるので、下見の応答にスキルのブロックを足して確かめる。
  await page.route("**/workflows/try-preview**", async (route) => {
    const res = await route.fetch();
    const body = (await res.json()) as { effects: { compute: Record<string, unknown>[] } };
    body.effects.compute.push({ node_id: "check", kind: "skill", skill: "skill:invoice-check@3" });
    await route.fulfill({ response: res, json: body });
  });
  await askTryRaw(page, { workflow_id: id, version: 1 });
  await expect(summary).toHaveAttribute("data-state", "ready", { timeout: 30_000 });
  await expect(summary.getByTestId("try-workflow-effects")).toContainText("スキル「invoice-check」（v3）を使う");
  await expect(summary.getByTestId("try-workflow-effects")).not.toContainText("skill:invoice-check@3");
  await expect(scopes).toContainText("スキルは、この中でスキルが決めた範囲だけ");
  await page.unrouteAll({ behavior: "ignoreErrors" });
  await decline(page);
  expect(await runsOf(page, id)).toHaveLength(0);
});

test("名前の指定・無い版・読めない入力・読み込めないときは、それぞれ言い分けて試運転しない", async ({ page }) => {
  test.setTimeout(300_000);
  await page.setViewportSize({ width: 1440, height: 900 });
  await loginViaKeycloak(page);
  const s = await setup(page);
  const card = page.getByTestId("approval-card");
  const summary = card.getByTestId("try-workflow-summary");

  // 名前で指定した（道具は名前では起動しない・承認を待つ間に名前が付け替わると別のものが走る）。
  await askTryRaw(page, { workflow_id: s.workflowName, version: 1 });
  await expect(summary).toHaveAttribute("data-state", "blocked", { timeout: 30_000 });
  await expect(summary).toContainText(
    `「${s.workflowName}」は ID ではなく名前で指定されているため、承認しても実行されません。`,
  );
  await expect(summary.getByTestId("try-workflow-real-data")).toHaveCount(0);
  if (SHOTS) {
    await page.setViewportSize({ width: 768, height: 900 });
    await card.screenshot({ path: `${SHOTS}/try-approval-card-named-768.png` });
    await page.setViewportSize({ width: 1440, height: 900 });
  }
  await decline(page);

  // まだ無い版（口はワークフローが無い・見えない・版が無いを言い分けないので、版を添えてまとめて言う）。
  await askTryRaw(page, { workflow_id: s.workflowId, version: 99 });
  await expect(summary).toHaveAttribute("data-state", "missing", { timeout: 30_000 });
  await expect(summary).toContainText("ワークフローの v99 を読めませんでした。");
  await expect(summary).toContainText("このワークフローか、その版が無いか、見る権限がありません。");
  await decline(page);

  // 道具が断る入力は、口を読まずに「実行されません」と言う（道具と同じ読み方）。
  const unreadable: [Record<string, unknown>, string][] = [
    // JSON として読めない文字列（stub に渡す文にバックスラッシュを入れないよう、引用符の無い壊れた JSON にする）。
    [{ workflow_id: s.workflowId, version: 1, input: "{title" }, "入力が読めない形で渡されているため"],
    [{ workflow_id: s.workflowId, version: "0x2" }, "試運転する版が正しく指定されていないため"],
    [{ workflow_id: s.workflowId, version: 1, scheduled_at: 3 }, "予定の日時が読めない形で渡されているため"],
  ];
  for (const [args, text] of unreadable) {
    await askTryRaw(page, args);
    await expect(summary).toHaveAttribute("data-state", "blocked", { timeout: 30_000 });
    await expect(summary).toContainText(`${text}、承認しても実行されません。`);
    await decline(page);
  }

  // 下見を読み込めない → もう一度読み込むと出る。
  await page.route("**/workflows/try-preview**", (route) => route.fulfill({ status: 500, body: "boom" }));
  await askTry(page, s.workflowId, `【試運転】${uniqueName("読み込み")}`);
  await expect(summary).toHaveAttribute("data-state", "failed", { timeout: 30_000 });
  await expect(summary).toContainText("試運転の内容を読み込めませんでした。");
  if (SHOTS) await card.screenshot({ path: `${SHOTS}/try-approval-card-failed.png` });
  await page.unrouteAll({ behavior: "ignoreErrors" });
  await summary.getByRole("button", { name: "もう一度読み込む" }).click();
  await expect(summary).toHaveAttribute("data-state", "ready", { timeout: 30_000 });
  await expect(summary).toContainText(`ワークフロー「${s.workflowName}」の v1 を試運転します。`);
  await decline(page);

  expect(await runsOf(page, s.workflowId)).toHaveLength(0);
  expect(await titlesOf(page, s.tableId)).toEqual([]);
});
