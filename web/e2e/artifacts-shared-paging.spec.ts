import { expect, test, type Page, type Route } from "@playwright/test";

import { cleanupQueue, deleteArtifactApi, loginAs, loginViaKeycloak, uniqueName } from "./helpers";

/// アーティファクトの一覧（GET /artifacts）の共有された分が、続きの位置をたどって読める（BUG-162）:
/// - 共有された分が `limit` を超えると `shared_next_cursor` が付き、`shared_cursor` に渡すと共有された分の続きだけが
///   返る（重ならず新しい順）。位置は不透明（id を含まない）で、読めない位置は 400（cursor_invalid）。
/// - 画面（スキルの一覧）は、共有された分が 0 件のまま続きの位置だけが付いて返っても、続きをたどって出す（応答は
///   route で差し替える）。往復の上限で読み切れなかったら「さらに読み込む」を出し、続きの頼みが失敗しても自分の
///   ものは捨てずに、読み込めなかったことを言う。
/// 作ったもの（アーティファクト・ブラウザーの文脈）は最後に片付ける（失敗したテストでも走る）。

const cleanup = cleanupQueue();
test.afterEach(() => cleanup.run());

async function csrf(page: Page): Promise<Record<string, string>> {
  const token = (await page.context().cookies()).find((c) => c.name === "shiki_csrf")?.value ?? "";
  return { "x-csrf-token": token, "content-type": "application/json" };
}

type ListBody = { items: { id: string; owner: string }[]; shared_next_cursor?: string | null };

test("共有された分が多いと続きの位置が付き、shared_cursor で共有された分の続きだけを読める（BUG-162）", async ({
  page,
  browser,
}) => {
  test.setTimeout(120_000);
  await loginViaKeycloak(page);
  const alice = (await (await page.request.get("/api/me")).json()) as { id: string };
  const bobCtx = await browser.newContext();
  cleanup.add(() => bobCtx.close());
  const bob = await bobCtx.newPage();
  await loginAs(bob, "bob");
  // bob が script を 3 つ作り、alice に閲覧で共有する（後から作ったものほど新しい）。
  const shared: string[] = [];
  for (let i = 0; i < 3; i += 1) {
    const res = await bob.request.post("/api/artifacts", {
      headers: await csrf(bob),
      data: { kind: "script", name: uniqueName(`e2e-共有の続き-${i}`), body: { source: "export default 1;" } },
    });
    expect(res.status(), await res.text()).toBe(201);
    const id = ((await res.json()) as { id: string }).id;
    cleanup.add(() => deleteArtifactApi(bob, id));
    const s = await bob.request.put(`/api/artifacts/${id}/shares`, {
      headers: await csrf(bob),
      data: { target: { type: "user", id: alice.id }, role: "viewer" },
    });
    expect(s.status(), await s.text()).toBe(204);
    shared.push(id);
  }

  const first = (await (await page.request.get("/api/artifacts?kind=script&limit=1")).json()) as ListBody;
  const cursor0 = first.shared_next_cursor;
  expect(cursor0, "共有された分に続きがある").toBeTruthy();
  for (const id of shared) expect(cursor0).not.toContain(id);
  const seen = first.items.filter((a) => a.owner !== alice.id).map((a) => a.id);
  let cursor = cursor0 ?? null;
  for (let round = 0; cursor && round < 10; round += 1) {
    const p = (await (
      await page.request.get(`/api/artifacts?kind=script&limit=1&shared_cursor=${encodeURIComponent(cursor)}`)
    ).json()) as ListBody;
    // 続きは共有された分だけ（自分のものは返さない）。
    expect(p.items.every((a) => a.owner !== alice.id)).toBe(true);
    seen.push(...p.items.map((a) => a.id));
    cursor = p.shared_next_cursor ?? null;
  }
  expect(seen.filter((id) => shared.includes(id))).toEqual([...shared].reverse());
  expect(new Set(seen).size).toBe(seen.length);

  // 読めない位置は 400（cursor_invalid）。所有分の続き（before_*）と同時には使えない。
  const bad = await page.request.get(`/api/artifacts?kind=script&shared_cursor=${encodeURIComponent(`${cursor0}x`)}`);
  expect(bad.status()).toBe(400);
  expect(((await bad.json()) as { code?: string }).code).toBe("cursor_invalid");
  const both = await page.request.get(
    `/api/artifacts?kind=script&shared_cursor=${encodeURIComponent(cursor0 ?? "")}` +
      `&before_updated_at=${encodeURIComponent(new Date().toISOString())}&before_id=${shared[0]}`,
  );
  expect(both.status()).toBe(400);
});

