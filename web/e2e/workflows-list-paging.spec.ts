import { expect, test, type Page, type Route } from "@playwright/test";

import { cleanupQueue, loginAs, loginViaKeycloak, removeWorkflowApi, uniqueName } from "./helpers";

/// ワークフローの一覧（GET /workflows）の続き（BUG-185）:
/// - 自分のものの続きの位置（`owned_next`）と共有された分の続きの位置（`shared_next_cursor`）を別に返す。自分のものの
///   続きは自分のものだけを、共有された分の続きは共有された分だけを、重ならずに返す。両方を同時に渡すと 400。
/// - 画面の「さらに読み込む」は、それぞれの位置から続きを読む。合わせたページの最後の 1 件（共有されたもの）を位置に
///   使わないので、それより新しい自分のワークフローを読み飛ばさない（応答は route で差し替える）。
/// - 読み込みの失敗を「まだワークフローがありません」に見せず、続きの失敗も言う（自分のもの・共有された分・両方を
///   言い分ける・BUG-189）。
/// 作ったもの（ワークフロー・ブラウザーの文脈）は最後に片付ける（失敗したテストでも走る）。

const cleanup = cleanupQueue();
test.afterEach(() => cleanup.run());

async function csrf(page: Page): Promise<Record<string, string>> {
  const token = (await page.context().cookies()).find((c) => c.name === "shiki_csrf")?.value ?? "";
  return { "x-csrf-token": token, "content-type": "application/json" };
}

function emptyIr(name: string) {
  return {
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
  };
}

async function createFlow(page: Page, name: string): Promise<string> {
  const res = await page.request.post("/api/workflows", { headers: await csrf(page), data: { ir: emptyIr(name) } });
  expect(res.ok(), await res.text()).toBeTruthy();
  const id = ((await res.json()) as { id: string }).id;
  cleanup.add(() => removeWorkflowApi(page, id));
  return id;
}

type OwnedNext = { before_updated_at: string; before_id: string };
type ListBody = { items: { id: string }[]; owned_next?: OwnedNext | null; shared_next_cursor?: string | null };

async function list(page: Page, query: string): Promise<ListBody> {
  const res = await page.request.get(`/api/workflows?limit=1${query}`);
  expect(res.ok(), await res.text()).toBeTruthy();
  return (await res.json()) as ListBody;
}

test("自分のものと共有されたものの続きの位置を別に返し、それぞれの続きを重ならずに読める（BUG-185）", async ({
  page,
  browser,
}) => {
  test.setTimeout(150_000);
  await loginViaKeycloak(page);
  const alice = (await (await page.request.get("/api/me")).json()) as { id: string };
  const bobCtx = await browser.newContext();
  cleanup.add(() => bobCtx.close());
  const bob = await bobCtx.newPage();
  await loginAs(bob, "bob");
  // bob が 2 つ作って alice に閲覧で共有し、alice が 2 つ作る（後から作ったものほど新しい）。
  const shared: string[] = [];
  for (let i = 0; i < 2; i += 1) {
    const id = await createFlow(bob, uniqueName(`e2e-list-shared-${i}`));
    const s = await bob.request.put(`/api/artifacts/${id}/shares`, {
      headers: await csrf(bob),
      data: { target: { type: "user", id: alice.id }, role: "viewer" },
    });
    expect(s.status(), await s.text()).toBe(204);
    shared.push(id);
  }
  const mine: string[] = [];
  for (let i = 0; i < 2; i += 1) mine.push(await createFlow(page, uniqueName(`e2e-list-mine-${i}`)));

  const first = await list(page, "");
  expect(first.owned_next, "自分のものに続きがある").toBeTruthy();
  expect(first.shared_next_cursor, "共有された分に続きがある").toBeTruthy();
  for (const id of [...shared, ...mine]) expect(first.shared_next_cursor).not.toContain(id);
  const seen = first.items.map((w) => w.id);

  // 自分のものの続き: 自分のものだけ。
  const ownedSeen = [...seen];
  let owned = first.owned_next ?? null;
  for (let round = 0; owned && round < 200; round += 1) {
    const p = await list(
      page,
      `&before_updated_at=${encodeURIComponent(owned.before_updated_at)}&before_id=${owned.before_id}`,
    );
    expect(p.shared_next_cursor ?? null, "自分のものの続きは共有された分を返さない").toBeNull();
    expect(p.items.some((w) => shared.includes(w.id))).toBe(false);
    ownedSeen.push(...p.items.map((w) => w.id));
    owned = p.owned_next ?? null;
  }
  expect(owned, "自分のものを読み切る").toBeNull();
  expect(ownedSeen.filter((id) => mine.includes(id))).toEqual([...mine].reverse());

  // 共有された分の続き: 共有された分だけ。
  const sharedSeen = [...seen];
  let cursor = first.shared_next_cursor ?? null;
  for (let round = 0; cursor && round < 200; round += 1) {
    const p = await list(page, `&shared_cursor=${encodeURIComponent(cursor)}`);
    expect(p.owned_next ?? null, "共有された分の続きは自分のものを返さない").toBeNull();
    expect(p.items.some((w) => mine.includes(w.id))).toBe(false);
    sharedSeen.push(...p.items.map((w) => w.id));
    cursor = p.shared_next_cursor ?? null;
  }
  expect(sharedSeen.filter((id) => shared.includes(id))).toEqual([...shared].reverse());
  const all = [...ownedSeen, ...sharedSeen.slice(seen.length)];
  expect(new Set(all).size, "重ならない").toBe(all.length);

  // 両方の位置は同時に使えない。読めない位置は 400（cursor_invalid）。
  const both = await page.request.get(
    `/api/workflows?shared_cursor=${encodeURIComponent(first.shared_next_cursor ?? "")}` +
      `&before_updated_at=${encodeURIComponent(first.owned_next?.before_updated_at ?? "")}&before_id=${first.owned_next?.before_id}`,
  );
  expect(both.status()).toBe(400);
  const bad = await page.request.get(`/api/workflows?shared_cursor=${encodeURIComponent(`${first.shared_next_cursor}x`)}`);
  expect(bad.status()).toBe(400);
  expect(((await bad.json()) as { code?: string }).code).toBe("cursor_invalid");
});

