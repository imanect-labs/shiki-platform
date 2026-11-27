import { expect, test, type Page } from "@playwright/test";

import { cleanupQueue, loginViaKeycloak, removeWorkflowApi, uniqueName } from "./helpers";

/// 「日付を計算」と予定の日時の e2e（GAP-15）:
/// - 設定の欄で「ずらす数」「ずらす単位」「期間」「週の初め」を選ぶと、既定でない値だけが定義に入る。手動で実行すると、
///   実行した日から数えた先週（日曜始まり）の最初の日と最後の日が結果に入る。
/// - 後のブロックの差し込みの候補に、このブロックの結果（start・end）と、実行情報の「予定の日時」が出る。
/// - 日付として読めない「基準の日」は、欄の下に理由を出して保存を止める。
/// - スケジュールで動いた実行では、予定の日時は分の境目の予定の時刻で、「日付を計算」はそこから数える。実行履歴の一覧と
///   詳細にも、その予定の日時を出す（GAP-89）。
/// - スマホ幅の一覧から開いた設定でも同じ欄が出て、横にはみ出さない。
/// 前提: shiki-server が SHIKI__WORKFLOW__ENABLED=true（AI のブロックは使わない）。作ったワークフローは最後に消す。

const SHOTS = process.env.E2E_SHOTS_DIR;

const cleanup = cleanupQueue();
test.afterEach(() => cleanup.run());

async function csrfHeaders(page: Page): Promise<Record<string, string>> {
  const csrf = (await page.context().cookies()).find((c) => c.name === "shiki_csrf")?.value ?? "";
  return { "x-csrf-token": csrf, "content-type": "application/json" };
}

type IrNode = { id: string; type: string; params: Record<string, unknown> };

async function savedParams(page: Page, id: string, node: string): Promise<Record<string, unknown>> {
  const res = await page.request.get(`/api/workflows/${id}`);
  expect(res.ok(), await res.text()).toBeTruthy();
  const ir = ((await res.json()) as { ir: { nodes: IrNode[] } }).ir;
  return ir.nodes.find((n) => n.id === node)?.params ?? {};
}

async function createWorkflow(page: Page, ir: Record<string, unknown>): Promise<string> {
  const name = uniqueName("wf-date");
  const res = await page.request.post("/api/workflows", {
    headers: await csrfHeaders(page),
    data: { ir: { ir_version: 1, name, display_name: name, edges: [], ...ir } },
  });
  expect(res.ok(), await res.text()).toBeTruthy();
  const id = ((await res.json()) as { id: string }).id;
  cleanup.add(() => removeWorkflowApi(page, id));
  return id;
}

async function stepOutput(page: Page, id: string, runId: string, node: string): Promise<Record<string, unknown>> {
  const res = await page.request.get(`/api/workflows/${id}/runs/${runId}/steps?path=${node}`);
  expect(res.ok(), await res.text()).toBeTruthy();
  return ((await res.json()) as { output: Record<string, unknown> }).output;
}

async function waitEnd(page: Page, id: string, runId: string): Promise<string> {
  let status = "";
  await expect
    .poll(
      async () => {
        status = ((await (await page.request.get(`/api/workflows/${id}/runs/${runId}`)).json()) as { status: string })
          .status;
        return ["succeeded", "failed"].includes(status);
      },
      { timeout: 90_000 },
    )
    .toBe(true);
  return status;
}

/// 日付（YYYY-MM-DD）を n 日ずらす（暦の日付で数える）。
function addDays(day: string, n: number): string {
  const d = new Date(`${day}T00:00:00Z`);
  d.setUTCDate(d.getUTCDate() + n);
  return d.toISOString().slice(0, 10);
}

/// 日曜始まりの週の最初の日。
function sundayOf(day: string): string {
  return addDays(day, -new Date(`${day}T00:00:00Z`).getUTCDay());
}

