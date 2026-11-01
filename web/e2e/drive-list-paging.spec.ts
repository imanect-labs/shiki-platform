import { expect, test, type Page, type Route } from "@playwright/test";

import { cleanupQueue, createFolderApi, loginViaKeycloak, trashFolderApi, uniqueName } from "./helpers";

/// ドライブのフォルダーの中身と名前の検索で、候補を 1 件ずつ単発の check で確かめない（BUG-187）:
/// - 続きの位置は不透明（名前・id を含まない）で、別のフォルダー・別の検索の語・別の並び・読めない位置は 400
///   （cursor_invalid）。たどると重ならず漏れなく出る（本物の API）。
/// - サーバーが 0 件のページに続きの位置だけを付けて返しても、画面は空の状態に見せずに続きをたどり、往復の上限で
///   止まったら「さらに読み込む」を出す（応答は route で差し替える・見えないものばかりの大きな組織を手元に作らない）。
/// 作ったもの（フォルダー・ブラウザーの文脈）は最後に片付ける（失敗したテストでも走る）。
/// `E2E_SHOTS_DIR` があれば、PC（1440px）とスマホ（390px）の画面を撮る。

const SHOTS = process.env.E2E_SHOTS_DIR;
const cleanup = cleanupQueue();
test.afterEach(() => cleanup.run());

type Page1 = { items: { id: string; name: string }[]; next_cursor: string | null };

const hex = (s: string) => Buffer.from(s, "utf8").toString("hex");

test("フォルダーの中身と名前の検索の続きの位置は、一覧に束縛した不透明な文字列（BUG-187）", async ({ page }) => {
  test.setTimeout(120_000);
  await loginViaKeycloak(page);
  const tag = uniqueName("続きの位置");
  const top = await createFolderApi(page, tag);
  cleanup.add(() => trashFolderApi(page, top));
  const names = ["見積-1", "見積-2", "見積-3"].map((n) => `${n}-${tag}`);
  const ids: string[] = [];
  for (const n of names) ids.push(await createFolderApi(page, n, top));
  const other = await createFolderApi(page, `${tag}-別`);
  cleanup.add(() => trashFolderApi(page, other));

  const get = async (qs: string) => page.request.get(`/api/nodes?${qs}`);
  // フォルダーの中身: 1 件ずつたどると名前の順に重ならず出る。位置は名前・id を含まない。
  const seen: string[] = [];
  let cursor: string | null = null;
  let firstCursor: string | null = null;
  for (let round = 0; round < 10; round += 1) {
    const res = await get(`parent_id=${top}&limit=1${cursor ? `&cursor=${encodeURIComponent(cursor)}` : ""}`);
    expect(res.status(), await res.text()).toBe(200);
    const p = (await res.json()) as Page1;
    seen.push(...p.items.map((i) => i.id));
    firstCursor ??= p.next_cursor;
    cursor = p.next_cursor;
    if (!cursor) break;
  }
  expect(seen).toEqual(ids);
  expect(firstCursor).toBeTruthy();
  for (const s of [...ids, hex(names[0])]) expect(firstCursor).not.toContain(s);

  // 別のフォルダー・別の並び・読めない位置・前の版の平文の位置は 400（cursor_invalid）。
  const c = encodeURIComponent(firstCursor!);
  const plain = hex(`n${ids[0]}${names[0]}`);
  for (const qs of [
    `parent_id=${other}&limit=1&cursor=${c}`,
    `parent_id=${top}&limit=1&sort=name&desc=true&cursor=${c}`,
    `parent_id=${top}&limit=1&cursor=${c}x`,
    `parent_id=${top}&limit=1&cursor=${plain}`,
    `q=${encodeURIComponent(tag)}&limit=1&cursor=${c}`,
  ]) {
    const res = await get(qs);
    expect(res.status(), qs).toBe(400);
    expect(((await res.json()) as { code?: string }).code, qs).toBe("cursor_invalid");
  }

  // 名前の検索: 続きの位置は検索の語に束縛される。
  const q = encodeURIComponent(`見積 ${tag}`);
  const s1 = (await (await get(`q=${q}&limit=2`)).json()) as Page1;
  expect(s1.items.map((i) => i.id)).toEqual(ids.slice(0, 2));
  expect(s1.next_cursor).toBeTruthy();
  const s2 = (await (await get(`q=${q}&limit=2&cursor=${encodeURIComponent(s1.next_cursor!)}`)).json()) as Page1;
  expect(s2.items.map((i) => i.id)).toEqual(ids.slice(2));
  const wrong = await get(`q=${encodeURIComponent(`見積-1 ${tag}`)}&limit=2&cursor=${encodeURIComponent(s1.next_cursor!)}`);
  expect(wrong.status()).toBe(400);
});

