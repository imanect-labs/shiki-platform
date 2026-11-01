import { expect, test, type Page, type Route } from "@playwright/test";

import { cleanupQueue, createFolderApi, loginAs, loginViaKeycloak, trashFolderApi, uniqueName, writeFgaTuples } from "./helpers";

/// ドライブの「共有されたもの」が ListObjects の上限（1,000 件）で切れない（BUG-162）:
/// - 見られるものが上限を超える人にも、共有されたフォルダが出る（上限を超える詰め物のタプルを OpenFGA に直接書く）。
/// - 続きの位置は不透明（フォルダの id を含まない）で、読めない位置は 400（cursor_invalid）。
/// - サーバーが 0 件のページに続きの位置だけを付けて返しても、画面は空の状態に見せずに続きをたどり、往復の上限で
///   止まったら「さらに読み込む」を出す（応答は route で差し替える・大きな組織を手元に作らない）。
/// 作ったもの（フォルダ・共有・タプル・ブラウザーの文脈）は最後に片付ける（失敗したテストでも走る）。
/// `E2E_SHOTS_DIR` があれば、PC（1440px）とスマホ（390px）の画面を撮る。

const SHOTS = process.env.E2E_SHOTS_DIR;
const cleanup = cleanupQueue();
test.afterEach(() => cleanup.run());

type Me = { id: string; tenant_id: string };

async function me(page: Page): Promise<Me> {
  return (await (await page.request.get("/api/me")).json()) as Me;
}

async function csrf(page: Page): Promise<Record<string, string>> {
  const token = (await page.context().cookies()).find((c) => c.name === "shiki_csrf")?.value ?? "";
  return { "x-csrf-token": token, "content-type": "application/json" };
}

/// alice がフォルダを作り、`userId` に閲覧で共有する（片付けも積む）。
async function sharedFolder(page: Page, name: string, userId: string): Promise<string> {
  const id = await createFolderApi(page, name);
  cleanup.add(() => trashFolderApi(page, id));
  const res = await page.request.put(`/api/nodes/${id}/shares`, {
    headers: await csrf(page),
    data: { target: { type: "user", id: userId }, role: "viewer" },
  });
  expect(res.status(), await res.text()).toBe(204);
  return id;
}

test("見られるものが上限を超える人にも、共有されたフォルダが「共有済み」に出る（BUG-162）", async ({ page, browser }) => {
  test.setTimeout(180_000);
  await loginViaKeycloak(page);
  const bobCtx = await browser.newContext();
  cleanup.add(() => bobCtx.close());
  const bob = await bobCtx.newPage();
  await loginAs(bob, "bob");
  const bobMe = await me(bob);
  // 詰め物（DB に無いフォルダへの bob の閲覧）を先に積み、本物の共有は後から張る（ListObjects が返す 1,000 件に
  // 入らない位置）。
  const fillers = Array.from({ length: 1_100 }, () => ({
    user: `user:${bobMe.tenant_id}|${bobMe.id}`,
    relation: "viewer",
    object: `folder:${bobMe.tenant_id}|${crypto.randomUUID()}`,
  }));
  cleanup.add(await writeFgaTuples(page, fillers));
  const names = [uniqueName("上限の外の共有-1"), uniqueName("上限の外の共有-2")];
  const ids: string[] = [];
  for (const n of names) ids.push(await sharedFolder(page, n, bobMe.id));

  // 画面: 両方とも「共有済み」に出る。
  await bob.goto("/drive/shared");
  for (const n of names) await expect(bob.getByText(n, { exact: true })).toBeVisible({ timeout: 20_000 });

  // API: 1 件ずつ読むと続きの位置が付き、たどると両方が重ならず新しい順に出る。位置は id を含まない。
  const first = (await (await bob.request.get("/api/shares/shared-with-me?limit=1")).json()) as {
    items: { id: string }[];
    next_cursor: string | null;
  };
  expect(first.next_cursor).toBeTruthy();
  for (const id of ids) expect(first.next_cursor).not.toContain(id);
  const seen = [...first.items.map((i) => i.id)];
  let cursor = first.next_cursor;
  for (let round = 0; cursor && round < 10; round += 1) {
    const p = (await (await bob.request.get(`/api/shares/shared-with-me?limit=1&cursor=${encodeURIComponent(cursor)}`)).json()) as {
      items: { id: string }[];
      next_cursor: string | null;
    };
    seen.push(...p.items.map((i) => i.id));
    cursor = p.next_cursor;
  }
  expect(seen.filter((id) => ids.includes(id))).toEqual([...ids].reverse());
  expect(new Set(seen).size).toBe(seen.length);
  // 読めない位置（改竄・前の版の平文の位置）は 400。別の人の位置も読めない（テナントと鍵に束縛）。
  const bad = await bob.request.get(`/api/shares/shared-with-me?cursor=${encodeURIComponent(`${first.next_cursor}x`)}`);
  expect(bad.status()).toBe(400);
  expect(((await bad.json()) as { code?: string }).code).toBe("cursor_invalid");
});

