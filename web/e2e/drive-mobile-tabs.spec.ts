import { expect, test, type Browser, type Page } from "@playwright/test";

import { loginViaKeycloak, uniqueName } from "./helpers";

/// スマホのドライブ: 上端のタブ（ホーム／最近使った／お気に入り／共有済み／ゴミ箱）で区画を
/// 切り替えられ、フォルダの中では出ない。PC はサイドバーが担うので出さない。
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

test("スマホのドライブは上端のタブで区画を切り替え、フォルダの中ではタブを出さない", async ({
  browser,
  page,
}) => {
  const phone = await phonePage(browser, page);
  await phone.goto("/drive");

  const tabs = phone.getByRole("navigation", { name: "ドライブ" });
  await expect(tabs).toBeVisible();
  await expect(phone.getByTestId("drive-tab-home")).toHaveAttribute("aria-current", "page");

  // 区画を切り替えると URL と表示中の印が追従する。
  await phone.getByTestId("drive-tab-trash").click();
  await expect(phone).toHaveURL(/\/drive\/trash$/);
  await expect(phone.getByTestId("drive-tab-trash")).toHaveAttribute("aria-current", "page");
  await expect(phone.getByTestId("drive-tab-home")).not.toHaveAttribute("aria-current", "page");

  await phone.getByTestId("drive-tab-favorites").click();
  await expect(phone).toHaveURL(/\/drive\/favorites$/);
  await expect(phone.getByRole("heading", { name: "お気に入り" })).toBeVisible();

  // フォルダの中は戻る＋パンくずが現在地を担うのでタブを出さない。
  const folder = uniqueName("タブ");
  await page.goto("/drive");
  await page.getByRole("button", { name: "新規フォルダ" }).click();
  const dialog = page.getByRole("dialog");
  await dialog.getByRole("textbox").fill(folder);
  await dialog.getByRole("button", { name: "作成" }).click();
  await expect(page.getByText(folder, { exact: true })).toBeVisible();

  await phone.getByTestId("drive-tab-home").click();
  await phone.getByText(folder, { exact: true }).click();
  await expect(phone).toHaveURL(/folder=/);
  await expect(tabs).toBeHidden();

  // PC はサイドバーに子メニューがあるのでタブを出さない。
  await expect(page.getByRole("navigation", { name: "ドライブ" })).toBeHidden();

  await phone.context().close();
});