const LIST = /\/api\/nodes(\?.*)?$/;

function folderNode(n: number) {
  return {
    id: `00000000-0000-4000-8000-${String(n).padStart(12, "0")}`,
    kind: "folder",
    name: `見つかったフォルダー ${n}`,
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

type Body = { items: unknown[]; next_cursor: string | null };

/// `/api/nodes` の頼みに、続きの位置ごとの応答を返す（`cursor` が無い頼みは `""`）。頼まれた位置を順に残す。
async function serveNodes(page: Page, pages: Record<string, Body>) {
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

/// 先頭から 5 ページは 0 件（見えないものばかり）で、6 ページ目に 1 件、7 ページ目で終わる。
function hiddenThenFound(): Record<string, Body> {
  const pages: Record<string, Body> = {};
  for (let i = 0; i < 5; i += 1) pages[i === 0 ? "" : `c${i}`] = { items: [], next_cursor: `c${i + 1}` };
  pages.c5 = { items: [folderNode(1)], next_cursor: "c6" };
  pages.c6 = { items: [folderNode(2)], next_cursor: null };
  return pages;
}

for (const where of [
  { label: "ドライブのトップ", path: "/drive", shot: "drive-top" },
  { label: "名前の検索", path: "/drive?q=%E8%A6%8B%E7%A9%8D", shot: "drive-search" },
]) {
  test(`${where.label}: 0 件のページでも続きをたどり、見つからないあいだは「さらに読み込む」を出す（BUG-187）`, async ({
    page,
    browser,
  }) => {
    test.setTimeout(120_000);
    await page.setViewportSize({ width: 1440, height: 900 });
    await loginViaKeycloak(page);
    const pages = hiddenThenFound();
    const served = await serveNodes(page, pages);
    try {
      await page.goto(where.path);
      // 往復の上限（5 回）まで読んでも見つからないあいだは、空の状態ではなく、続きを探せることを言う。
      const stalled = page.getByTestId("drive-more-stalled");
      await expect(stalled).toContainText("まだ見つかっていません。「さらに読み込む」で続きを探します。", { timeout: 20_000 });
      await expect(page.getByText("見つかりませんでした")).toHaveCount(0);
      await expect(page.getByText("このフォルダは空です")).toHaveCount(0);
      expect(served.asked.slice(0, 5)).toEqual(["", "c1", "c2", "c3", "c4"]);
      if (SHOTS) await page.screenshot({ path: `${SHOTS}/${where.shot}-pending-pc.png` });

      // 続きを読むと見つかったものが出て、見つかった後は下までスクロールすると残りも読む（続きが無くなるまで）。
      await page.getByTestId("drive-more").click();
      await expect(page.getByText("見つかったフォルダー 1", { exact: true }).first()).toBeVisible();
      await expect(page.getByText("見つかったフォルダー 2", { exact: true }).first()).toBeVisible();
      await expect(page.getByTestId("drive-more")).toHaveCount(0);
      if (SHOTS) await page.screenshot({ path: `${SHOTS}/${where.shot}-found-pc.png` });

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
          const phoneServed = await serveNodes(p, hiddenThenFound());
          await p.goto(where.path);
          await expect(p.getByTestId("drive-more-stalled")).toBeVisible({ timeout: 20_000 });
          expect(await p.evaluate(() => document.documentElement.scrollWidth <= window.innerWidth + 1)).toBe(true);
          await p.screenshot({ path: `${SHOTS}/${where.shot}-pending-phone.png` });
          await phoneServed.stop();
        } finally {
          await phone.close();
        }
      }
    } finally {
      await served.stop();
    }
  });
}