test("日付を計算: 選んだ欄だけが定義に入り、先週（日曜始まり）の最初と最後の日を求め、後のブロックで選べる（GAP-15）", async ({
  page,
  browser,
}) => {
  test.setTimeout(240_000);
  await page.setViewportSize({ width: 1440, height: 900 });
  await loginViaKeycloak(page);
  page.on("dialog", (d) => void d.accept());
  const id = await createWorkflow(page, {
    triggers: [{ kind: "interactive" }],
    nodes: [
      { id: "last_week", type: "transform.date", label: "先週", params: {} },
      {
        id: "reply",
        type: "workflow.reply",
        label: "結果を返す",
        params: {
          format: "text",
          value: { $template: "週報-{d}.md", vars: { d: { $from: "nodes.last_week.output", path: "/start" } } },
        },
      },
    ],
    edges: [{ from: "last_week", to: "reply" }],
  });

  await page.goto(`/workflows/${id}`);
  const panel = page.getByRole("complementary", { name: "ブロックの設定" });
  await page.getByTestId("rf__node-last_week").click({ position: { x: 60, y: 24 } });
  await expect(panel.getByText("基準の日（省略可）")).toBeVisible({ timeout: 30_000 });
  await expect(panel.getByText(/空欄なら、この実行の予定の日時から数えます/)).toBeVisible();
  // ずらす数を入れるまでは単位の欄を出さない。
  await expect(panel.getByRole("combobox", { name: "ずらす単位" })).toHaveCount(0);
  await panel.locator('[data-value-expr="ずらす数（省略可）"]').getByRole("textbox").fill("-1");
  await panel.getByRole("combobox", { name: "ずらす単位" }).click();
  await page.getByRole("option", { name: "週", exact: true }).click();
  await panel.getByRole("combobox", { name: "期間" }).click();
  await page.getByRole("option", { name: "週", exact: true }).click();
  await panel.getByRole("combobox", { name: "週の初め" }).click();
  await page.getByRole("option", { name: "日曜日" }).click();
  await expect(panel.getByRole("combobox", { name: "時間帯" })).toHaveText("日本時間（おすすめ）");
  await expect(panel.getByTestId("date-result-note")).toBeVisible();
  if (SHOTS) await page.screenshot({ path: `${SHOTS}/date-panel-pc.png` });

  // 読めない基準の日は、欄の下に理由を出して保存を止める。
  const base = panel.locator('[data-value-expr="基準の日（省略可）"]').getByRole("textbox");
  await base.fill("2026/11/02");
  await page.getByRole("button", { name: "保存", exact: true }).click();
  await expect(panel.getByText(/「基準の日」は、日付（2026-11-02）か時差つきの日時/)).toBeVisible({ timeout: 15_000 });
  if (SHOTS) await page.screenshot({ path: `${SHOTS}/date-base-error-pc.png` });
  await base.fill("");
  const saved = page.waitForResponse((r) => r.request().method() === "PUT" && /\/api\/workflows\/[^/]+$/.test(r.url()));
  await page.getByRole("button", { name: "保存", exact: true }).click();
  expect((await saved).ok()).toBeTruthy();
  // 既定（日本時間）と空の基準の日は書かない。
  expect(await savedParams(page, id, "last_week")).toEqual({
    shift: { amount: -1, unit: "week" },
    period: "week",
    week_starts_on: "sunday",
  });

  // 後のブロックの差し込みの候補に、このブロックの結果と、実行情報の予定の日時が出る。
  await page.getByTestId("rf__node-reply").click({ position: { x: 60, y: 24 } });
  await panel.getByTestId("insert-var").first().click();
  await expect(page.getByRole("menuitem", { name: "「先週」の start", exact: true })).toBeVisible();
  await expect(page.getByRole("menuitem", { name: "「先週」の end", exact: true })).toBeVisible();
  await expect(page.getByRole("menuitem", { name: /予定の日時（scheduled_at）/ })).toBeVisible();
  if (SHOTS) await page.screenshot({ path: `${SHOTS}/date-candidates-pc.png` });
  await page.keyboard.press("Escape");

  // 手動で実行すると、実行した日（日本時間）から数えた先週の日曜〜土曜。
  const run = await page.request.post(`/api/workflows/${id}/runs`, { headers: await csrfHeaders(page), data: { input: {} } });
  expect(run.status(), await run.text()).toBe(202);
  const runId = ((await run.json()) as { run_id: string }).run_id;
  expect(await waitEnd(page, id, runId)).toBe("succeeded");
  const out = await stepOutput(page, id, runId, "last_week");
  const today = new Date().toLocaleDateString("sv-SE", { timeZone: "Asia/Tokyo" });
  const start = sundayOf(addDays(today, -7));
  expect(out).toMatchObject({ date: addDays(today, -7), start, end: addDays(start, 6) });
  expect(out.start_at).toBe(`${start}T00:00:00+09:00`);
  expect(out.next_start_at).toBe(`${addDays(start, 7)}T00:00:00+09:00`);
  expect((await stepOutput(page, id, runId, "reply")).text).toBe(`週報-${start}.md`);
  await page.goto(`/workflows/${id}/runs?run=${runId}`);
  await expect(page.getByRole("dialog").getByText("先週").first()).toBeVisible({ timeout: 30_000 });
  if (SHOTS) await page.screenshot({ path: `${SHOTS}/date-run-pc.png` });

  // スマホ幅: 一覧から開いた設定でも同じ欄が出て、横にはみ出さない。
  const phoneCtx = await browser.newContext({
    storageState: await page.context().storageState(),
    viewport: { width: 390, height: 844 },
    hasTouch: true,
    isMobile: true,
    deviceScaleFactor: 2,
    ignoreHTTPSErrors: true,
  });
  cleanup.add(() => phoneCtx.close());
  const phone = await phoneCtx.newPage();
  await phone.goto(`/workflows/${id}`);
  const list = phone.getByTestId("workflow-mobile-flow");
  await expect(list).toBeVisible({ timeout: 15_000 });
  await list.getByRole("button", { name: / 先週$/ }).first().click();
  const sheet = phone.getByRole("dialog").filter({ hasText: "このブロックを削除" });
  await expect(sheet.getByRole("combobox", { name: "週の初め" })).toHaveText("日曜日");
  await expect(sheet.getByRole("combobox", { name: "ずらす単位" })).toHaveText("週");
  await sheet.getByRole("combobox", { name: "時間帯" }).scrollIntoViewIfNeeded();
  expect(await sheet.evaluate((el) => el.scrollWidth <= el.clientWidth + 1)).toBe(true);
  if (SHOTS) await phone.screenshot({ path: `${SHOTS}/date-panel-phone.png` });
  await sheet.getByRole("button", { name: "閉じる" }).click();
});