const LIST = /\/api\/shares\/shared-with-me(\?.*)?$/;

function node(n: number) {
  return {
    id: `00000000-0000-4000-8000-${String(n).padStart(12, "0")}`,
    kind: "folder",
    name: `見つかったフォルダ ${n}`,
    parent_id: null,
    size_bytes: null,
    content_type: null,
    version: 1,
    created_by: "someone-else",
    updated_by: "someone-else",
    created_at: "2026-10-30T00:00:00Z",
    updated_at: "2026-10-30T00:00:00Z",
  };
}

/// 一覧の頼みに、続きの位置ごとの応答を返す（`cursor` が無い頼みは `""`）。頼まれた位置を順に残す。
async function serveList(page: Page, pages: Record<string, { items: unknown[]; next_cursor: string | null }>) {
  const asked: string[] = [];
  const handler = async (route: Route) => {
    if (route.request().method() !== "GET") return route.fallback();
    const cursor = new URL(route.request().url()).searchParams.get("cursor") ?? "";
    asked.push(cursor);
    const body = pages[cursor];
    if (!body) return route.fulfill({ status: 500, contentType: "application/json", body: JSON.stringify({ status: 500 }) });
    return route.fulfill({ status: 200, contentType: "application/json", body: JSON.stringify(body) });
  };
  await page.route(LIST, handler);
  return { asked, stop: () => page.unroute(LIST, handler) };
}

