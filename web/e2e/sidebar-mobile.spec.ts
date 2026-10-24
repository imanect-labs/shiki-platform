import { expect, test, type Browser, type Page } from "@playwright/test";

import { loginViaKeycloak } from "./helpers";

/// スマホのドロワ（サイドバー）の上でアカウントメニューを開いたとき、メニューの外を
/// タップしてもメニューだけが閉じ、ドロワは残る（#868）。指の下のリンクにも反応しない。

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

/// ドロワを開き、開き終えた位置を返す（メニュー表示中はドロワが aria-hidden になるため先に測る）。
async function openDrawer(phone: Page) {
  await phone.getByRole("button", { name: "メニューを開く", exact: true }).tap();
  const drawer = phone.getByRole("dialog", { name: "ナビゲーション" });
  await expect(drawer).toBeVisible();
  // スライドインの終了を待ってから測る。
  await expect
    .poll(async () => (await drawer.boundingBox())?.x)
    .toBeGreaterThanOrEqual(0);
  const box = await drawer.boundingBox();
  if (!box) throw new Error("ドロワが見えない");
  return { drawer, box };
}

async function openAccountMenu(phone: Page) {
  await phone
    .getByRole("dialog", { name: "ナビゲーション" })
    .getByRole("button", { name: "アカウントメニューを開く" })
    .tap();
  await expect(phone.getByRole("menu")).toBeVisible();
}

test.describe("スマホのドロワとアカウントメニュー", () => {
  test("メニューの外をタップするとメニューだけが閉じ、ドロワと現在の画面は残る", async ({
    browser,
    page,
  }) => {
    const phone = await phonePage(browser, page);
    await phone.goto("/");
    const { drawer, box } = await openDrawer(phone);
    const driveBox = await drawer.getByRole("link", { name: "ドライブ" }).boundingBox();
    if (!driveBox) throw new Error("ドライブのリンクが見えない");

    // ドロワ内のリンクの上をタップ: メニューは閉じるが、リンクには反応しない。
    await openAccountMenu(phone);
    await phone.touchscreen.tap(driveBox.x + driveBox.width / 2, driveBox.y + driveBox.height / 2);
    await expect(phone.getByRole("menu")).toHaveCount(0);
    await expect(drawer).toBeVisible();
    await expect(phone).toHaveURL(/\/$/);

    // ドロワの外（オーバーレイ）をタップ: メニューだけ閉じる。
    const outside = { x: box.x + box.width + 20, y: box.y + box.height / 3 };
    await openAccountMenu(phone);
    await phone.touchscreen.tap(outside.x, outside.y);
    await expect(phone.getByRole("menu")).toHaveCount(0);
    await expect(drawer).toBeVisible();

    // メニューが無ければ、オーバーレイのタップでドロワは閉じる。
    await phone.touchscreen.tap(outside.x, outside.y);
    await expect(drawer).toHaveCount(0);
  });

  test("メニューの項目はそのまま選べる", async ({ browser, page }) => {
    const phone = await phonePage(browser, page);
    await phone.goto("/");
    const { drawer } = await openDrawer(phone);
    await openAccountMenu(phone);
    await phone.getByRole("menuitem", { name: "設定" }).tap();
    await expect(phone).toHaveURL(/\/settings/);
    await expect(drawer).toHaveCount(0);
  });
});
