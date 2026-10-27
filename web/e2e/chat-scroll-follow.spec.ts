import { expect, test, type Browser, type Page } from "@playwright/test";

import { loginViaKeycloak } from "./helpers";

/// 生成中のスクロール追従: 下端にいる間だけ追い、上へスクロールしたら読んでいる位置を保つ。
/// 前提: LLM=stub。`long:<段落数>` は番号付き段落の長文を数文字ずつ流す（生成中に読み返せる長さ）。
///
/// 上へは「小刻みに」動かす（トラックパッドや指のゆっくりした操作は 1 回の動きが 1〜2px で、
/// それを下端扱いにして引き戻していた）。

const PHONE = { width: 390, height: 844 };

type Metrics = { top: number; bottomGap: number; textLength: number };

function metrics(page: Page): Promise<Metrics> {
  return page.getByTestId("conversation-scroller").evaluate((el) => ({
    top: el.scrollTop,
    bottomGap: el.scrollHeight - el.scrollTop - el.clientHeight,
    textLength: el.textContent?.length ?? 0,
  }));
}

/// 長文を送り、本文が画面を超えて伸びても下端へ追従していることを確かめる。
async function startLongAnswer(page: Page, send: () => Promise<void>) {
  const input = page.getByLabel("メッセージを入力");
  await input.fill("long:150");
  await send();
  await page.waitForURL(/\/c\/[0-9a-f-]+/i, { timeout: 20_000 });
  await expect(page.getByRole("button", { name: "生成を停止" })).toBeVisible({ timeout: 30_000 });
  // 1 画面ぶん以上伸びて、なお下端にいる（＝追従している）。
  await expect.poll(async () => (await metrics(page)).top, { timeout: 30_000 }).toBeGreaterThan(600);
  await expect.poll(async () => (await metrics(page)).bottomGap, { timeout: 5_000 }).toBeLessThanOrEqual(8);
}

/// 上へ動かしたあと、生成が進んでも読んでいる位置が動かないことを確かめる。
async function expectPositionHeld(page: Page) {
  // スクロールの慣性が落ち着くのを待ってから位置を取る。
  await page.waitForTimeout(600);
  const before = await metrics(page);
  // 下端から離れている（追従で引き戻されていない）。
  expect(before.bottomGap).toBeGreaterThan(30);
  await page.waitForTimeout(1_500);
  const after = await metrics(page);
  // 生成はまだ続いている（止まっていたら位置が動かないのは当然で、確かめたことにならない）。
  expect(after.textLength).toBeGreaterThan(before.textLength);
  await expect(page.getByRole("button", { name: "生成を停止" })).toBeVisible();
  expect(Math.abs(after.top - before.top)).toBeLessThanOrEqual(1);
  await expect(page.getByTestId("jump-to-latest")).toBeVisible();
}

/// 「最新へ」で下端へ戻ると、再び生成に合わせて追従する。
async function expectFollowAfterJump(page: Page) {
  await page.getByTestId("jump-to-latest").click();
  await expect.poll(async () => (await metrics(page)).bottomGap, { timeout: 5_000 }).toBeLessThanOrEqual(8);
  const before = await metrics(page);
  await page.waitForTimeout(1_000);
  await expect.poll(async () => (await metrics(page)).bottomGap, { timeout: 5_000 }).toBeLessThanOrEqual(8);
  const after = await metrics(page);
  expect(after.textLength).toBeGreaterThan(before.textLength);
  expect(after.top).toBeGreaterThan(before.top);
  await expect(page.getByTestId("jump-to-latest")).toBeHidden();
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

test.describe("生成中のスクロール追従", () => {
  test.setTimeout(120_000);

  test("PC: ホイールで少し上へ動かすと追従が止まり、最新へで戻ると再び追う", async ({ page }) => {
    await loginViaKeycloak(page);
    await page.goto("/");
    await startLongAnswer(page, () => page.getByLabel("メッセージを入力").press("Enter"));

    const box = await page.getByTestId("conversation-scroller").boundingBox();
    await page.mouse.move(box!.x + box!.width / 2, box!.y + box!.height / 3);
    // トラックパッドのような 2px ずつの動きだけで上へ。
    for (let i = 0; i < 40; i += 1) {
      await page.mouse.wheel(0, -2);
      await page.waitForTimeout(16);
    }
    await expectPositionHeld(page);

    // 大きく上へ動かしてから下へ少し戻しても、下端から離れている間は追わない。
    await page.mouse.wheel(0, -600);
    await page.mouse.wheel(0, 100);
    await expectPositionHeld(page);

    await expectFollowAfterJump(page);
  });

  test("スマホ: 指でゆっくり上へ動かすと追従が止まり、最新へで戻ると再び追う", async ({
    browser,
    page,
  }) => {
    const phone = await phonePage(browser, page);
    await phone.goto("/");
    await startLongAnswer(phone, () => phone.getByRole("button", { name: "送信" }).click());

    // 指の操作（touchstart → touchmove → touchend）で、1 回 2px ずつゆっくり上へ。
    const cdp = await phone.context().newCDPSession(phone);
    const x = PHONE.width / 2;
    const y0 = 300;
    await cdp.send("Input.dispatchTouchEvent", { type: "touchStart", touchPoints: [{ x, y: y0 }] });
    for (let i = 1; i <= 100; i += 1) {
      await cdp.send("Input.dispatchTouchEvent", {
        type: "touchMove",
        touchPoints: [{ x, y: y0 + i * 2 }],
      });
      await phone.waitForTimeout(16);
    }
    await cdp.send("Input.dispatchTouchEvent", { type: "touchEnd", touchPoints: [] });
    await expectPositionHeld(phone);

    await expectFollowAfterJump(phone);
  });
});
