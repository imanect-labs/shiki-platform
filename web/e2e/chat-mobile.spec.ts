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
  test("入力欄は既定のモデルなら 1 行で、長い URL があっても会話が横にはみ出さない", async ({
    browser,
    page,
  }) => {
    const phone = await phonePage(browser, page);
    await phone.goto("/");

    // モデルを選んでいない（Auto）ときは 1 行の丸いバー（約 54px）で、モデルのボタンは出さない。
    const bar = phone.getByTestId("composer");
    await expect(bar).toBeVisible();
    expect((await bar.boundingBox())?.height ?? 0).toBeLessThanOrEqual(58);
    await expect(phone.getByTestId("model-button")).toBeHidden();

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

    // 「+」は下からのシート（添付 → 作成 → モデル → ツール → スキル）。
    await phone.getByRole("button", { name: "追加メニューを開く" }).click();
    const sheet = phone.getByTestId("composer-plus-sheet");
    await expect(sheet).toBeVisible();
    await expect(sheet.getByRole("button", { name: /ファイル・写真/ })).toBeVisible();
    await expect(sheet.getByTestId("composer-create-note")).toBeVisible();
    await expect(sheet.getByTestId("composer-open-model")).toBeVisible();
    await phone.context().close();
  });

  test("「+」のシートの「ファイル・写真」で選ばずに戻っても、シートは開いたまま", async ({
    browser,
    page,
  }) => {
    const phone = await phonePage(browser, page);
    await phone.goto("/");

    await phone.getByRole("button", { name: "追加メニューを開く" }).click();
    const sheet = phone.getByTestId("composer-plus-sheet");
    await expect(sheet).toBeVisible();
    const [chooser] = await Promise.all([
      phone.waitForEvent("filechooser"),
      sheet.getByRole("button", { name: /ファイル・写真/ }).click(),
    ]);
    // ファイルの無い change（選ばずに戻った）でも閉じない。選び直せるようにシートを残す。
    await chooser.setFiles([]);
    await phone.waitForTimeout(500);
    await expect(sheet).toHaveAttribute("data-state", "open");
    await phone.context().close();
  });

  test("「+」のシートの「ファイル・写真」は、ファイルを選ぶまでシートを閉じない", async ({
    browser,
    page,
  }) => {
    const phone = await phonePage(browser, page);
    await phone.goto("/");

    await phone.getByRole("button", { name: "追加メニューを開く" }).click();
    const sheet = phone.getByTestId("composer-plus-sheet");
    await expect(sheet).toBeVisible();
    const [chooser] = await Promise.all([
      phone.waitForEvent("filechooser"),
      sheet.getByRole("button", { name: /ファイル・写真/ }).click(),
    ]);
    // ファイルを選んでいる間はシートが残る（閉じるアニメーション中も見えるので data-state で見る）。
    // 少し待つのは、押した後に遅れて閉じる回帰も拾うため。
    await phone.waitForTimeout(500);
    await expect(sheet).toHaveAttribute("data-state", "open");

    const name = `mobile-attach-${Date.now().toString(36)}.txt`;
    await chooser.setFiles({ name, mimeType: "text/plain", buffer: Buffer.from("添付のテスト") });
    // 選んだらシートを閉じ、入力欄の上に添付が付く（アップロード中のチップと分けるため「添付を外す」を持つものを見る）。
    await expect(sheet).toBeHidden();
    const chip = phone
      .locator("span", { has: phone.getByRole("button", { name: "添付を外す" }) })
      .filter({ hasText: name });
    await expect(chip).toBeVisible({ timeout: 30_000 });
    await phone.context().close();
  });

  test("画像を添付すると、入力欄と送った発話に小さい四角のサムネイルが出る", async ({ browser, page }) => {
    const phone = await phonePage(browser, page);
    await phone.goto("/");

    // 1×1 の PNG（名前は一意にする: 同じ名前はドライブで重複になりアップロードが失敗する）。
    const png = Buffer.from(
      "iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAYAAAAfFcSJAAAADUlEQVR42mP8z8BQDwAEhQGAhKmMIQAAAABJRU5ErkJggg==",
      "base64",
    );
    const name = `mobile-photo-${Date.now().toString(36)}.png`;
    await phone.getByRole("button", { name: "追加メニューを開く" }).click();
    const [chooser] = await Promise.all([
      phone.waitForEvent("filechooser"),
      phone.getByTestId("composer-plus-sheet").getByRole("button", { name: /ファイル・写真/ }).click(),
    ]);
    await chooser.setFiles({ name, mimeType: "image/png", buffer: png });

    // 入力欄の上: 画像のサムネイル（読み込めている）と、アップロード完了後にだけ出る「外す」。
    const remove = phone.getByRole("button", { name: `${name} の添付を外す` });
    await expect(remove).toBeVisible({ timeout: 30_000 });
    const thumb = phone.getByRole("img", { name });
    await expect.poll(() => thumb.evaluate((el: HTMLImageElement) => el.naturalWidth)).toBeGreaterThan(0);

    // 送った発話にも出て、押すとファイルのビューアへ行けるリンクになっている。
    await phone.getByLabel("メッセージを入力").fill("この画像は？");
    await phone.getByRole("button", { name: "送信" }).click();
    await phone.waitForURL(/\/c\/[0-9a-f-]+/i, { timeout: 20_000 });
    // 入力欄の添付は送ったら空になる。
    await expect(remove).toHaveCount(0);
    const link = phone.getByRole("link", { name: `${name} を開く` });
    await expect(link).toHaveAttribute("href", /^\/file\/[0-9a-f-]+$/i);
    await expect.poll(
      () => link.getByRole("img").evaluate((el: HTMLImageElement) => el.naturalWidth),
      { timeout: 30_000 },
    ).toBeGreaterThan(0);
    await phone.context().close();
  });

  test("「+」のシートからモデルを選ぶと、入力欄にモデルが出て 2 段になる", async ({ browser, page }) => {
    const phone = await phonePage(browser, page);
    await phone.goto("/");
    const bar = phone.getByTestId("composer");
    await expect(phone.getByTestId("model-button")).toBeHidden();

    await phone.getByRole("button", { name: "追加メニューを開く" }).click();
    await phone.getByTestId("composer-open-model").click();
    const picker = phone.getByTestId("model-picker");
    await expect(picker).toBeVisible();
    // プロバイダの見出しの中の行（モデル）を選ぶ（モデルの id が default でも Auto の行と取り違えない）。
    await picker.getByRole("group").getByRole("option").first().click();
    await phone.keyboard.press("Escape");
    await expect(picker).toBeHidden();

    // 選んだモデルは送信の左に出て、入力欄は文字の段と操作の段の 2 段になる。
    const model = phone.getByTestId("model-button");
    await expect(model).toBeVisible();
    const textBox = await phone.getByLabel("メッセージを入力").boundingBox();
    const modelBox = await model.boundingBox();
    expect((textBox?.y ?? 0) + (textBox?.height ?? 0)).toBeLessThanOrEqual((modelBox?.y ?? 0) + 1);
    expect((await bar.boundingBox())?.height ?? 0).toBeLessThanOrEqual(116);

    // Auto に戻すと 1 行に戻る。
    await model.click();
    await phone.getByTestId("model-picker").getByRole("option", { name: /Auto|組織の既定/ }).click();
    await expect(model).toBeHidden();
    expect((await bar.boundingBox())?.height ?? 0).toBeLessThanOrEqual(58);
    await phone.context().close();
  });
});
