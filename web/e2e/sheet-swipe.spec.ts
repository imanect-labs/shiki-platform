import { expect, test, type Browser, type Page } from "@playwright/test";

import { loginViaKeycloak } from "./helpers";

/// スマホの下からのシートを下へ払って閉じる（Dialog のシート化・Sheet side="bottom" の両方）。
/// 短く引いただけなら戻り、十分に引けば閉じる。指の動きは CDP のタッチイベントで再現する
/// （Playwright の touchscreen は tap しか持たないため）。

const PHONE = { width: 390, height: 844 };

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

/// (x, fromY) から (x, toY) まで指で引く。`end` が "touchCancel" なら離さずに中断し、
/// `holdMs` があれば離す前にその場で止まる。
async function swipe(
  page: Page,
  x: number,
  fromY: number,
  toY: number,
  { steps = 10, end = "touchEnd", holdMs = 0 }: { steps?: number; end?: "touchEnd" | "touchCancel"; holdMs?: number } = {},
) {
  const cdp = await page.context().newCDPSession(page);
  const point = (y: number) => [{ x, y, id: 1, radiusX: 4, radiusY: 4, force: 1 }];
  await cdp.send("Input.dispatchTouchEvent", { type: "touchStart", touchPoints: point(fromY) });
  for (let i = 1; i <= steps; i += 1) {
    await cdp.send("Input.dispatchTouchEvent", {
      type: "touchMove",
      touchPoints: point(fromY + ((toY - fromY) * i) / steps),
    });
    await page.waitForTimeout(16);
  }
  if (holdMs) await page.waitForTimeout(holdMs);
  await cdp.send("Input.dispatchTouchEvent", { type: end, touchPoints: [] });
  await cdp.detach();
}

/// シートの上端（取っ手の帯）を起点に引く。
async function swipeSheet(
  page: Page,
  testId: string,
  distance: number,
  opts?: Parameters<typeof swipe>[4],
) {
  const box = await page.getByTestId(testId).boundingBox();
  if (!box) throw new Error(`${testId} が見えない`);
  const y = box.y + 12;
  await swipe(page, box.x + box.width / 2, y, y + distance, opts);
}

test.describe("スマホのシートを払って閉じる", () => {
  test("＋メニュー（Dialog のシート）: 少し引くと戻り、大きく引くと閉じる", async ({ browser, page }) => {
    const phone = await phonePage(browser, page);
    await phone.goto("/");
    await phone.getByRole("button", { name: "追加メニューを開く" }).click();
    const sheet = phone.getByTestId("composer-plus-sheet");
    await expect(sheet).toBeVisible();

    await swipeSheet(phone, "composer-plus-sheet", 40);
    await phone.waitForTimeout(400);
    await expect(sheet).toBeVisible();
    // 戻ったあと、位置のずれ（transform）が残っていない。
    await expect(sheet).toHaveCSS("transform", "none");

    // 引いている途中で OS がタッチを中断しても閉じない（元へ戻る）。
    await swipeSheet(phone, "composer-plus-sheet", 320, { end: "touchCancel" });
    await phone.waitForTimeout(400);
    await expect(sheet).toBeVisible();
    await expect(sheet).toHaveCSS("transform", "none");

    // 素早く少しだけ引いて止めてから離すのは「払った」ではない（距離が足りなければ戻る）。
    await swipeSheet(phone, "composer-plus-sheet", 60, { steps: 2, holdMs: 400 });
    await phone.waitForTimeout(400);
    await expect(sheet).toBeVisible();

    await swipeSheet(phone, "composer-plus-sheet", 320);
    await expect(sheet).toBeHidden();
    await phone.context().close();
  });

  test("モデル選択（Sheet side=bottom）: 下へ払うと閉じる", async ({ browser, page }) => {
    const phone = await phonePage(browser, page);
    await phone.goto("/");
    // 既定のモデルのときは「+」のシートの「モデル」から開く。
    await phone.getByRole("button", { name: "追加メニューを開く" }).click();
    await phone.getByTestId("composer-open-model").click();
    const picker = phone.getByTestId("model-picker");
    await expect(picker).toBeVisible();

    await swipeSheet(phone, "model-picker", 300);
    await expect(picker).toBeHidden();
    await phone.context().close();
  });
});
