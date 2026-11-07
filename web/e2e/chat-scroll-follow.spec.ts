import { expect, test, type Browser, type Page } from "@playwright/test";

import { loginViaKeycloak } from "./helpers";

/// 生成中のスクロール: 回答の先頭（生成開始地点）が画面の上端に来るまでだけ追い、そこで止める。
/// 以降は生成が続いても動かさない（上から読み進める）。「最新へ」は下端へ一度動かすだけ。
/// 前提: LLM=stub。`long:<段落数>` は番号付き段落の長文を、`longthink:<段落数>` は思考を流してから
/// 長文を、数文字ずつ流す（生成中に読める長さ）。

const PHONE = { width: 390, height: 844 };

type Metrics = { top: number; bottomGap: number; textLength: number; answerOffset: number | null };

/// スクロール位置と、生成中の回答の先頭が上端（本文の上の余白の位置）からどれだけ下にあるか。
function metrics(page: Page): Promise<Metrics> {
  return page.getByTestId("conversation-scroller").evaluate((el) => {
    const row = el.querySelector('[data-testid="streaming-row"]');
    const inset = parseFloat(getComputedStyle(el.firstElementChild!).paddingTop) || 0;
    return {
      top: el.scrollTop,
      bottomGap: el.scrollHeight - el.scrollTop - el.clientHeight,
      textLength: el.textContent?.length ?? 0,
      answerOffset: row
        ? row.getBoundingClientRect().top - el.getBoundingClientRect().top - inset
        : null,
    };
  });
}

const generating = (page: Page) => page.getByRole("button", { name: "生成を停止" });

/// 1 往復済みの会話にする（2 通目の回答は画面の下の方から始まり、上端まで追う区間ができる）。
async function startConversation(page: Page, send: (text: string) => Promise<void>) {
  await send("long:12");
  await page.waitForURL(/\/c\/[0-9a-f-]+/i, { timeout: 20_000 });
  await expect(page.getByText(/^段落12。/)).toBeVisible({ timeout: 30_000 });
  await expect(generating(page)).toHaveCount(0, { timeout: 30_000 });
}

/// 長文を送り、回答の先頭が上端に来るまで下へ追っていることを確かめる。
async function sendLongAnswer(page: Page, send: (text: string) => Promise<void>, text = "long:150") {
  const sentAt = (await metrics(page)).top;
  await send(text);
  await expect(generating(page)).toBeVisible({ timeout: 30_000 });
  await expect.poll(async () => (await metrics(page)).top, { timeout: 30_000 }).toBeGreaterThan(sentAt);
}

/// 回答の先頭が上端で止まり、生成が続いても動かないことを確かめる。
async function expectStoppedAtAnswerStart(page: Page) {
  await expect
    .poll(async () => Math.abs((await metrics(page)).answerOffset ?? Number.NaN), { timeout: 30_000 })
    .toBeLessThanOrEqual(2);
  await expectPositionHeld(page);
}

/// 生成が進んでも読んでいる位置が動かないことを確かめる。
async function expectPositionHeld(page: Page) {
  // スクロールの慣性が落ち着くのを待ってから位置を取る。
  await page.waitForTimeout(600);
  const before = await metrics(page);
  expect(before.bottomGap).toBeGreaterThan(30);
  await page.waitForTimeout(1_500);
  const after = await metrics(page);
  // 生成はまだ続いている（止まっていたら位置が動かないのは当然で、確かめたことにならない）。
  expect(after.textLength).toBeGreaterThan(before.textLength);
  await expect(generating(page)).toBeVisible();
  expect(Math.abs(after.top - before.top)).toBeLessThanOrEqual(1);
  await expect(page.getByTestId("jump-to-latest")).toBeVisible();
}

/// 「最新へ」は下端へ一度動かすだけで、生成中でも追わない。
async function expectJumpWithoutFollow(page: Page) {
  // 追わないので、下端に着いた直後から本文の伸びたぶん離れていく。着いた瞬間をスクロールの
  // たびに測って拾う（一定間隔で見ると取りこぼす）。測るのは位置を変えた次の描画なので、その間に
  // 伸びた 1〜2 行は許す（「最新へ」が消える距離＝下端付近の内側に着けばよい）。
  const scroller = page.getByTestId("conversation-scroller");
  await scroller.evaluate((el) => {
    const w = window as unknown as { minGap: number };
    w.minGap = Infinity;
    el.addEventListener("scroll", () => {
      w.minGap = Math.min(w.minGap, el.scrollHeight - el.scrollTop - el.clientHeight);
    });
  });
  await page.getByTestId("jump-to-latest").click();
  await expect
    .poll(() => page.evaluate(() => (window as unknown as { minGap: number }).minGap), {
      timeout: 5_000,
    })
    .toBeLessThanOrEqual(60);
  await page.waitForTimeout(400);
  const before = await metrics(page);
  await page.waitForTimeout(1_500);
  const after = await metrics(page);
  expect(after.textLength).toBeGreaterThan(before.textLength);
  expect(Math.abs(after.top - before.top)).toBeLessThanOrEqual(1);
  expect(after.bottomGap).toBeGreaterThan(before.bottomGap);
}