test("「共有済み」: 0 件のページでも続きをたどり、見つからないあいだは「さらに読み込む」を出す（BUG-162）", async ({
  page,
  browser,
}) => {
  test.setTimeout(120_000);
  await page.setViewportSize({ width: 1440, height: 900 });
  await loginViaKeycloak(page);
  // 先頭から 5 ページは 0 件（見えないものばかり）で、6 ページ目に 1 件、7 ページ目で終わる。
  const pages: Record<string, { items: unknown[]; next_cursor: string | null }> = {};
  for (let i = 0; i < 5; i += 1) pages[i === 0 ? "" : `c${i}`] = { items: [], next_cursor: `c${i + 1}` };
  pages.c5 = { items: [node(1)], next_cursor: "c6" };
  pages.c6 = { items: [node(2)], next_cursor: null };
  const served = await serveList(page, pages);
  try {
    await page.goto("/drive/shared");
    // 往復の上限（5 回）まで読んでも見つからないあいだは、空の状態ではなく、続きを探せることを言う。
    const pending = page.getByTestId("drive-shared-pending");
    await expect(pending).toContainText("まだ見つかっていません。「さらに読み込む」で続きを探します。", { timeout: 15_000 });
    await expect(page.getByText("共有されたアイテムはありません")).toHaveCount(0);
    expect(served.asked).toEqual(["", "c1", "c2", "c3", "c4"]);
    if (SHOTS) await page.screenshot({ path: `${SHOTS}/drive-shared-pending-pc.png` });

    // 続きを読むと見つかったものが出て、見つかった後は下までスクロールすると残りも読む（続きが無くなるまで）。
    await page.getByTestId("drive-shared-more").click();
    await expect(page.getByText("見つかったフォルダ 1", { exact: true })).toBeVisible();
    await expect(page.getByText("見つかったフォルダ 2", { exact: true })).toBeVisible();
    await expect(page.getByTestId("drive-shared-more")).toHaveCount(0);
    expect(served.asked.slice(5)).toEqual(["c5", "c6"]);
    if (SHOTS) await page.screenshot({ path: `${SHOTS}/drive-shared-found-pc.png` });

    if (SHOTS) {
      const phone = await browser.newContext({
        storageState: await page.context().storageState(),
        viewport: { width: 390, height: 844 },
        deviceScaleFactor: 2,
        isMobile: true,
        hasTouch: true,
      });
      try {
        const p = await phone.newPage();
        const phoneServed = await serveList(p, pages);
        await p.goto("/drive/shared");
        await expect(p.getByTestId("drive-shared-pending")).toBeVisible({ timeout: 15_000 });
        expect(await p.evaluate(() => document.documentElement.scrollWidth <= window.innerWidth + 1)).toBe(true);
        await p.screenshot({ path: `${SHOTS}/drive-shared-pending-phone.png` });
        await phoneServed.stop();
      } finally {
        await phone.close();
      }
    }
  } finally {
    await served.stop();
  }
});

test("「共有済み」: 見つかった後も、何も見つからない読み込みが続いたら自動で読むのをやめ、末尾に「さらに読み込む」を出す（BUG-162）", async ({
  page,
}) => {
  test.setTimeout(120_000);
  await page.setViewportSize({ width: 1440, height: 900 });
  await loginViaKeycloak(page);
  // 先頭に 1 件、そのあと 9 ページは 0 件（見えないものばかり）で、10 ページ目に 1 件、そこで終わる。
  const pages: Record<string, { items: unknown[]; next_cursor: string | null }> = {
    "": { items: [node(1)], next_cursor: "c1" },
  };
  for (let i = 1; i < 10; i += 1) pages[`c${i}`] = { items: [], next_cursor: `c${i + 1}` };
  pages.c10 = { items: [node(2)], next_cursor: null };
  const served = await serveList(page, pages);
  try {
    await page.goto("/drive/shared");
    await expect(page.getByText("見つかったフォルダ 1", { exact: true })).toBeVisible({ timeout: 15_000 });
    // 1 件目の読み込み（往復 5 回）の後、末尾が見えているので 1 回だけ読み足す（往復 5 回）。そこで何も見つからない
    // ので、末尾に「さらに読み込む」を出して止める（それ以上は頼まない）。
    const stalled = page.getByTestId("drive-shared-stalled");
    await expect(stalled).toContainText("続きはまだ見つかっていません。「さらに読み込む」で続きを探します。", {
      timeout: 15_000,
    });
    await page.waitForTimeout(2_000);
    expect(served.asked).toEqual(["", "c1", "c2", "c3", "c4", "c5", "c6", "c7", "c8", "c9"]);
    if (SHOTS) await page.screenshot({ path: `${SHOTS}/drive-shared-stalled-pc.png` });

    // 押すと続きを探し、見つかったものが出て、続きが無くなればボタンも消える。
    await stalled.getByTestId("drive-shared-more").click();
    await expect(page.getByText("見つかったフォルダ 2", { exact: true })).toBeVisible();
    await expect(page.getByTestId("drive-shared-stalled")).toHaveCount(0);
    expect(served.asked.slice(10)).toEqual(["c10"]);
  } finally {
    await served.stop();
  }
});
