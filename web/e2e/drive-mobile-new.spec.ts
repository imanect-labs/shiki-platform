import { expect, test, type Browser, type Page } from "@playwright/test";

import { loginViaKeycloak, uniqueName } from "./helpers";

/// スマホのドライブの「＋ 新規」: アップロードはファイルを選ぶまでシートを閉じず、
/// 新規フォルダの名前の入力欄はソフトキーボードに隠れない。
///
/// ログインは PC の画面で済ませ、そのセッションをタッチ端末の文脈へ持ち込む
/// （ログイン完了の判定はシェルのアカウントメニューを見るため、スマホ幅では使えない）。

const PHONE = { width: 390, height: 844 };

async function phonePage(browser: Browser, page: Page): Promise<Page> {
  await loginViaKeycloak(page);
  const ctx = await browser.newContext({
    storageState: await page.context().storageState(),
    viewport: PHONE,
    hasTouch: true,
    isMobile: true,
    ignoreHTTPSErrors: true,
  });
  return ctx.newPage();
}

test("「＋ 新規」のアップロードは、ファイルを選ぶまでシートを閉じない", async ({ browser, page }) => {
  const phone = await phonePage(browser, page);
  await phone.goto("/drive");

  await phone.getByTestId("drive-fab").click();
  const sheet = phone.getByTestId("drive-new-sheet");
  await expect(sheet).toBeVisible();

  // 選ばずに戻った（ファイルの無い change）ときはシートを残し、選び直せるようにする。
  const [cancelled] = await Promise.all([
    phone.waitForEvent("filechooser"),
    sheet.getByRole("button", { name: "アップロード" }).click(),
  ]);
  await cancelled.setFiles([]);
  // 少し待つのは、押した後に遅れて閉じる回帰も拾うため（閉じるアニメーション中も見えるので data-state で見る）。
  await phone.waitForTimeout(500);
  await expect(sheet).toHaveAttribute("data-state", "open");

  const [chooser] = await Promise.all([
    phone.waitForEvent("filechooser"),
    sheet.getByRole("button", { name: "アップロード" }).click(),
  ]);
  await phone.waitForTimeout(500);
  await expect(sheet).toHaveAttribute("data-state", "open");

  // 選んだらシートを閉じ、一覧にファイルが出る。
  const name = `${uniqueName("mobile-upload")}.txt`;
  await chooser.setFiles({ name, mimeType: "text/plain", buffer: Buffer.from("アップロードのテスト") });
  await expect(sheet).toBeHidden();
  await expect(phone.getByText(name, { exact: true })).toBeVisible({ timeout: 30_000 });
  await phone.context().close();
});

test("「＋ 新規」の新規フォルダは、ソフトキーボードの上に名前の入力欄を出す", async ({ browser, page }) => {
  const phone = await phonePage(browser, page);
  // iOS のキーボードを再現する: layout viewport は縮まず、visualViewport だけが縮む。
  await phone.addInitScript(() => {
    let keyboard = 0;
    const vv = new EventTarget();
    const getters: Record<string, () => number> = {
      height: () => window.innerHeight - keyboard,
      width: () => window.innerWidth,
      offsetTop: () => 0,
      offsetLeft: () => 0,
      pageTop: () => window.scrollY,
      pageLeft: () => window.scrollX,
      scale: () => 1,
    };
    for (const [key, get] of Object.entries(getters)) {
      Object.defineProperty(vv, key, { get });
    }
    Object.defineProperty(window, "visualViewport", { configurable: true, get: () => vv });
    (window as unknown as { setKeyboard: (px: number) => void }).setKeyboard = (px) => {
      keyboard = px;
      vv.dispatchEvent(new Event("resize"));
    };
  });
  await phone.goto("/drive");

  await phone.getByTestId("drive-fab").click();
  await phone.getByTestId("drive-new-sheet").getByRole("button", { name: "新規フォルダ" }).click();
  const dialog = phone.getByRole("dialog", { name: "新規フォルダ" });
  const input = dialog.getByRole("textbox");
  await expect(input).toBeVisible();

  const KEYBOARD = 320;
  await phone.evaluate((px) => (window as unknown as { setKeyboard: (px: number) => void }).setKeyboard(px), KEYBOARD);
  const top = PHONE.height - KEYBOARD;
  // シートの下端がキーボードの上端に揃い、入力欄と「作成」がキーボードより上にある。
  await expect
    .poll(async () => {
      const box = await dialog.boundingBox();
      return box ? Math.round(box.y + box.height) : null;
    })
    .toBe(top);
  for (const el of [input, dialog.getByRole("button", { name: "作成" })]) {
    const box = await el.boundingBox();
    expect(box).not.toBeNull();
    expect(box!.y).toBeGreaterThanOrEqual(0);
    expect(box!.y + box!.height).toBeLessThanOrEqual(top);
  }

  // キーボードを閉じたら画面下端へ戻る。
  await phone.evaluate(() => (window as unknown as { setKeyboard: (px: number) => void }).setKeyboard(0));
  await expect
    .poll(async () => {
      const box = await dialog.boundingBox();
      return box ? Math.round(box.y + box.height) : null;
    })
    .toBe(PHONE.height);

  const folder = uniqueName("キーボード");
  await input.fill(folder);
  await dialog.getByRole("button", { name: "作成" }).click();
  await expect(phone.getByText(folder, { exact: true })).toBeVisible();
  await phone.context().close();
});
