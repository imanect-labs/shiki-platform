import { expect, test, type Locator, type Page } from "@playwright/test";

import { cleanupQueue, loginViaKeycloak, removeWorkflowApi, uniqueName } from "./helpers";

/// 予定の日時を指定して試す・実行履歴の予定の日時・子の実行の引き継ぎの e2e（GAP-88・GAP-89・GAP-90）:
/// - スケジュールのきっかけがあるワークフローの実行のダイアログは、「詳しい設定」に予定の日時（日本時間）の欄を出し、
///   開いたときの日時で始める。変えなければ送らず（予定の日時は実行した日時のまま）、変えるとその日時で動く。
/// - 「日付を計算」はその日時から数え、「別のワークフローを実行」で始めた子の実行も同じ日時を引き継ぐ。
/// - 実行履歴の一覧（PC は「予定の日時」の列、スマホはカードの 3 行目）と詳細で、指定した予定の日時と、引き継いだ予定の
///   日時を言い分ける。予定の日時の無い実行には出さない。
/// 前提: shiki-server が SHIKI__WORKFLOW__ENABLED=true（AI のブロックは使わない）。作ったワークフローは最後に消す。

const SHOTS = process.env.E2E_SHOTS_DIR;

// 使う人の多い環境（日本の時刻・日本語の表示）で見る。実行履歴の日時はブラウザーの時間帯で出し、実行のダイアログの
// 予定の日時は日本時間で受けるので、ふだんの環境ではどちらも同じ時刻になる。
test.use({ locale: "ja-JP", timezoneId: "Asia/Tokyo" });

const cleanup = cleanupQueue();
test.afterEach(() => cleanup.run());

async function csrfHeaders(page: Page): Promise<Record<string, string>> {
  const csrf = (await page.context().cookies()).find((c) => c.name === "shiki_csrf")?.value ?? "";
  return { "x-csrf-token": csrf, "content-type": "application/json" };
}

async function createWorkflow(page: Page, name: string, ir: Record<string, unknown>): Promise<string> {
  const res = await page.request.post("/api/workflows", {
    headers: await csrfHeaders(page),
    data: { ir: { ir_version: 1, name, display_name: name, edges: [], ...ir } },
  });
  expect(res.ok(), await res.text()).toBeTruthy();
  const id = ((await res.json()) as { id: string }).id;
  cleanup.add(() => removeWorkflowApi(page, id));
  return id;
}

type RunItem = { run_id: string; status: string; scheduled_at: string | null; scheduled_at_source: string | null };

