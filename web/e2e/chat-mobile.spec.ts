import { expect, test, type Browser, type Page } from "@playwright/test";

import { loginViaKeycloak } from "./helpers";

/// スマホのチャット（#733）: 2 段の入力欄（#768）・はみ出し・免責文・実行モードのカード。
/// 前提: chat.spec と同じ（LLM=stub は「回答: <質問>」を返す）。
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
    deviceScaleFactor: 3,
    ignoreHTTPSErrors: true,
  });
  return ctx.newPage();
}

test.describe("スマホのチャット", () => {
  test("入力欄は文字の段と操作の段の 2 段で、長い URL があっても会話が横にはみ出さない", async ({
    browser,
    page,
  }) => {
    const phone = await phonePage(browser, page);
    await phone.goto("/");

    // 2 段（文字の段 ＋ 「+」・モデル・送信の段・#768）。空のときは約 108px に収まる。
    const bar = phone.getByTestId("composer");
    await expect(bar).toBeVisible();
    const box = await bar.boundingBox();
    expect(box?.height ?? 0).toBeLessThanOrEqual(116);
    const textBox = await phone.getByLabel("メッセージを入力").boundingBox();
    const modelBox = await phone.getByTestId("model-button").boundingBox();
    expect((textBox?.y ?? 0) + (textBox?.height ?? 0)).toBeLessThanOrEqual((modelBox?.y ?? 0) + 1);

    // タッチ端末では Enter は改行（送信はボタン）。
    const input = phone.getByLabel("メッセージを入力");
    await input.fill("一行目");
    await input.press("Enter");
    await expect(input).toHaveValue("一行目\n");

    const url = `https://example.com/${"very-long-path-segment-".repeat(12)}${Date.now().toString(36)}`;
    await input.fill(`このページを要約して ${url}`);
    await phone.getByRole("button", { name: "送信" }).click();
    await phone.waitForURL(/\/c\/[0-9a-f-]+/i, { timeout: 20_000 });
    await expect(phone.getByText(/回答/).first()).toBeVisible({ timeout: 30_000 });

    // 画面も会話のスクロール領域も横にはみ出さない。
    const overflow = await phone.evaluate(() => {
      const scroller = document.querySelector<HTMLElement>('[data-testid="conversation-scroller"]');
      return {
        page: document.documentElement.scrollWidth - window.innerWidth,
        scroller: scroller ? scroller.scrollWidth - scroller.clientWidth : 0,
      };
    });
    expect(overflow.page).toBeLessThanOrEqual(0);
    expect(overflow.scroller).toBeLessThanOrEqual(0);

    // 免責文は最新の回答の下に 1 回だけ。
    await expect(phone.getByTestId("chat-disclaimer")).toHaveCount(1, { timeout: 30_000 });
    await phone.context().close();
  });

  test("実行モードのボタンからエージェントに切り替えると、プレースホルダで状態が分かる", async ({
    browser,
    page,
  }) => {
    const phone = await phonePage(browser, page);
    await phone.goto("/");

    // スマホではトグルを並べず、1 つのボタンにまとめる。
    await expect(phone.getByRole("switch", { name: "エージェントモード" })).toHaveCount(0);
    await phone.getByTestId("composer-mode-button").click();
    const card = phone.getByTestId("composer-mode-card");
    await expect(card).toBeVisible();
    await card.getByTestId("composer-mode-agent").click();
    await expect(card.getByTestId("composer-mode-agent")).toHaveAttribute("aria-checked", "true");
    await card.getByRole("button", { name: "完了" }).click();

    await expect(phone.getByLabel("メッセージを入力")).toHaveAttribute("placeholder", /エージェント/);
    // ボタンを押しても入力欄にフォーカスしない（ソフトキーボードを出さない）。
    await phone.getByTestId("composer-mode-button").click();
    await expect(phone.getByLabel("メッセージを入力")).not.toBeFocused();
    await phone.keyboard.press("Escape");

    // 「+」は下からのシート（添付 → 作成 → モデル → スキル）。
    await phone.getByRole("button", { name: "追加メニューを開く" }).click();
    const sheet = phone.getByTestId("composer-plus-sheet");
    await expect(sheet).toBeVisible();
    await expect(sheet.getByRole("button", { name: /ファイル・写真/ })).toBeVisible();
    await expect(sheet.getByTestId("composer-create-note")).toBeVisible();
    await phone.context().close();
  });
});