const LIST = /\/api\/workflows(\?.*)?$/;

function flow(n: number, name: string, day: number) {
  return {
    id: `00000000-0000-4000-8000-${String(n).padStart(12, "0")}`,
    name: `flow-${n}`,
    display_name: name,
    description: null,
    current_version: 1,
    trigger_kinds: ["interactive"],
    enabled_status: "none",
    enabled_version: null,
    updated_at: `2026-10-${String(day).padStart(2, "0")}T00:00:00Z`,
  };
}

const OWN_NEW = flow(1, "自分の新しいワークフロー", 20);
const OWN_MID = flow(2, "自分の次のワークフロー", 15);
const SHARED_OLD = flow(3, "共有された古いワークフロー", 10);
const SHARED_OLDER = flow(4, "共有されたもっと古いワークフロー", 5);
const ownedNextOf = (w: { id: string; updated_at: string }) => ({ before_updated_at: w.updated_at, before_id: w.id });

/// 一覧の頼みに、続きの位置ごとの応答を返す（`fail` に入れた鍵は 1 回だけ 500）。鍵は自分のものの続きなら
/// `before:<id>`、共有された分の続きなら `shared:<位置>`、先頭のページなら空の文字。
async function serveFlows(page: Page, pages: Record<string, unknown>, fail: Set<string> = new Set()) {
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
  await page.route(LIST, handler);
  return { asked, stop: () => page.unroute(LIST, handler) };
}

const rows = (page: Page) => page.getByTestId("workflow-row");