const LIST = /\/api\/artifacts\?kind=skill(&.*)?$/;

test("スキルの一覧: 共有された分が 0 件のまま続きの位置が付いても、続きをたどって出す（BUG-162）", async ({ page }) => {
  test.setTimeout(90_000);
  await page.setViewportSize({ width: 1440, height: 900 });
  await loginViaKeycloak(page);
  const asked: string[] = [];
  const skill = {
    id: "00000000-0000-4000-8000-000000000162",
    kind: "skill",
    name: "続きで見つかったスキル",
    owner: "someone-else",
    current_version: 1,
    created_at: "2026-10-30T00:00:00Z",
    updated_at: "2026-10-30T00:00:00Z",
  };
  const handler = async (route: Route) => {
    if (route.request().method() !== "GET") return route.fallback();
    const cursor = new URL(route.request().url()).searchParams.get("shared_cursor") ?? "";
    asked.push(cursor);
    const body =
      cursor === "" ? { items: [], shared_next_cursor: "s1" } : cursor === "s1" ? { items: [], shared_next_cursor: "s2" } : { items: [skill], shared_next_cursor: null };
    return route.fulfill({ status: 200, contentType: "application/json", body: JSON.stringify(body) });
  };
  await page.route(LIST, handler);
  try {
    await page.goto("/skills");
    await expect(page.getByText("続きで見つかったスキル")).toBeVisible({ timeout: 15_000 });
    // ログインの後のホームの画面（スキルの選択）も同じ一覧を頼むので、最後の読み込みからの頼みを見る。
    expect(asked.slice(asked.lastIndexOf(""))).toEqual(["", "s1", "s2"]);
  } finally {
    await page.unroute(LIST, handler);
  }
});

function fakeSkill(n: number, name: string) {
  return {
    id: `00000000-0000-4000-8000-${String(n).padStart(12, "0")}`,
    kind: "skill",
    name,
    owner: "someone-else",
    current_version: 1,
    created_at: "2026-10-30T00:00:00Z",
    updated_at: `2026-10-${String(10 + n).padStart(2, "0")}T00:00:00Z`,
  };
}

/// スキルの一覧の頼みに、共有された分の続きの位置ごとの応答を返す（`fail` に入れた位置は 1 回だけ 500）。
async function serveSkills(page: Page, pages: Record<string, unknown>, fail: Set<string> = new Set()) {
  const asked: string[] = [];
  const handler = async (route: Route) => {
    if (route.request().method() !== "GET") return route.fallback();
    const cursor = new URL(route.request().url()).searchParams.get("shared_cursor") ?? "";
    asked.push(cursor);
    if (fail.delete(cursor) || !(cursor in pages)) {
      return route.fulfill({ status: 500, contentType: "application/json", body: JSON.stringify({ status: 500 }) });
    }
    return route.fulfill({ status: 200, contentType: "application/json", body: JSON.stringify(pages[cursor]) });
  };
  await page.route(LIST, handler);
  return { asked, stop: () => page.unroute(LIST, handler) };
}

