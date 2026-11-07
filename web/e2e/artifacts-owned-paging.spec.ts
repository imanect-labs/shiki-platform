import { expect, test, type Browser, type Page, type Route } from "@playwright/test";

import { cleanupQueue, deleteArtifactApi, loginViaKeycloak, uniqueName } from "./helpers";

/// アーティファクトの一覧（GET /artifacts）の自分のものの続き（BUG-186）:
/// - 自分のものに続きがあると `owned_next` が付き、`before_*` に渡すと自分のものの続きだけが返る。読み切ると付かない。
/// - スキルの一覧の「さらに読み込む」は、自分のものの続きも読む（応答は route で差し替える）。
/// - ホームのスキルの選択は、6 件より後ろのスキルを「スキルを探す」から名前で絞り込んで選べ、50 件より古い自分の
///   スキルは「さらに読み込む」で足せる。読み込みの失敗は、選択肢が無いことに見せない（BUG-189）。
/// - ミニアプリのエディターのスキルとワークフローの選択肢も、続きを「さらに読み込む」で足せ、読み込めなかったことを
///   言う（BUG-189）。
/// 作ったもの（アーティファクト）は最後に片付ける（失敗したテストでも走る）。

const cleanup = cleanupQueue();
test.afterEach(() => cleanup.run());

async function csrf(page: Page): Promise<Record<string, string>> {
  const token = (await page.context().cookies()).find((c) => c.name === "shiki_csrf")?.value ?? "";
  return { "x-csrf-token": token, "content-type": "application/json" };
}

type OwnedNext = { before_updated_at: string; before_id: string };
type ListBody = { items: { id: string; owner: string }[]; owned_next?: OwnedNext | null; shared_next_cursor?: string | null };

test("自分のものに続きがあると owned_next が付き、before_* で自分のものの続きだけを読める（BUG-186）", async ({ page }) => {
  test.setTimeout(90_000);
  await loginViaKeycloak(page);
  const me = (await (await page.request.get("/api/me")).json()) as { id: string };
  const mine: string[] = [];
  for (let i = 0; i < 3; i += 1) {
    const res = await page.request.post("/api/artifacts", {
      headers: await csrf(page),
      data: { kind: "script", name: uniqueName(`e2e-自分の続き-${i}`), body: { source: "export default 1;" } },
    });
    expect(res.status(), await res.text()).toBe(201);
    const id = ((await res.json()) as { id: string }).id;
    cleanup.add(() => deleteArtifactApi(page, id));
    mine.push(id);
  }
  const first = (await (await page.request.get("/api/artifacts?kind=script&limit=1")).json()) as ListBody;
  expect(first.owned_next, "自分のものに続きがある").toBeTruthy();
  const seen = first.items.filter((a) => a.owner === me.id).map((a) => a.id);
  let next = first.owned_next ?? null;
  for (let round = 0; next && round < 200; round += 1) {
    const res = await page.request.get(
      `/api/artifacts?kind=script&limit=1&before_updated_at=${encodeURIComponent(next.before_updated_at)}&before_id=${next.before_id}`,
    );
    expect(res.ok(), await res.text()).toBeTruthy();
    const p = (await res.json()) as ListBody;
    // 続きは自分のものだけ（共有された分は先頭のページで合わせ済み）。
    expect(p.items.every((a) => a.owner === me.id)).toBe(true);
    expect(p.shared_next_cursor ?? null).toBeNull();
    seen.push(...p.items.map((a) => a.id));
    next = p.owned_next ?? null;
  }
  expect(next, "読み切ると owned_next は付かない").toBeNull();
  expect(seen.filter((id) => mine.includes(id))).toEqual([...mine].reverse());
  expect(new Set(seen).size).toBe(seen.length);
});

const SKILLS = /\/api\/artifacts\?kind=skill(&.*)?$/;
const WORKFLOWS = /\/api\/artifacts\?kind=workflow(&.*)?$/;

