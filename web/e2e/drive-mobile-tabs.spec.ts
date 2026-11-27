import { expect, test, type Browser, type Page } from "@playwright/test";

import { cleanupQueue, loginViaKeycloak, trackCreatedResources, uniqueName } from "./helpers";

/// 画面から作ったフォルダーを、テストの後にゴミ箱へ移す（ドライブのトップに残さない・失敗したテストでも走る・
/// BUG-125・BUG-126）。
const cleanup = cleanupQueue();
test.beforeEach(({ page }) => trackCreatedResources(page, cleanup));
test.afterEach(() => cleanup.run());

/// スマホのドライブ: 上端のタブ（ホーム／最近使った／お気に入り／共有済み／ゴミ箱）で区画を
/// 切り替えられ、フォルダの中では出ない。PC も同じタブで切り替え、サイドバーに子メニューは無い。
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
  const created = page.waitForResponse(
    (r) => r.request().method() === "POST" && new URL(r.url()).pathname === "/api/folders",
  );
  await dialog.getByRole("button", { name: "作成" }).click();
  const folderId = ((await (await created).json()) as { id: string }).id;
  await expect(page.getByText(folder, { exact: true })).toBeVisible();

  // スマホでフォルダを開く（ドライブのトップに残ったほかのフォルダの数に左右されないよう、作ったフォルダへ直に移る・
  // BUG-125）。
  await phone.getByTestId("drive-tab-home").click();
  await phone.goto(`/drive?folder=${folderId}`);
  await expect(phone.getByRole("heading", { name: folder })).toBeVisible({ timeout: 15_000 });
  await expect(phone).toHaveURL(/folder=/);
  await expect(tabs).toBeHidden();

  // PC も同じタブを出し（フォルダの外）、サイドバーにはドライブの子メニュー（開閉）を持たない。
  await page.goto("/drive");
  await expect(page.getByRole("navigation", { name: "ドライブ" })).toBeVisible();
  await expect(page.getByRole("button", { name: /ドライブを(開く|閉じる)/ })).toHaveCount(0);
  await page.getByTestId("drive-tab-shared").click();
  await expect(page).toHaveURL(/\/drive\/shared$/);
  await expect(page.getByTestId("drive-tab-shared")).toHaveAttribute("aria-current", "page");

  await phone.context().close();
});