async function phonePage(browser: Browser, page: Page): Promise<Page> {
  await loginViaKeycloak(page);
  const ctx = await browser.newContext({
    storageState: await page.context().storageState(),
    viewport: PHONE,
    hasTouch: true,
    isMobile: true,
    deviceScaleFactor: 3,
    ignoreHTTPSErrors: true,
  });
  return ctx.newPage();
}

const pcSend = (page: Page) => async (text: string) => {
  await page.getByLabel("メッセージを入力").fill(text);
  await page.getByLabel("メッセージを入力").press("Enter");
};
const phoneSend = (page: Page) => async (text: string) => {
  await page.getByLabel("メッセージを入力").fill(text);
  await page.getByRole("button", { name: "送信" }).click();
};

test.describe("生成中のスクロール", () => {
  test.setTimeout(120_000);

  test("PC: 回答の先頭が上端に来るまで追ってそこで止まり、「最新へ」は追わない", async ({ page }) => {
    await loginViaKeycloak(page);
    await page.goto("/");
    await startConversation(page, pcSend(page));
    await sendLongAnswer(page, pcSend(page));
    await expectStoppedAtAnswerStart(page);
    await expectJumpWithoutFollow(page);
  });

  test("スマホ: 回答の先頭が上端（浮いたボタンの下）に来るまで追ってそこで止まる", async ({
    browser,
    page,
  }) => {
    const phone = await phonePage(browser, page);
    await phone.goto("/");
    await startConversation(phone, phoneSend(phone));
    await sendLongAnswer(phone, phoneSend(phone));
    await expectStoppedAtAnswerStart(phone);
    await expectJumpWithoutFollow(phone);
  });

  test("PC: 先頭に着く前でも、上へ少し動かしたらそこで止まる", async ({ page }) => {
    await loginViaKeycloak(page);
    await page.goto("/");
    await startConversation(page, pcSend(page));
    await sendLongAnswer(page, pcSend(page));
    const box = await page.getByTestId("conversation-scroller").boundingBox();
    await page.mouse.move(box!.x + box!.width / 2, box!.y + box!.height / 3);
    // トラックパッドのような 2px ずつの動き。
    for (let i = 0; i < 20; i += 1) {
      await page.mouse.wheel(0, -2);
      await page.waitForTimeout(16);
    }
    await expectPositionHeld(page);
    // 先頭までは追っていない（止まり位置まで動かされていない）。
    expect((await metrics(page)).answerOffset ?? 0).toBeGreaterThan(20);
  });

  test("思考を開いたままでも先頭で止まり、生成が終わって思考が閉じたり位置が飛んだりしない", async ({
    page,
  }) => {
    await loginViaKeycloak(page);
    await page.goto("/");
    await startConversation(page, pcSend(page));
    await sendLongAnswer(page, pcSend(page), "longthink:25");
    // 思考が流れている間に、思考パネルと詳細な思考を開く。
    await page.getByRole("button", { name: /考えています/ }).click();
    await page.getByRole("button", { name: "詳細な思考を表示" }).click();
    await expectStoppedAtAnswerStart(page);

    // 読んでいる詳細な思考の位置（画面上の y）。
    const detail = page.getByText(/^思考1。/);
    const where = async () => ({
      top: (await metrics(page)).top,
      y: Math.round((await detail.boundingBox())?.y ?? Number.NaN),
    });
    const before = await where();
    await expect(generating(page)).toHaveCount(0, { timeout: 60_000 });
    // 確定（生成中の行 → 確定した行）と、その後の読み直し（→ サーバの行）を待つ。
    await page.waitForTimeout(3_000);
    await expect(page.getByRole("button", { name: "詳細な思考を隠す" })).toBeVisible();
    expect(await where()).toEqual(before);
  });
});