test("スキルの一覧: 共有された分を往復の上限で読み切れなかったら「さらに読み込む」を出し、押すと続きを読む（BUG-162）", async ({
  page,
}) => {
  test.setTimeout(90_000);
  await page.setViewportSize({ width: 1440, height: 900 });
  await loginViaKeycloak(page);
  const pages: Record<string, unknown> = { "": { items: [fakeSkill(1, "自分のスキル")], shared_next_cursor: "s1" } };
  for (let i = 1; i <= 5; i += 1) pages[`s${i}`] = { items: [], shared_next_cursor: `s${i + 1}` };
  pages.s6 = { items: [fakeSkill(2, "続きで見つかったスキル")], shared_next_cursor: null };
  const served = await serveSkills(page, pages);
  try {
    await page.goto("/skills");
    await expect(page.getByText("自分のスキル")).toBeVisible({ timeout: 15_000 });
    const more = page.getByTestId("artifacts-shared-more");
    await expect(more).toHaveText(/共有されたスキルの続きは、まだ見つかっていません。「さらに読み込む」で続きを探します。/);
    const start = served.asked.lastIndexOf("");
    expect(served.asked.slice(start)).toEqual(["", "s1", "s2", "s3", "s4", "s5"]);
    await more.getByTestId("artifacts-shared-more-button").click();
    await expect(page.getByText("続きで見つかったスキル")).toBeVisible();
    await expect(page.getByTestId("artifacts-shared-more")).toHaveCount(0);
    expect(served.asked.slice(start + 6)).toEqual(["s6"]);
  } finally {
    await served.stop();
  }
});

test("スキルの一覧: 共有された分の続きの頼みが失敗しても、読めた分を出して読み込めなかったことを言う（BUG-162）", async ({
  page,
  browser,
}) => {
  test.setTimeout(90_000);
  await page.setViewportSize({ width: 1440, height: 900 });
  await loginViaKeycloak(page);
  const pages: Record<string, unknown> = {
    "": { items: [fakeSkill(1, "自分のスキル")], shared_next_cursor: "s1" },
    s1: { items: [fakeSkill(2, "続きで見つかったスキル")], shared_next_cursor: null },
  };
  const served = await serveSkills(page, pages, new Set(["s1"]));
  try {
    await page.goto("/skills");
    await expect(page.getByText("自分のスキル")).toBeVisible({ timeout: 15_000 });
    await expect(page.getByText("スキルはまだありません")).toHaveCount(0);
    const more = page.getByTestId("artifacts-shared-more");
    await expect(more).toHaveText(/共有されたスキルの続きを読み込めませんでした。「さらに読み込む」でもう一度試します。/);
    if (process.env.E2E_SHOTS_DIR) await page.screenshot({ path: `${process.env.E2E_SHOTS_DIR}/skills-shared-failed-pc.png` });
    // 押すと、失敗した位置から頼み直す。
    await more.getByTestId("artifacts-shared-more-button").click();
    await expect(page.getByText("続きで見つかったスキル")).toBeVisible();
    await expect(page.getByTestId("artifacts-shared-more")).toHaveCount(0);
    // ログインの後のホームの画面（スキルの選択）も同じ一覧を頼むので、最後の読み込みからの頼みを見る。
    expect(served.asked.slice(served.asked.lastIndexOf(""))).toEqual(["", "s1", "s1"]);
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
      const phoneServed = await serveSkills(p, pages, new Set(["s1"]));
      await p.goto("/skills");
      await expect(p.getByTestId("artifacts-shared-more")).toBeVisible({ timeout: 15_000 });
      expect(await p.evaluate(() => document.documentElement.scrollWidth <= window.innerWidth + 1)).toBe(true);
      await p.screenshot({ path: `${process.env.E2E_SHOTS_DIR}/skills-shared-failed-phone.png` });
      await phoneServed.stop();
    } finally {
      await phone.close();
    }
  }
});

test("スキルの一覧: 最初の読み込みが失敗したら、空の状態に見せずに読み込めなかったことを言う", async ({ page }) => {
  test.setTimeout(60_000);
  await loginViaKeycloak(page);
  const served = await serveSkills(page, {}, new Set());
  try {
    await page.goto("/skills");
    await expect(page.getByTestId("list-load-failed")).toContainText("スキルの一覧を読み込めませんでした。", { timeout: 15_000 });
    await expect(page.getByText("スキルはまだありません")).toHaveCount(0);
  } finally {
    await served.stop();
  }
});