async function runs(page: Page, id: string): Promise<RunItem[]> {
  const res = await page.request.get(`/api/workflows/${id}/runs`);
  expect(res.ok(), await res.text()).toBeTruthy();
  return ((await res.json()) as { items: RunItem[] }).items;
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

async function stepOutput(page: Page, id: string, runId: string, node: string): Promise<Record<string, unknown>> {
  const res = await page.request.get(`/api/workflows/${id}/runs/${runId}/steps?path=${node}`);
  expect(res.ok(), await res.text()).toBeTruthy();
  return ((await res.json()) as { output: Record<string, unknown> }).output;
}

/// 「詳しい設定」を開く（開き直したダイアログは前に開いた状態のままなので、閉じていれば押す）。
async function openAdvanced(dialog: Locator): Promise<void> {
  const toggle = dialog.getByRole("button", { name: "詳しい設定" });
  if ((await toggle.getAttribute("aria-expanded")) !== "true") await toggle.click();
}

/// 実行のダイアログを開いて「実行する」を押し、送った本文と始めた実行の id を返す。
async function runFromDialog(
  page: Page,
  scheduled: string | null,
): Promise<{ body: Record<string, unknown>; runId: string }> {
  await page.getByRole("button", { name: "実行", exact: true }).click();
  const dialog = page.getByRole("dialog", { name: "いま実行する" });
  await expect(dialog).toBeVisible({ timeout: 30_000 });
  await openAdvanced(dialog);
  const field = dialog.getByLabel("予定の日時（日本時間）");
  // 開いたときの日時（日本時間）で始める。
  const today = new Date().toLocaleDateString("sv-SE", { timeZone: "Asia/Tokyo" });
  await expect(field).toHaveValue(new RegExp(`^${today}T\\d{2}:\\d{2}$`));
  if (scheduled !== null) await field.fill(scheduled);
  const sent = page.waitForRequest((r) => r.method() === "POST" && /\/api\/workflows\/[^/]+\/runs$/.test(r.url()));
  await dialog.getByRole("button", { name: "実行する" }).click();
  const body = (await sent).postDataJSON() as Record<string, unknown>;
  await page.waitForURL(/\/runs\?run=/, { timeout: 30_000 });
  return { body, runId: new URL(page.url()).searchParams.get("run")! };
}

test("予定の日時: 指定して試し、子の実行も引き継ぎ、実行履歴で言い分ける（GAP-88・GAP-89・GAP-90）", async ({
  page,
  browser,
}) => {
  test.setTimeout(300_000);
  await page.setViewportSize({ width: 1440, height: 900 });
  await loginViaKeycloak(page);
  const childName = uniqueName("wf-sched-child");
  const child = await createWorkflow(page, childName, {
    triggers: [{ kind: "interactive" }],
    nodes: [{ id: "day", type: "transform.date", label: "予定の日", params: {} }],
  });
  const parentName = uniqueName("wf-sched-parent");
  const parent = await createWorkflow(page, parentName, {
    declared_scopes: ["workflow.start"],
    // 自動実行はオンにしない（手動で試すだけ）。
    triggers: [{ kind: "schedule", cron: "0 9 * * 1", tz: "Asia/Tokyo", catchup: "skip" }],
    nodes: [
      {
        id: "last_week",
        type: "transform.date",
        label: "先週",
        params: { shift: { amount: -1, unit: "week" }, period: "week" },
      },
      { id: "start", type: "workflow.start", label: "子を実行", params: { name: childName } },
    ],
    edges: [{ from: "last_week", to: "start" }],
  });

  // --- 変えなければ予定の日時を送らない（実行した日時のまま）。予定の日時の無い実行には出さない。
  await page.goto(`/workflows/${parent}`);
  const plain = await runFromDialog(page, null);
  expect(plain.body).not.toHaveProperty("scheduled_at");
  expect(await waitEnd(page, parent, plain.runId)).toBe("succeeded");
  const sheet = page.getByRole("dialog").filter({ hasText: "実行の詳細" });
  await expect(sheet.getByText("ブロックごとの記録")).toBeVisible({ timeout: 30_000 });
  await expect(sheet.getByTestId("run-detail-scheduled-at")).toHaveCount(0);

  // --- 予定の日時を指定すると、その日時に動いたものとして先週を数える。
  await page.goto(`/workflows/${parent}`);
  await page.getByRole("button", { name: "実行", exact: true }).click();
  const dialog = page.getByRole("dialog", { name: "いま実行する" });
  await expect(
    dialog.getByText("このワークフローのきっかけは「スケジュール」ですが、ここでは試しに 1 回だけ実行します。"),
  ).toBeVisible();
  await openAdvanced(dialog);
  await dialog.getByLabel("予定の日時（日本時間）").fill("2026-11-09T09:00");
  await expect(dialog.getByText(/スケジュールがこの日時に動いたものとして実行し/)).toBeVisible();
  if (SHOTS) await page.screenshot({ path: `${SHOTS}/scheduled-at-dialog-pc.png` });
  await dialog.getByRole("button", { name: "閉じる" }).click();
  const trial = await runFromDialog(page, "2026-11-09T09:00");
  expect(trial.body.scheduled_at).toBe("2026-11-09T09:00");
  expect(await waitEnd(page, parent, trial.runId)).toBe("succeeded");
  const lastWeek = await stepOutput(page, parent, trial.runId, "last_week");
  expect(lastWeek).toMatchObject({ start: "2026-11-02", end: "2026-11-08" });

  // 詳細: 指定した予定の日時と、その言い分け。
  await expect(sheet.getByText("指定した予定の日時")).toBeVisible({ timeout: 30_000 });
  await expect(sheet.getByTestId("run-detail-scheduled-at")).toHaveText(
    new Date("2026-11-09T00:00:00Z").toLocaleString("ja-JP", {
      timeZone: "Asia/Tokyo",
      month: "numeric",
      day: "numeric",
      hour: "2-digit",
      minute: "2-digit",
      second: "2-digit",
    }),
  );
  await expect(sheet.getByTestId("run-detail-scheduled-note")).toHaveText(
    "手動で実行するときに指定した日時を、予定の日時として使いました。",
  );
  if (SHOTS) await page.screenshot({ path: `${SHOTS}/scheduled-at-detail-pc.png` });
  await sheet.getByRole("button", { name: "閉じる" }).click();
  await expect(sheet).toHaveCount(0);

  // 一覧: 「予定の日時」の列。指定した実行には「指定した日時」を添え、予定の日時の無い実行は「—」。
  const table = page.locator("table");
  await expect(table.locator("th", { hasText: "予定の日時" })).toBeVisible();
  const cell = table.getByTestId("run-scheduled-at");
  await expect(cell).toHaveCount(1);
  await expect(cell).toHaveAttribute("data-source", "specified");
  await expect(cell).toContainText("指定した日時");
  if (SHOTS) await page.screenshot({ path: `${SHOTS}/scheduled-at-list-pc.png` });
  const items = await runs(page, parent);
  expect(items.find((r) => r.run_id === trial.runId)).toMatchObject({
    scheduled_at: "2026-11-09T00:00:00+00:00",
    scheduled_at_source: "specified",
  });
  expect(items.find((r) => r.run_id === plain.runId)).toMatchObject({ scheduled_at: null, scheduled_at_source: null });

  // --- 子の実行は、指定した予定の日時を引き継ぐ（指定しなかった親の子は引き継がない）。
  let childRuns: RunItem[] = [];
  await expect
    .poll(
      async () => {
        childRuns = await runs(page, child);
        return childRuns.length === 2 && childRuns.every((r) => r.status === "succeeded");
      },
      { timeout: 90_000, message: "子の実行が 2 回とも終わる" },
    )
    .toBe(true);
  const inherited = childRuns.find((r) => r.scheduled_at_source === "parent");
  expect(inherited?.scheduled_at).toBe("2026-11-09T00:00:00+00:00");
  expect(childRuns.filter((r) => r.scheduled_at === null)).toHaveLength(1);
  expect((await stepOutput(page, child, inherited!.run_id, "day")).date).toBe("2026-11-09");
  await page.goto(`/workflows/${child}/runs?run=${inherited!.run_id}`);
  await expect(sheet.getByText("引き継いだ予定の日時")).toBeVisible({ timeout: 30_000 });
  await expect(sheet.getByTestId("run-detail-scheduled-note")).toHaveText(
    "「別のワークフローを実行」で始めた実行です。始めた実行の予定の日時を引き継いでいます。",
  );
  if (SHOTS) await page.screenshot({ path: `${SHOTS}/scheduled-at-child-detail-pc.png` });
  await sheet.getByRole("button", { name: "閉じる" }).click();
  await expect(table.getByTestId("run-scheduled-at")).toHaveAttribute("data-source", "parent");
  await expect(table.getByTestId("run-scheduled-at")).toContainText("引き継いだ日時");

  // --- スマホ幅: 一覧のカードに予定の日時を出し、ダイアログの欄も横にはみ出さない。
  const phoneCtx = await browser.newContext({
    storageState: await page.context().storageState(),
    viewport: { width: 390, height: 844 },
    hasTouch: true,
    isMobile: true,
    deviceScaleFactor: 2,
    ignoreHTTPSErrors: true,
    locale: "ja-JP",
    timezoneId: "Asia/Tokyo",
  });
  cleanup.add(() => phoneCtx.close());
  const phone = await phoneCtx.newPage();
  await phone.goto(`/workflows/${parent}/runs`);
  const card = phone.getByRole("list", { name: "実行の一覧" }).getByTestId("run-scheduled-at");
  await expect(card).toHaveCount(1, { timeout: 30_000 });
  await expect(card).toHaveAttribute("data-source", "specified");
  await expect(card).toContainText("指定した予定の日時:");
  if (SHOTS) await phone.screenshot({ path: `${SHOTS}/scheduled-at-list-phone.png` });
  await phone.goto(`/workflows/${parent}/runs?run=${trial.runId}`);
  const phoneSheet = phone.getByRole("dialog").filter({ hasText: "実行の詳細" });
  await expect(phoneSheet.getByText("指定した予定の日時")).toBeVisible({ timeout: 30_000 });
  expect(await phoneSheet.evaluate((el) => el.scrollWidth <= el.clientWidth + 1)).toBe(true);
  if (SHOTS) await phone.screenshot({ path: `${SHOTS}/scheduled-at-detail-phone.png` });
  await phoneSheet.getByRole("button", { name: "閉じる" }).click();
  await phone.goto(`/workflows/${parent}`);
  await phone.getByRole("button", { name: "実行", exact: true }).click();
  const phoneDialog = phone.getByRole("dialog", { name: "いま実行する" });
  await openAdvanced(phoneDialog);
  await expect(phoneDialog.getByLabel("予定の日時（日本時間）")).toBeVisible();
  expect(await phoneDialog.evaluate((el) => el.scrollWidth <= el.clientWidth + 1)).toBe(true);
  if (SHOTS) await phone.screenshot({ path: `${SHOTS}/scheduled-at-dialog-phone.png` });
  await phoneDialog.getByRole("button", { name: "閉じる" }).click();
});
