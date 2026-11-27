import { expect, test, type Page } from "@playwright/test";

import { cleanupAll, createFolderApi, loginViaKeycloak, trashFolderApi, uniqueName } from "./helpers";

/// ドライブの移動先のダイアログは、移すものがいまあるフォルダーから始まる（GAP-48）。以前はいつもドライブのトップから
/// 始まり、近くのフォルダーへ移すのにトップからたどり直す必要があった。
///
/// - パンくずの最後がいまあるフォルダーで、その中のフォルダーが並ぶ。いまある場所へは移せない（「ここへ移動」を押せず、
///   そう言う）。
/// - 中のフォルダーを開けば移せる。パンくずで上のフォルダーへ戻っても移せる。
/// - ドライブのトップに作業用のフォルダーを 1 つ作り、その中で操作する。最後にゴミ箱へ移す。

const SHOTS = process.env.E2E_SHOTS_DIR;

async function uploadText(page: Page, name: string) {
  await page.locator('input[type="file"][multiple]').setInputFiles({
    name,
    mimeType: "text/plain",
    buffer: Buffer.from(`移動のテスト ${name}\n`),
  });
  await expect(page.getByText(name, { exact: true })).toBeVisible({ timeout: 20_000 });
}

test("移動先のダイアログは、いまあるフォルダーから始まる（PC・スマホ）", async ({ page, browser }) => {
  test.setTimeout(180_000);
  await page.setViewportSize({ width: 1440, height: 900 });
  await loginViaKeycloak(page);
  const workName = uniqueName("e2e-move-start");
  const work = await createFolderApi(page, workName);
  const intoName = uniqueName("移動先");
  const otherName = uniqueName("ほかのフォルダー");
  try {
    const into = await createFolderApi(page, intoName, work);
    await createFolderApi(page, otherName, work);
    await page.goto(`/drive?folder=${work}`);
    const fileName = `${uniqueName("報告")}.txt`;
    await uploadText(page, fileName);

    // --- PC: 作業用のフォルダーから始まる ---
    await page.getByRole("button", { name: `「${fileName}」の操作` }).click();
    await page.getByRole("menuitem", { name: "移動", exact: true }).click();
    const dialog = page.getByRole("dialog");
    const crumbs = dialog.getByRole("navigation", { name: "移動先の階層" });
    await expect(crumbs).toContainText(workName);
    await expect(dialog.getByRole("button", { name: intoName, exact: true })).toBeVisible({ timeout: 15_000 });
    await expect(dialog.getByRole("button", { name: otherName, exact: true })).toBeVisible();
    // いまある場所へは移せない。
    const moveHere = dialog.getByRole("button", { name: "ここへ移動" });
    await expect(moveHere).toBeDisabled();
    await expect(dialog.getByTestId("move-here-note")).toHaveText("いまある場所です。移動先のフォルダーを開いてください。");
    if (SHOTS) await page.screenshot({ path: `${SHOTS}/drive-move-start-pc.png` });

    // 中のフォルダーを開けば移せる。
    await dialog.getByRole("button", { name: intoName, exact: true }).click();
    await expect(crumbs).toContainText(intoName);
    await expect(dialog.getByTestId("move-here-note")).toHaveCount(0);
    await expect(moveHere).toBeEnabled();
    if (SHOTS) await page.screenshot({ path: `${SHOTS}/drive-move-start-into-pc.png` });
    await moveHere.click();
    await expect(dialog).toBeHidden();
    await expect(page.getByText(fileName, { exact: true })).toHaveCount(0);

    // --- スマホ: 移した先（中のフォルダー）から始まり、パンくずで上へ戻して移せる ---
    const phoneCtx = await browser.newContext({
      storageState: await page.context().storageState(),
      viewport: { width: 390, height: 844 },
      hasTouch: true,
      isMobile: true,
      deviceScaleFactor: 3,
      ignoreHTTPSErrors: true,
    });
    try {
      const phone = await phoneCtx.newPage();
      await phone.goto(`/drive?folder=${into}`);
      await expect(phone.getByText(fileName, { exact: true })).toBeVisible({ timeout: 20_000 });
      await phone.getByRole("button", { name: `「${fileName}」の操作` }).click();
      await phone.getByRole("dialog").getByRole("button", { name: "移動", exact: true }).click();
      const sheet = phone.getByRole("dialog", { name: `「${fileName}」を移動` });
      const phoneCrumbs = sheet.getByRole("navigation", { name: "移動先の階層" });
      await expect(phoneCrumbs).toContainText(intoName, { timeout: 15_000 });
      await expect(sheet.getByRole("button", { name: "ここへ移動" })).toBeDisabled();
      await expect(sheet.getByTestId("move-here-note")).toBeVisible();
      expect(await phone.evaluate(() => document.documentElement.scrollWidth)).toBeLessThanOrEqual(390);
      if (SHOTS) await phone.screenshot({ path: `${SHOTS}/drive-move-start-phone.png` });
      // パンくずで作業用のフォルダーへ戻る（そこには中のフォルダーとほかのフォルダーが並ぶ）。
      await phoneCrumbs.getByRole("button", { name: workName, exact: true }).click();
      await expect(sheet.getByRole("button", { name: otherName, exact: true })).toBeVisible({ timeout: 15_000 });
      await sheet.getByRole("button", { name: "ここへ移動" }).click();
      await expect(sheet).toBeHidden();
      await expect(phone.getByText(fileName, { exact: true })).toHaveCount(0);
    } finally {
      await phoneCtx.close();
    }

    // 作業用のフォルダーに戻っている。
    await page.goto(`/drive?folder=${work}`);
    await expect(page.getByText(fileName, { exact: true })).toBeVisible({ timeout: 20_000 });
  } finally {
    await cleanupAll(() => trashFolderApi(page, work));
  }
});
