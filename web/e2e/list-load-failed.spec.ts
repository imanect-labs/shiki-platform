import { expect, test, type Browser, type Page, type Route } from "@playwright/test";

import { cleanupQueue, loginViaKeycloak, removeWorkflowApi, uniqueName } from "./helpers";

/// 読み込みの失敗を空の状態に見せない（BUG-189・付録 C の P5）:
/// - バージョン履歴（「履歴がありません。」に見せない）・スキルストア（欄ごと消さない）・インストール済みのコード
///   アプリ（「ありません」に見せない）とインストールできるアプリの一覧（欄が無いことに見せない）・実行履歴（「まだ
///   実行していません」に見せない）。どれも読み込めなかったことと「もう一度読み込む」を出す。
/// 応答は route で差し替える。作ったもの（ワークフロー・ブラウザーの文脈）は最後に片付ける（失敗したテストでも走る）。

const cleanup = cleanupQueue();
test.afterEach(() => cleanup.run());

async function csrf(page: Page): Promise<Record<string, string>> {
  const token = (await page.context().cookies()).find((c) => c.name === "shiki_csrf")?.value ?? "";
  return { "x-csrf-token": token, "content-type": "application/json" };
}

/// `pattern` の GET に、`failures` 回だけ 500 を返し、その後は `body` を返す（`body` が無ければ本物へ通す）。
async function failThen(page: Page, pattern: RegExp, failures: number, body?: unknown) {
  let left = failures;
  const handler = async (route: Route) => {
    if (route.request().method() !== "GET") return route.fallback();
    if (left > 0) {
      left -= 1;
      return route.fulfill({ status: 500, contentType: "application/json", body: JSON.stringify({ status: 500 }) });
    }
    if (body === undefined) return route.fallback();
    return route.fulfill({ status: 200, contentType: "application/json", body: JSON.stringify(body) });
  };
  await page.route(pattern, handler);
  return () => page.unroute(pattern, handler);
}

async function phonePage(browser: Browser, page: Page) {
  const ctx = await browser.newContext({
    storageState: await page.context().storageState(),
    viewport: { width: 390, height: 844 },
    deviceScaleFactor: 2,
    isMobile: true,
    hasTouch: true,
  });
  return { ctx, page: await ctx.newPage() };
}

const SKILL = {
  id: "00000000-0000-4000-8000-000000000189",
  kind: "skill",
  name: "履歴を見るスキル",
  owner: "someone-else",
  current_version: 2,
  created_at: "2026-10-01T00:00:00Z",
  updated_at: "2026-10-20T00:00:00Z",
};

test("スキルの画面: バージョン履歴とスキルストアの読み込みの失敗を、空に見せない（BUG-189）", async ({ page, browser }) => {
  test.setTimeout(120_000);
  await page.setViewportSize({ width: 1440, height: 900 });
  await loginViaKeycloak(page);
  const list = /\/api\/artifacts\?kind=skill(&.*)?$/;
  const stopList = await failThen(page, list, 0, { items: [SKILL], owned_next: null, shared_next_cursor: null });
  const stopVersions = await failThen(page, new RegExp(`/api/artifacts/${SKILL.id}/versions$`), 1, {
    items: [{ version: 2, created_by: "someone-else", created_at: "2026-10-20T00:00:00Z" }],
  });
  const stopStore = await failThen(page, /\/api\/skills\/registry$/, 100);
  try {
    await page.goto("/skills");
    // スキルストアは欄ごと消さずに、読み込めなかったことを言う。
    const store = page.locator("#skill-store");
    await expect(store.getByTestId("list-load-failed")).toContainText("スキルストアを読み込めませんでした。", {
      timeout: 20_000,
    });
    await page.getByRole("button", { name: `${SKILL.name} のその他の操作` }).click();
    await page.getByRole("menuitem", { name: "バージョン履歴" }).click();
    const dialog = page.getByRole("dialog");
    const failed = dialog.getByTestId("list-load-failed");
    await expect(failed).toContainText("バージョン履歴を読み込めませんでした。");
    await expect(dialog.getByText("履歴がありません。")).toHaveCount(0);
    if (process.env.E2E_SHOTS_DIR) await page.screenshot({ path: `${process.env.E2E_SHOTS_DIR}/versions-load-failed-pc.png` });
    await failed.getByRole("button", { name: "もう一度読み込む" }).click();
    await expect(dialog.getByTestId("version-label")).toHaveText(["v2"]);
    await dialog.getByRole("button", { name: "閉じる" }).click();
    await store.scrollIntoViewIfNeeded();
    if (process.env.E2E_SHOTS_DIR) await page.screenshot({ path: `${process.env.E2E_SHOTS_DIR}/skill-store-load-failed-pc.png` });
  } finally {
    await stopList();
    await stopVersions();
    await stopStore();
  }

  if (process.env.E2E_SHOTS_DIR) {
    const phone = await phonePage(browser, page);
    try {
      const a = await failThen(phone.page, list, 0, { items: [SKILL], owned_next: null, shared_next_cursor: null });
      const b = await failThen(phone.page, new RegExp(`/api/artifacts/${SKILL.id}/versions$`), 100);
      const c = await failThen(phone.page, /\/api\/skills\/registry$/, 100);
      await phone.page.goto("/skills");
      await phone.page.getByRole("button", { name: `${SKILL.name} のその他の操作` }).click();
      await phone.page.getByRole("menuitem", { name: "バージョン履歴" }).click();
      await expect(phone.page.getByRole("dialog").getByTestId("list-load-failed")).toBeVisible({ timeout: 20_000 });
      expect(await phone.page.evaluate(() => document.documentElement.scrollWidth <= window.innerWidth + 1)).toBe(true);
      await phone.page.screenshot({ path: `${process.env.E2E_SHOTS_DIR}/versions-load-failed-phone.png` });
      await phone.page.getByRole("dialog").getByRole("button", { name: "閉じる" }).click();
      await phone.page.locator("#skill-store").scrollIntoViewIfNeeded();
      await phone.page.screenshot({ path: `${process.env.E2E_SHOTS_DIR}/skill-store-load-failed-phone.png` });
      await a();
      await b();
      await c();
    } finally {
      await phone.ctx.close();
    }
  }
});