function fake(kind: string, n: number, name: string, day: number, owner = "someone-else") {
  return {
    id: `00000000-0000-4000-8000-${String(n).padStart(12, "0")}`,
    kind,
    name,
    owner,
    current_version: 1,
    created_at: "2026-10-01T00:00:00Z",
    updated_at: `2026-10-${String(day).padStart(2, "0")}T00:00:00Z`,
  };
}
const ownedNextOf = (a: { id: string; updated_at: string }) => ({ before_updated_at: a.updated_at, before_id: a.id });

/// 一覧の頼みに、続きの位置ごとの応答を返す（`fail` に入れた鍵は 1 回だけ 500）。鍵は自分のものの続きなら
/// `before:<id>`、共有された分の続きなら `shared:<位置>`、先頭のページなら空の文字。
async function serve(page: Page, pattern: RegExp, pages: Record<string, unknown>, fail: Set<string> = new Set()) {
  const asked: string[] = [];
  const handler = async (route: Route) => {
    if (route.request().method() !== "GET") return route.fallback();
    const q = new URL(route.request().url()).searchParams;
    const key = q.get("before_id") ? `before:${q.get("before_id")}` : q.get("shared_cursor") ? `shared:${q.get("shared_cursor")}` : "";
    asked.push(key);
    if (fail.delete(key) || !(key in pages)) {
      return route.fulfill({ status: 500, contentType: "application/json", body: JSON.stringify({ status: 500 }) });
    }
    return route.fulfill({ status: 200, contentType: "application/json", body: JSON.stringify(pages[key]) });
  };
  await page.route(pattern, handler);
  return { asked, stop: () => page.unroute(pattern, handler) };
}

async function phonePage(browser: Browser, page: Page, width = 390) {
  const ctx = await browser.newContext({
    storageState: await page.context().storageState(),
    viewport: { width, height: 844 },
    deviceScaleFactor: 2,
    isMobile: true,
    hasTouch: true,
  });
  return { ctx, page: await ctx.newPage() };
}

test("スキルの一覧の「さらに読み込む」は、自分のものの続きも読む（BUG-186）", async ({ page }) => {
  test.setTimeout(90_000);
  await page.setViewportSize({ width: 1440, height: 900 });
  await loginViaKeycloak(page);
  const newer = fake("skill", 1, "新しい自分のスキル", 20);
  const older = fake("skill", 2, "古い自分のスキル", 10);
  const served = await serve(page, SKILLS, {
    "": { items: [newer], owned_next: ownedNextOf(newer), shared_next_cursor: null },
    [`before:${newer.id}`]: { items: [older], owned_next: null, shared_next_cursor: null },
  });
  try {
    await page.goto("/skills");
    await expect(page.getByText(newer.name)).toBeVisible({ timeout: 20_000 });
    await expect(page.getByText(older.name)).toHaveCount(0);
    await page.getByTestId("list-more-button").click();
    await expect(page.getByText(older.name)).toBeVisible();
    await expect(page.getByTestId("list-more")).toHaveCount(0);
    expect(served.asked.slice(served.asked.lastIndexOf(""))).toEqual(["", `before:${newer.id}`]);
  } finally {
    await served.stop();
  }
});

