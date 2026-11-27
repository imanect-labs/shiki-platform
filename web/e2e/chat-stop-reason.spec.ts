import { expect, test, type Browser, type Page } from "@playwright/test";

import { loginViaKeycloak } from "./helpers";

/// 回答が途中で止まったとき、回答の下に理由と続け方の注記を出す（GAP-106）。
///
/// 前提: LLM=stub。`truncate:<本文>` は「回答: <本文>」を返して出力の上限で終える。`loop:drive.find` は毎ターン
/// ドライブの検索を呼び続けるので、手数の上限（通常チャットの max_steps）で止まる。先頭のツール（`loop:`）は
/// ファイルの複製で、承認が要るため承認待ちで止まり、手数の上限まで届かない。
/// 注記は生成の直後（done イベントの理由）と、読み直したとき（メッセージの理由）の両方で出る。

const SHOTS = process.env.E2E_SHOTS_DIR;
const PHONE = { width: 390, height: 844 };

async function send(page: Page, text: string): Promise<void> {
  const input = page.getByLabel("メッセージを入力");
  await input.click();
  await input.fill(text);
  await page.getByRole("button", { name: "送信" }).click();
}

/// 生成が終わるまで待つ（停止ボタンが消える）。
async function waitIdle(page: Page): Promise<void> {
  await expect(page.getByRole("button", { name: "生成を停止" })).toHaveCount(0, { timeout: 60_000 });
}

test.describe("途中で止まった回答の注記", () => {
  test("出力の上限で切れた回答に、生成の直後も読み直した後も注記が出る", async ({ page }) => {
    await loginViaKeycloak(page);
    await page.goto("/");
    const tag = Date.now().toString(36);

    await send(page, `truncate:途中で切れる説明 ${tag}`);
    // 手元は next dev なので、移った先の画面が出るまで待つ。
    await page.waitForURL(/\/c\/[0-9a-f-]+/i, { timeout: 20_000 });
    await expect(page.getByText(`回答: 途中で切れる説明 ${tag}`).first()).toBeVisible({
      timeout: 30_000,
    });
    await waitIdle(page);

    // 生成の直後（仮の行）に、回答の行の中で注記が出る。
    const note = page.getByTestId("stop-reason-note");
    await expect(note).toHaveCount(1, { timeout: 20_000 });
    await expect(note).toHaveAttribute("data-stop-reason", "truncated");
    await expect(note).toHaveText(
      "回答が長くなったため、途中で打ち切りました。続きが必要なら「続けて」と送ってください。",
    );
    const row = page.locator("[data-message-state]").filter({ has: note });
    await expect(row).toContainText(`回答: 途中で切れる説明 ${tag}`);
    // 生成中の予算の警告は生成の行だけのもので、確定した回答の注記と並ばない。
    await expect(page.getByText("予算警告")).toHaveCount(0);
    if (SHOTS) await page.screenshot({ path: `${SHOTS}/stop-reason-truncated-live-pc.png` });

    // 読み直しても注記が残る（メッセージに保存した理由）。
    await page.reload();
    const saved = page.locator('[data-message-state="saved"]').filter({ has: page.getByTestId("stop-reason-note") });
    await expect(saved).toHaveCount(1, { timeout: 30_000 });
    await expect(saved).toContainText(`回答: 途中で切れる説明 ${tag}`);
    await expect(saved.getByTestId("stop-reason-note")).toHaveAttribute("data-stop-reason", "truncated");

    // 自然に終わった回答には出ない（注記は 1 件のまま）。
    await send(page, `ふつうの質問 ${tag}`);
    await expect(page.getByText(`回答: ふつうの質問 ${tag}`).first()).toBeVisible({ timeout: 30_000 });
    await waitIdle(page);
    await expect(page.getByTestId("stop-reason-note")).toHaveCount(1);
    await page.reload();
    await expect(page.getByText(`回答: ふつうの質問 ${tag}`).first()).toBeVisible({ timeout: 30_000 });
    await expect(page.getByTestId("stop-reason-note")).toHaveCount(1);
    if (SHOTS) await page.screenshot({ path: `${SHOTS}/stop-reason-truncated-saved-pc.png` });
  });

  test("手順の上限で止まった回答に注記が出る", async ({ page }) => {
    await loginViaKeycloak(page);
    await page.goto("/");

    await send(page, "loop:drive.find");
    await page.waitForURL(/\/c\/[0-9a-f-]+/i, { timeout: 20_000 });
    await waitIdle(page);

    const note = page.getByTestId("stop-reason-note");
    await expect(note).toHaveCount(1, { timeout: 30_000 });
    await expect(note).toHaveAttribute("data-stop-reason", "step_limit");
    await expect(note).toContainText("手順の上限に達したため、途中で止めました。");
    await expect(page.getByText("予算警告")).toHaveCount(0);
    if (SHOTS) await page.screenshot({ path: `${SHOTS}/stop-reason-step-limit-pc.png` });

    await page.reload();
    await expect(page.locator('[data-message-state="saved"]').getByTestId("stop-reason-note")).toHaveAttribute(
      "data-stop-reason",
      "step_limit",
      { timeout: 30_000 },
    );
  });

  test("スマホの幅でも注記が回答の下に収まり、横にはみ出さない", async ({ browser, page }) => {
    const phone = await phonePage(browser, page);
    await phone.goto("/");
    const tag = Date.now().toString(36);

    await send(phone, `truncate:スマホで切れる説明 ${tag}`);
    await phone.waitForURL(/\/c\/[0-9a-f-]+/i, { timeout: 20_000 });
    await expect(phone.getByText(`回答: スマホで切れる説明 ${tag}`).first()).toBeVisible({
      timeout: 30_000,
    });
    await waitIdle(phone);
    await phone.reload();

    const note = phone.getByTestId("stop-reason-note");
    await expect(note).toBeVisible({ timeout: 30_000 });
    // 注記は画面の幅に収まる（左右の余白の内側）。
    const box = await note.boundingBox();
    expect(box).not.toBeNull();
    expect(box!.x).toBeGreaterThanOrEqual(0);
    expect(box!.x + box!.width).toBeLessThanOrEqual(PHONE.width);
    const overflow = await phone.evaluate(() => {
      const scroller = document.querySelector<HTMLElement>('[data-testid="conversation-scroller"]');
      return {
        page: document.documentElement.scrollWidth - window.innerWidth,
        scroller: scroller ? scroller.scrollWidth - scroller.clientWidth : 0,
      };
    });
    expect(overflow.page).toBeLessThanOrEqual(0);
    expect(overflow.scroller).toBeLessThanOrEqual(0);
    await note.scrollIntoViewIfNeeded();
    if (SHOTS) await phone.screenshot({ path: `${SHOTS}/stop-reason-truncated-phone.png` });
    await phone.context().close();
  });
});

/// ログインは PC の画面で済ませ、そのセッションをスマホの文脈へ持ち込む（chat-mobile.spec と同じ）。
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