test("スケジュールの実行: 予定の日時は分の境目の予定の時刻で、日付を計算はそこから数える（GAP-15）", async ({ page }) => {
  test.setTimeout(240_000);
  await loginViaKeycloak(page);
  const id = await createWorkflow(page, {
    declared_scopes: [],
    // 毎分（すぐ動かす）。
    triggers: [{ kind: "schedule", cron: "* * * * *", tz: "Asia/Tokyo", catchup: "skip" }],
    nodes: [
      { id: "today", type: "transform.date", label: "予定の日", params: {} },
      {
        id: "seen",
        type: "script.run",
        label: "実行情報を見る",
        params: {
          source: { inline: "function main(input) { return input; }" },
          input: { run: { $from: "run" }, trigger: { $from: "trigger" } },
        },
      },
    ],
    edges: [{ from: "today", to: "seen" }],
  });
  cleanup.add(async () => {
    const off = await page.request.post(`/api/workflows/${id}/disable`, {
      headers: await csrfHeaders(page),
      data: { revoke_delegations: true },
    });
    expect([200, 404], await off.text()).toContain(off.status());
  });
  const on = await page.request.post(`/api/workflows/${id}/enable`, {
    headers: await csrfHeaders(page),
    data: { version: 1, consent: true, grants: [] },
  });
  expect(on.ok(), await on.text()).toBeTruthy();

  let runId = "";
  await expect
    .poll(
      async () => {
        const items = ((await (await page.request.get(`/api/workflows/${id}/runs`)).json()) as {
          items: { run_id: string; trigger_kind: string }[];
        }).items;
        runId = items.find((r) => r.trigger_kind === "schedule")?.run_id ?? "";
        return runId !== "";
      },
      { timeout: 150_000, intervals: [5_000], message: "毎分のスケジュールで実行される" },
    )
    .toBe(true);
  // 次の分の実行を待たない（片付けで止める）。
  const off = await page.request.post(`/api/workflows/${id}/disable`, {
    headers: await csrfHeaders(page),
    data: { revoke_delegations: true },
  });
  expect(off.ok(), await off.text()).toBeTruthy();
  expect(await waitEnd(page, id, runId)).toBe("succeeded");
  const seen = (await stepOutput(page, id, runId, "seen")) as {
    run: { scheduled_at: string; started_at: string };
    trigger: unknown;
  };
  // 予定の時刻は分の境目（日本時間）。きっかけの内容は今までどおり空。
  expect(seen.run.scheduled_at).toMatch(/^\d{4}-\d{2}-\d{2}T\d{2}:\d{2}:00\+09:00$/);
  expect(Date.parse(seen.run.started_at)).toBeGreaterThanOrEqual(Date.parse(seen.run.scheduled_at));
  expect(seen.trigger).toBeNull();
  const today = await stepOutput(page, id, runId, "today");
  expect(today.date).toBe(seen.run.scheduled_at.slice(0, 10));

  // 実行履歴は、スケジュールの予定の時刻を予定の日時として一覧と詳細に出す（どの回の分か分かる・GAP-89）。
  const item = (
    (await (await page.request.get(`/api/workflows/${id}/runs`)).json()) as {
      items: { run_id: string; scheduled_at: string | null; scheduled_at_source: string | null }[];
    }
  ).items.find((r) => r.run_id === runId);
  expect(item?.scheduled_at_source).toBe("schedule");
  expect(Date.parse(item?.scheduled_at ?? "")).toBe(Date.parse(seen.run.scheduled_at));
  await page.setViewportSize({ width: 1440, height: 900 });
  await page.goto(`/workflows/${id}/runs?run=${runId}`);
  const sheet = page.getByRole("dialog").filter({ hasText: "実行の詳細" });
  await expect(sheet.getByText("予定の日時", { exact: true })).toBeVisible({ timeout: 30_000 });
  await expect(sheet.getByTestId("run-detail-scheduled-note")).toHaveCount(0);
  if (SHOTS) await page.screenshot({ path: `${SHOTS}/scheduled-at-schedule-detail-pc.png` });
  await sheet.getByRole("button", { name: "閉じる" }).click();
  await expect(page.locator("table").getByTestId("run-scheduled-at").first()).toHaveAttribute(
    "data-source",
    "schedule",
  );
});