test("ホームのスキルの選択: 6 件より後ろと 50 件より古いスキルも「スキルを探す」から選べる（BUG-186・BUG-189）", async ({
  page,
  browser,
}) => {
  test.setTimeout(120_000);
  await page.setViewportSize({ width: 1440, height: 900 });
  await loginViaKeycloak(page);
  const firstPage = Array.from({ length: 7 }, (_, i) => fake("skill", i + 1, `最近のスキル ${i + 1}`, 28 - i));
  const old = fake("skill", 20, "古い経費のスキル", 2);
  const pages: Record<string, unknown> = {
    "": { items: firstPage, owned_next: ownedNextOf(firstPage[6]), shared_next_cursor: null },
    [`before:${firstPage[6].id}`]: { items: [old], owned_next: null, shared_next_cursor: null },
  };
  const served = await serve(page, SKILLS, pages);
  try {
    await page.goto("/");
    const picker = page.getByLabel("スキルを選択");
    await expect(picker.getByRole("button", { name: "最近のスキル 1" })).toBeVisible({ timeout: 20_000 });
    // 6 件までをすぐ選べるように並べ、7 件目は「スキルを探す」から選ぶ。
    await expect(picker.getByRole("button", { name: "最近のスキル 7" })).toHaveCount(0);
    await picker.getByTestId("skill-picker-search").click();
    const dialog = page.getByTestId("skill-picker-dialog");
    await expect(dialog.getByTestId("skill-picker-option")).toHaveCount(7);
    await dialog.getByLabel("スキルの名前").fill("経費");
    await expect(dialog.getByText("「経費」に合うスキルは、読み込んだ中にありません。")).toBeVisible();
    await expect(dialog.getByTestId("list-more")).toHaveText(/ほかにもスキルがあります。「さらに読み込む」で選択肢に足します。/);
    if (process.env.E2E_SHOTS_DIR) await page.screenshot({ path: `${process.env.E2E_SHOTS_DIR}/home-skill-search-pc.png` });
    await dialog.getByTestId("list-more-button").click();
    await expect(dialog.getByTestId("skill-picker-option")).toHaveText([old.name]);
    await expect(dialog.getByTestId("list-more")).toHaveCount(0);
    await dialog.getByTestId("skill-picker-option").click();
    await expect(dialog).toHaveCount(0);
    // 選んだスキルは先頭に置いて、選んでいることを見せる。
    await expect(picker.getByRole("button", { name: old.name })).toHaveAttribute("aria-pressed", "true");
  } finally {
    await served.stop();
  }

  // 読み込みの失敗: 選択肢が無いことに見せず、もう一度読み込める。
  const failing = await serve(page, SKILLS, pages, new Set([""]));
  try {
    await page.goto("/");
    const failed = page.getByLabel("スキルを選択").getByTestId("list-load-failed");
    await expect(failed).toContainText("スキルの一覧を読み込めませんでした。", { timeout: 20_000 });
    await failed.getByRole("button", { name: "もう一度読み込む" }).click();
    await expect(page.getByLabel("スキルを選択").getByRole("button", { name: "最近のスキル 1" })).toBeVisible();
  } finally {
    await failing.stop();
  }

  if (process.env.E2E_SHOTS_DIR) {
    // スマホ幅のホームはスキルの段を出さない（スキルは「+」から選ぶ・#975）。探す画面は、段が出るいちばん狭い幅
    // （sm の 640px）で崩れないことを見る。
    const phone = await phonePage(browser, page);
    try {
      const phoneServed = await serve(phone.page, SKILLS, pages);
      await phone.page.goto("/");
      const picker = phone.page.getByLabel("スキルを選択");
      await expect(picker).toHaveCount(1);
      await expect(picker).toBeHidden();
      await phoneServed.stop();
    } finally {
      await phone.ctx.close();
    }
    const narrow = await phonePage(browser, page, 640);
    try {
      const narrowServed = await serve(narrow.page, SKILLS, pages);
      await narrow.page.goto("/");
      await narrow.page.getByLabel("スキルを選択").getByTestId("skill-picker-search").click();
      const dialog = narrow.page.getByTestId("skill-picker-dialog");
      await expect(dialog.getByTestId("skill-picker-option")).toHaveCount(7);
      expect(await narrow.page.evaluate(() => document.documentElement.scrollWidth <= window.innerWidth + 1)).toBe(true);
      await narrow.page.screenshot({ path: `${process.env.E2E_SHOTS_DIR}/home-skill-search-narrow.png` });
      await dialog.getByLabel("スキルの名前").fill("経費");
      await expect(dialog.getByTestId("list-more")).toBeVisible();
      await narrow.page.screenshot({ path: `${process.env.E2E_SHOTS_DIR}/home-skill-search-more-narrow.png` });
      await narrowServed.stop();
    } finally {
      await narrow.ctx.close();
    }
  }
});