test("「さらに読み込む」は自分のものの続きの位置から読み、共有されたものの後ろの自分のワークフローを読み飛ばさない（BUG-185）", async ({
  page,
  browser,
}) => {
  test.setTimeout(90_000);
  await page.setViewportSize({ width: 1440, height: 900 });
  await loginViaKeycloak(page);
  const pages: Record<string, unknown> = {
    // 合わせたページの最後は共有されたもの。自分のものの続きの位置は OWN_NEW。
    "": { items: [OWN_NEW, SHARED_OLD], owned_next: ownedNextOf(OWN_NEW), shared_next_cursor: "s1" },
    "shared:s1": { items: [SHARED_OLDER], owned_next: null, shared_next_cursor: null },
    [`before:${OWN_NEW.id}`]: { items: [OWN_MID], owned_next: null, shared_next_cursor: null },
    // 合わせたページの最後（共有されたもの）を位置に使うと、OWN_MID は返らない。
    [`before:${SHARED_OLD.id}`]: { items: [], owned_next: null, shared_next_cursor: null },
  };
  const served = await serveFlows(page, pages);
  try {
    await page.goto("/workflows");
    await expect(page.getByText(SHARED_OLDER.display_name)).toBeVisible({ timeout: 20_000 });
    const more = page.getByTestId("list-more");
    // 自分のものの続きだけが残っているときは、ボタンだけを出す。
    await expect(more.getByRole("button", { name: "さらに読み込む" })).toBeVisible();
    await expect(more.locator("p")).toHaveCount(0);
    await more.getByTestId("list-more-button").click();
    await expect(page.getByText(OWN_MID.display_name)).toBeVisible();
    await expect(page.getByTestId("list-more")).toHaveCount(0);
    // 更新の新しい順に並ぶ。
    await expect(rows(page)).toHaveText([
      new RegExp(OWN_NEW.display_name),
      new RegExp(OWN_MID.display_name),
      new RegExp(SHARED_OLD.display_name),
      new RegExp(SHARED_OLDER.display_name),
    ]);
    expect(served.asked).toContain(`before:${OWN_NEW.id}`);
    expect(served.asked).not.toContain(`before:${SHARED_OLD.id}`);
    if (process.env.E2E_SHOTS_DIR) await page.screenshot({ path: `${process.env.E2E_SHOTS_DIR}/workflows-more-pc.png` });
  } finally {
    await served.stop();
  }
  if (process.env.E2E_SHOTS_DIR) {
    const phone = await browser.newContext({
      storageState: await page.context().storageState(),
      viewport: { width: 390, height: 844 },
      deviceScaleFactor: 2,
      isMobile: true,
      hasTouch: true,
    });
    try {
      const p = await phone.newPage();
      const phoneServed = await serveFlows(p, pages);
      await p.goto("/workflows");
      await expect(p.getByTestId("list-more")).toBeVisible({ timeout: 20_000 });
      expect(await p.evaluate(() => document.documentElement.scrollWidth <= window.innerWidth + 1)).toBe(true);
      await p.screenshot({ path: `${process.env.E2E_SHOTS_DIR}/workflows-more-phone.png`, fullPage: true });
      await phoneServed.stop();
    } finally {
      await phone.close();
    }
  }
});