test("アプリの画面: インストール済みのアプリとインストールできるアプリの読み込みの失敗を、空に見せない（BUG-189）", async ({
  page,
  browser,
}) => {
  test.setTimeout(90_000);
  await page.setViewportSize({ width: 1440, height: 900 });
  await loginViaKeycloak(page);
  const stopInstalled = await failThen(page, /\/api\/apps\/installations$/, 1, { items: [] });
  const stopRegistry = await failThen(page, /\/api\/apps\/registry$/, 1, { items: [] });
  try {
    await page.goto("/apps");
    const failed = page.getByTestId("list-load-failed");
    await expect(failed.filter({ hasText: "インストール済みのアプリを読み込めませんでした。" })).toBeVisible({
      timeout: 20_000,
    });
    await expect(failed.filter({ hasText: "インストールできるアプリの一覧を読み込めませんでした。" })).toBeVisible();
    await expect(page.getByText("インストール済みのコードアプリはありません。")).toHaveCount(0);
    if (process.env.E2E_SHOTS_DIR) {
      await failed.first().scrollIntoViewIfNeeded();
      await page.screenshot({ path: `${process.env.E2E_SHOTS_DIR}/installed-apps-load-failed-pc.png` });
    }
    await failed.first().getByRole("button", { name: "もう一度読み込む" }).click();
    await expect(page.getByText("インストール済みのコードアプリはありません。")).toBeVisible();
    await expect(page.getByTestId("list-load-failed")).toHaveCount(0);
  } finally {
    await stopInstalled();
    await stopRegistry();
  }

  if (process.env.E2E_SHOTS_DIR) {
    const phone = await phonePage(browser, page);
    try {
      const a = await failThen(phone.page, /\/api\/apps\/installations$/, 100);
      const b = await failThen(phone.page, /\/api\/apps\/registry$/, 100);
      await phone.page.goto("/apps");
      const failed = phone.page.getByTestId("list-load-failed");
      await expect(failed.first()).toBeVisible({ timeout: 20_000 });
      await failed.first().scrollIntoViewIfNeeded();
      expect(await phone.page.evaluate(() => document.documentElement.scrollWidth <= window.innerWidth + 1)).toBe(true);
      await phone.page.screenshot({ path: `${process.env.E2E_SHOTS_DIR}/installed-apps-load-failed-phone.png` });
      await a();
      await b();
    } finally {
      await phone.ctx.close();
    }
  }
});

test("実行履歴: 読み込みの失敗を「まだ実行していません」に見せず、もう一度読み込める（BUG-189）", async ({ page, browser }) => {
  test.setTimeout(120_000);
  await page.setViewportSize({ width: 1440, height: 900 });
  await loginViaKeycloak(page);
  const name = uniqueName("runs-load-failed");
  const res = await page.request.post("/api/workflows", {
    headers: await csrf(page),
    data: {
      ir: {
        ir_version: 1,
        name,
        display_name: name,
        description: null,
        declared_scopes: [],
        triggers: [{ kind: "interactive", label: null }],
        input_schema: null,
        nodes: [],
        edges: [],
        policies: { run_timeout_sec: 259200, max_parallel_runs: 10, on_trigger_overflow: "queue" },
      },
    },
  });
  expect(res.ok(), await res.text()).toBeTruthy();
  const id = ((await res.json()) as { id: string }).id;
  cleanup.add(() => removeWorkflowApi(page, id));
  const runs = new RegExp(`/api/workflows/${id}/runs\\?`);
  // 表示中の 5 秒ごとの読み直しも失敗させ続ける（失敗の表示が勝手に消えない）。確かめてから本物へ戻す。
  const stop = await failThen(page, runs, 1_000);
  try {
    await page.goto(`/workflows/${id}/runs`);
    const failed = page.getByTestId("runs-load-failed");
    await expect(failed).toContainText("実行履歴を読み込めませんでした", { timeout: 20_000 });
    await expect(failed).toContainText("少し待ってから、もう一度読み込んでください。");
    await expect(page.getByText("まだ実行していません")).toHaveCount(0);
    if (process.env.E2E_SHOTS_DIR) await page.screenshot({ path: `${process.env.E2E_SHOTS_DIR}/runs-load-failed-pc.png` });
    await stop();
    await failed.getByRole("button", { name: "もう一度読み込む" }).click();
    await expect(page.getByText("まだ実行していません")).toBeVisible();
    await expect(page.getByTestId("runs-load-failed")).toHaveCount(0);
  } finally {
    await stop();
  }

  if (process.env.E2E_SHOTS_DIR) {
    const phone = await phonePage(browser, page);
    try {
      const s = await failThen(phone.page, runs, 100);
      await phone.page.goto(`/workflows/${id}/runs`);
      await expect(phone.page.getByTestId("runs-load-failed")).toBeVisible({ timeout: 20_000 });
      expect(await phone.page.evaluate(() => document.documentElement.scrollWidth <= window.innerWidth + 1)).toBe(true);
      await phone.page.screenshot({ path: `${process.env.E2E_SHOTS_DIR}/runs-load-failed-phone.png` });
      await s();
    } finally {
      await phone.ctx.close();
    }
  }
});