test("ミニアプリのエディター: スキルとワークフローの選択肢の続きを足せ、読み込めなかったことを言う（BUG-186・BUG-189）", async ({
  page,
  browser,
}) => {
  test.setTimeout(120_000);
  await page.setViewportSize({ width: 1440, height: 900 });
  await loginViaKeycloak(page);
  const newSkill = fake("skill", 31, "新しいスキル", 20);
  const oldSkill = fake("skill", 32, "古いスキル", 3);
  const flow = fake("workflow", 33, "expense-flow", 12);
  const skillPages = {
    "": { items: [newSkill], owned_next: ownedNextOf(newSkill), shared_next_cursor: null },
    [`before:${newSkill.id}`]: { items: [oldSkill], owned_next: null, shared_next_cursor: null },
  };
  const flowPages = { "": { items: [flow], owned_next: null, shared_next_cursor: null } };
  const skills = await serve(page, SKILLS, skillPages);
  const flows = await serve(page, WORKFLOWS, flowPages, new Set([""]));
  try {
    await page.goto("/apps");
    await page.getByRole("button", { name: "アプリを作成" }).first().click();
    const dialog = page.getByRole("dialog");
    // ワークフローの一覧を読めなかった: 選択肢が無いことに見せず、もう一度読み込める。
    const failed = dialog.getByTestId("list-load-failed");
    await expect(failed).toContainText("ワークフローの一覧を読み込めませんでした。", { timeout: 20_000 });
    // スキルの選択肢には、まだ読んでいない自分のスキルがあることを言う。
    const skillSelect = dialog.locator("#app-skill");
    await expect(skillSelect.locator("option")).toHaveText(["なし", "新しいスキル（v1）"]);
    const more = dialog.getByTestId("list-more");
    await expect(more).toHaveText(/ほかにもスキルがあります。「さらに読み込む」で選択肢に足します。/);
    if (process.env.E2E_SHOTS_DIR) await page.screenshot({ path: `${process.env.E2E_SHOTS_DIR}/miniapp-editor-pickers-pc.png` });
    await more.getByTestId("list-more-button").click();
    await expect(skillSelect.locator("option")).toHaveText(["なし", "新しいスキル（v1）", "古いスキル（v1）"]);
    await expect(dialog.getByTestId("list-more")).toHaveCount(0);
    await skillSelect.selectOption({ label: "古いスキル（v1）" });
    await failed.getByRole("button", { name: "もう一度読み込む" }).click();
    await expect(dialog.getByLabel("追加するワークフロー").locator("option")).toHaveText([
      "ワークフローを選ぶ",
      "expense-flow（v1）",
    ]);
    await expect(dialog.getByTestId("list-load-failed")).toHaveCount(0);
    await dialog.getByRole("button", { name: "キャンセル" }).click();
  } finally {
    await skills.stop();
    await flows.stop();
  }

  if (process.env.E2E_SHOTS_DIR) {
    const phone = await phonePage(browser, page);
    try {
      const s = await serve(phone.page, SKILLS, skillPages);
      const f = await serve(phone.page, WORKFLOWS, flowPages, new Set([""]));
      await phone.page.goto("/apps");
      await phone.page.getByRole("button", { name: "アプリを作成" }).first().click();
      const dialog = phone.page.getByRole("dialog");
      await expect(dialog.getByTestId("list-load-failed")).toBeVisible({ timeout: 20_000 });
      await dialog.getByTestId("list-more").scrollIntoViewIfNeeded();
      expect(await phone.page.evaluate(() => document.documentElement.scrollWidth <= window.innerWidth + 1)).toBe(true);
      await phone.page.screenshot({ path: `${process.env.E2E_SHOTS_DIR}/miniapp-editor-pickers-phone.png` });
      await dialog.getByLabel("追加するワークフロー").scrollIntoViewIfNeeded();
      await phone.page.screenshot({ path: `${process.env.E2E_SHOTS_DIR}/miniapp-editor-workflows-phone.png` });
      await s.stop();
      await f.stop();
    } finally {
      await phone.ctx.close();
    }
  }
});