test("一覧の読み込みの失敗を空に見せず、続きの失敗と共有された分を読み切れなかったことを言う（BUG-185・BUG-189）", async ({
  page,
  browser,
}) => {
  test.setTimeout(120_000);
  await page.setViewportSize({ width: 1440, height: 900 });
  await loginViaKeycloak(page);

  // 1. 最初の読み込みの失敗: 空の状態に見せず、もう一度読み込める。
  const pages: Record<string, unknown> = {
    "": { items: [OWN_NEW], owned_next: ownedNextOf(OWN_NEW), shared_next_cursor: null },
    [`before:${OWN_NEW.id}`]: { items: [OWN_MID], owned_next: null, shared_next_cursor: null },
  };
  const served = await serveFlows(page, pages, new Set(["", `before:${OWN_NEW.id}`]));
  try {
    await page.goto("/workflows");
    const failed = page.getByTestId("list-load-failed");
    await expect(failed).toContainText("ワークフローの一覧を読み込めませんでした。", { timeout: 20_000 });
    await expect(page.getByText("まだワークフローがありません")).toHaveCount(0);
    if (process.env.E2E_SHOTS_DIR) await page.screenshot({ path: `${process.env.E2E_SHOTS_DIR}/workflows-load-failed-pc.png` });
    await failed.getByRole("button", { name: "もう一度読み込む" }).click();
    await expect(page.getByText(OWN_NEW.display_name)).toBeVisible();

    // 2. 自分のものの続きの失敗: 読めた分を残して言い、押すと同じ位置から読む。
    await page.getByTestId("list-more-button").click();
    await expect(page.getByTestId("list-more")).toHaveText(
      /ワークフローの続きを読み込めませんでした。「さらに読み込む」でもう一度試します。/,
    );
    await expect(page.getByText(OWN_NEW.display_name)).toBeVisible();
    if (process.env.E2E_SHOTS_DIR) await page.screenshot({ path: `${process.env.E2E_SHOTS_DIR}/workflows-more-failed-pc.png` });
    await page.getByTestId("list-more-button").click();
    await expect(page.getByText(OWN_MID.display_name)).toBeVisible();
    await expect(page.getByTestId("list-more")).toHaveCount(0);
    expect(served.asked.filter((k) => k === `before:${OWN_NEW.id}`)).toHaveLength(2);
  } finally {
    await served.stop();
  }

  // 3. 共有された分を往復の上限で読み切れなかった: まだ見つかっていないことを言い、押すと続きを探す。
  const sharedPages: Record<string, unknown> = {
    "": { items: [OWN_NEW], owned_next: null, shared_next_cursor: "s1" },
  };
  for (let i = 1; i <= 5; i += 1) sharedPages[`shared:s${i}`] = { items: [], owned_next: null, shared_next_cursor: `s${i + 1}` };
  sharedPages["shared:s6"] = { items: [SHARED_OLD], owned_next: null, shared_next_cursor: null };
  const sharedServed = await serveFlows(page, sharedPages);
  try {
    await page.goto("/workflows");
    const more = page.getByTestId("list-more");
    await expect(more).toHaveText(
      /共有されたワークフローの続きは、まだ見つかっていません。「さらに読み込む」で続きを探します。/,
      { timeout: 20_000 },
    );
    await more.getByTestId("list-more-button").click();
    await expect(page.getByText(SHARED_OLD.display_name)).toBeVisible();
    await expect(page.getByTestId("list-more")).toHaveCount(0);
  } finally {
    await sharedServed.stop();
  }

  // 4. 自分のものと共有された分の続きが両方とも失敗した: 両方を読めていないことを言い、押すと両方の位置から読む。
  const bothPages: Record<string, unknown> = {
    "": { items: [OWN_NEW], owned_next: ownedNextOf(OWN_NEW), shared_next_cursor: "s1" },
    [`before:${OWN_NEW.id}`]: { items: [OWN_MID], owned_next: null, shared_next_cursor: null },
  };
  // 最初の読み込みは共有された分の続きを 5 回まで（s1〜s5・0 件）たどり、s6 を残す。
  for (let i = 1; i <= 5; i += 1) bothPages[`shared:s${i}`] = { items: [], owned_next: null, shared_next_cursor: `s${i + 1}` };
  bothPages["shared:s6"] = { items: [SHARED_OLD], owned_next: null, shared_next_cursor: null };
  const bothServed = await serveFlows(page, bothPages, new Set([`before:${OWN_NEW.id}`, "shared:s6"]));
  try {
    await page.goto("/workflows");
    await expect(page.getByText(OWN_NEW.display_name)).toBeVisible({ timeout: 20_000 });
    await page.getByTestId("list-more-button").click();
    await expect(page.getByTestId("list-more")).toHaveText(
      /ワークフローと、共有されたワークフローの続きを読み込めませんでした。「さらに読み込む」でもう一度試します。/,
    );
    await page.getByTestId("list-more-button").click();
    await expect(page.getByText(OWN_MID.display_name)).toBeVisible();
    await expect(page.getByText(SHARED_OLD.display_name)).toBeVisible();
    await expect(page.getByTestId("list-more")).toHaveCount(0);
  } finally {
    await bothServed.stop();
  }

  if (process.env.E2E_SHOTS_DIR) {
    const phone = await browser.newContext({
      storageState: await page.context().storageState(),
      viewport: { width: 390, height: 844 },
      deviceScaleFactor: 2,
      isMobile: true,
      hasTouch: true,
    });
    try {
      const p = await phone.newPage();
      const phoneServed = await serveFlows(p, pages, new Set([`before:${OWN_NEW.id}`]));
      await p.goto("/workflows");
      await expect(p.getByText(OWN_NEW.display_name)).toBeVisible({ timeout: 20_000 });
      await p.getByTestId("list-more-button").click();
      await expect(p.getByTestId("list-more")).toContainText("読み込めませんでした");
      expect(await p.evaluate(() => document.documentElement.scrollWidth <= window.innerWidth + 1)).toBe(true);
      await p.screenshot({ path: `${process.env.E2E_SHOTS_DIR}/workflows-more-failed-phone.png` });
      await phoneServed.stop();
      const failServed = await serveFlows(p, {}, new Set());
      await p.goto("/workflows");
      await expect(p.getByTestId("list-load-failed")).toBeVisible({ timeout: 20_000 });
      await p.screenshot({ path: `${process.env.E2E_SHOTS_DIR}/workflows-load-failed-phone.png` });
      await failServed.stop();
    } finally {
      await phone.close();
    }
  }
});
