import { test, type Page } from "@playwright/test";

import { loginViaKeycloak } from "./helpers";

/// メッセージ画面（Phase 14 Stage 4・UI モック）の視覚確認専用キャプチャ。
/// SHOTS_DIR を設定したときだけ実行する（CI ではスキップ）。
/// 例: SHOTS_DIR=/tmp/shots pnpm exec playwright test messages-visual --project=chromium
const SHOTS = process.env.SHOTS_DIR;

test.use({ viewport: { width: 1440, height: 900 }, deviceScaleFactor: 2 });

/// next-themes は localStorage("theme") を hydration 時に読むので、
/// 値を書いてから遷移し直す（クラスを手で付けるだけでは再描画で戻される）。
async function setTheme(page: Page, theme: "light" | "dark") {
  await page.evaluate((t) => window.localStorage.setItem("theme", t), theme);
  await page.goto("/messages");
  await page.waitForTimeout(400);
}

test("メッセージ画面のスクリーンショットを撮る", async ({ page }) => {
  test.skip(!SHOTS, "SHOTS_DIR 未設定（視覚確認専用）");

  await loginViaKeycloak(page);

  for (const theme of ["light", "dark"] as const) {
    await page.goto("/messages");
    await setTheme(page, theme);
    await page.getByRole("heading", { name: "総務-予算相談" }).waitFor();
    await page.waitForTimeout(600);
    await page.screenshot({ path: `${SHOTS}/msg-01-channel-${theme}.png` });

    // スレッド（返信 2 件）を開く。
    await page.getByRole("button", { name: /返信 2 件/ }).first().click();
    await page.waitForTimeout(500);
    await page.screenshot({ path: `${SHOTS}/msg-02-thread-${theme}.png` });
    await page.getByRole("button", { name: "スレッドを閉じる" }).click();

    // 権限の無い利用者に切り替え → 同じ発言が「参照できない添付」になる。
    await page.getByRole("button", { name: "表示中の利用者を切り替える" }).click();
    await page.getByRole("menuitem", { name: /鈴木 一郎/ }).click();
    await page.waitForTimeout(500);
    await page.screenshot({ path: `${SHOTS}/msg-03-no-permission-${theme}.png` });

    // 検索（鈴木は非公開チャンネルの発言が出ない）。
    await page.getByRole("button", { name: "発言を検索" }).click();
    await page.waitForTimeout(500);
    await page.screenshot({ path: `${SHOTS}/msg-04-search-suzuki-${theme}.png` });
    await page.getByRole("button", { name: "閉じる" }).click();

    // 田中に戻して検索（非公開チャンネルの発言も出る）。
    await page.getByRole("button", { name: "表示中の利用者を切り替える" }).click();
    await page.getByRole("menuitem", { name: /田中 誠/ }).click();
    await page.getByRole("button", { name: "発言を検索" }).click();
    await page.waitForTimeout(500);
    await page.screenshot({ path: `${SHOTS}/msg-05-search-tanaka-${theme}.png` });
    await page.getByRole("button", { name: "閉じる" }).click();

    // シキに聞く（スレッドに回答が積まれる）。
    const budgetRow = page
      .locator("div.group\\/msg")
      .filter({ hasText: "お待たせしました。2027年度の予算案です。" })
      .first();
    await budgetRow.hover();
    await budgetRow.getByRole("button", { name: "シキに聞く" }).click();
    await page.waitForTimeout(6500);
    await page.screenshot({ path: `${SHOTS}/msg-06-ask-ai-${theme}.png` });
    await page.getByRole("button", { name: "スレッドを閉じる" }).click();

    // メンション補完。
    const composer = page.getByPlaceholder("#総務-予算相談 へメッセージを送る");
    await composer.click();
    await composer.type("@佐");
    await page.waitForTimeout(400);
    await page.screenshot({ path: `${SHOTS}/msg-07-mention-${theme}.png` });
    await page.keyboard.press("Escape");
    await composer.fill("");

    // ドライブからの共有（添付候補）。
    await page.getByRole("button", { name: "ドライブの文書を共有" }).click();
    await page.waitForTimeout(300);
    await page.screenshot({ path: `${SHOTS}/msg-08-attach-${theme}.png` });
    await page.getByRole("button", { name: "ドライブの文書を共有" }).click();

    // 未読のあるチャンネル（未読ライン）。
    await page.getByRole("button", { name: "全社お知らせ" }).click();
    await page.waitForTimeout(400);
    await page.screenshot({ path: `${SHOTS}/msg-09-unread-${theme}.png` });

    // DM。
    await page.getByRole("button", { name: /^佐藤 花子 未読/ }).click();
    await page.waitForTimeout(400);
    await page.screenshot({ path: `${SHOTS}/msg-10-dm-${theme}.png` });

    // DM で送信 → 相手の入力中 → 返信が届く（配信の演出）。
    const dm = page.getByPlaceholder("佐藤 花子 へメッセージを送る");
    await dm.click();
    await dm.type("15時で大丈夫です。会議室は 3F を押さえておきます。");
    await page.keyboard.press("Enter");
    await page.waitForTimeout(1800);
    await page.screenshot({ path: `${SHOTS}/msg-12-typing-${theme}.png` });
    await page.waitForTimeout(2600);
    await page.screenshot({ path: `${SHOTS}/msg-13-replied-${theme}.png` });

    // チャンネル作成ダイアログ。
    await page.getByRole("button", { name: "チャンネルを作成" }).click();
    await page.waitForTimeout(400);
    await page.screenshot({ path: `${SHOTS}/msg-11-create-${theme}.png` });
    await page.keyboard.press("Escape");
  }
});
