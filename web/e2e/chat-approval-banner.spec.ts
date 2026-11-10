import { expect, test, type Page } from "@playwright/test";

import { loginViaKeycloak, uniqueName } from "./helpers";

/// 承認待ちの知らせ（前提: LLM=stub）。
///
/// - 承認カードが画面外（上へ読み返している・入力欄の下に隠れている）にある間だけ、入力欄の上に
///   「承認が必要です」の帯を出す。帯から承認はさせず、「確認する」で承認カードへ移動するだけ。
/// - 読んでいる位置は勝手に動かさない（帯を出すだけ）。
/// - 承認待ちの間はタブのタイトルの先頭に「（承認待ち）」を付け、決まったら外す。
///
/// stub 駆動: `long:<段落数>` で長文、`calltool:<ツール名> <入力 JSON>` でツールを決定的に呼ぶ。

const MARK = "（承認待ち）";

async function csrf(page: Page): Promise<string> {
  return (await page.context().cookies()).find((c) => c.name === "shiki_csrf")?.value ?? "";
}

async function createFolder(page: Page, name: string): Promise<string> {
  const res = await page.request.post("/api/folders", {
    headers: { "X-CSRF-Token": await csrf(page) },
    data: { name, parent_id: null },
  });
  expect(res.status(), await res.text()).toBeLessThan(300);
  return ((await res.json()) as { id: string }).id;
}

async function createNote(page: Page, name: string): Promise<string> {
  const res = await page.request.post("/api/notes", {
    headers: { "X-CSRF-Token": await csrf(page) },
    data: { name, parent_id: null, markdown: `# ${name}\n` },
  });
  expect(res.status(), await res.text()).toBe(200);
  return ((await res.json()) as { id: string }).id;
}

/// 承認カードが会話の表示領域の内側（入力欄に隠れていない）にあるか。
function cardInView(page: Page): Promise<boolean> {
  return page.evaluate(() => {
    const scroller = document.querySelector('[data-testid="conversation-scroller"]');
    const card = document.querySelector('[data-testid="approval-card"]');
    const composer = document.querySelector('[data-testid="composer"]');
    if (!scroller || !card || !composer) return false;
    const box = scroller.getBoundingClientRect();
    const r = card.getBoundingClientRect();
    // 入力欄の枠（浮いている）の上端より下は見えていない。
    const floor = Math.min(box.bottom, composer.getBoundingClientRect().top);
    return r.top >= box.top - 1 && r.bottom <= floor + 1;
  });
}

const scrollTop = (page: Page) => page.getByTestId("conversation-scroller").evaluate((el) => el.scrollTop);

test("承認カードが画面外の間だけ帯を出し、「確認する」でカードへ移動する", async ({ page }) => {
  test.setTimeout(120_000);
  await loginViaKeycloak(page);
  const folderId = await createFolder(page, uniqueName("承認帯の移動先"));
  const noteId = await createNote(page, uniqueName("承認帯のメモ"));
  const input = page.getByLabel("メッセージを入力");

  // 読み返せる長さの会話にしておく。
  await page.goto("/");
  await input.fill("long:20");
  await page.getByRole("button", { name: "送信" }).click();
  await page.waitForURL(/\/c\/[0-9a-f-]+/i, { timeout: 20_000 });
  await expect(page.getByText(/^段落20。/)).toBeVisible({ timeout: 30_000 });
  await expect(page.getByRole("button", { name: "生成を停止" })).toHaveCount(0, { timeout: 30_000 });
  const baseTitle = await page.title();
  expect(baseTitle.startsWith(MARK)).toBe(false);

  // 承認が要る操作を頼む。送信直後は下端まで追うので、カードは見えていて帯は出ない。
  await input.fill(`calltool:drive.move {"node_id":"${noteId}","destination_folder_id":"${folderId}"}`);
  await page.getByRole("button", { name: "送信" }).click();
  const card = page.getByTestId("approval-card");
  await expect(card).toBeVisible({ timeout: 30_000 });
  await expect.poll(() => cardInView(page)).toBe(true);
  await expect(page.getByTestId("approval-banner")).toHaveCount(0);
  await expect.poll(() => page.title()).toBe(`${MARK}${baseTitle}`);

  // 上へ読み返すと、カードが画面外になるので帯が出る（「最新へ」の代わりに同じ位置へ）。
  const scroller = page.getByTestId("conversation-scroller");
  await scroller.hover();
  await page.mouse.wheel(0, -4_000);
  const banner = page.getByTestId("approval-banner");
  await expect(banner).toBeVisible();
  await expect(banner).toContainText("承認が必要です");
  await expect(page.getByTestId("jump-to-latest")).toHaveCount(0);
  // 帯を出しても読んでいる位置は動かさない（ホイールのスクロールが止まってから測る）。
  let last = Number.NaN;
  await expect
    .poll(async () => {
      const now = await scrollTop(page);
      const settled = now === last;
      last = now;
      return settled;
    })
    .toBe(true);
  const reading = await scrollTop(page);
  await page.waitForTimeout(800);
  expect(Math.abs((await scrollTop(page)) - reading)).toBeLessThanOrEqual(1);
  // 帯からは承認できない（承認は内容の見えるカードでだけ）。
  await expect(banner.getByRole("button")).toHaveCount(1);

  // 「確認する」でカードが入力欄の上に見える位置まで動き、帯は消え、カードにフォーカスが移る。
  await banner.getByRole("button", { name: "確認する" }).click();
  await expect.poll(() => cardInView(page)).toBe(true);
  await expect(banner).toHaveCount(0);
  await expect(card).toBeFocused();

  // 承認すると、タイトルの印が外れる。
  await card.getByRole("button", { name: "承認して続行" }).click();
  await expect(card).toHaveCount(0, { timeout: 30_000 });
  await expect.poll(async () => (await page.title()).startsWith(MARK)).toBe(false);
});
