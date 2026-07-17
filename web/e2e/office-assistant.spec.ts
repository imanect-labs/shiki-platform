import { expect, test } from "@playwright/test";

import { loginViaKeycloak } from "./helpers";

/// Office 文書のアシスタントパネル＋選択→AI（Task 11.10・Office 版）。
/// 実 Collabora が必要なため OFFICE_E2E=1 でのみ実行する。
test.skip(process.env.OFFICE_E2E !== "1", "OFFICE_E2E=1 のときのみ実行（Collabora が必要）");
test.use({ locale: "ja-JP" });

async function openNewDocument(page: import("@playwright/test").Page) {
  await page.goto("/drive");
  for (let i = 0; i < 3 && !/\/office\//.test(page.url()); i++) {
    await page.getByRole("button", { name: "新規作成" }).click();
    await page.getByTestId("new-document").click();
    await page.waitForURL(/\/office\//, { timeout: 30_000 }).catch(() => {});
  }
  expect(page.url()).toMatch(/\/office\//);
  await expect(page.getByText("エディタを起動しています…")).toBeHidden({ timeout: 60_000 });
  // Collabora 本体の描画が落ち着くまで待つ。
  await page.waitForTimeout(8000);
}

test("アシスタントパネルを開いて会話を準備できる", async ({ page }) => {
  await loginViaKeycloak(page);
  await openNewDocument(page);

  await page.getByTestId("office-chat-toggle").click();
  await expect(page.getByTestId("office-chat-panel")).toBeVisible();
  // 会話が自動作成され、入力欄が出る（DocChatPanel の準備完了）。
  await expect(
    page.getByTestId("office-chat-panel").getByPlaceholder(/尋ねて|メッセージ|指示/),
  ).toBeVisible({ timeout: 20_000 });
});

test("文書内の選択が AI への依頼チップになる（Collabora Action_Copy 経由）", async ({ page }) => {
  await loginViaKeycloak(page);
  await openNewDocument(page);

  // 本文に文字を打って全選択する（Collabora の編集領域は iframe 内の要素をクリックして
  // フォーカスする必要がある。iframe 要素の外側クリックでは入力が届かない）。
  const inner = page.frameLocator('[data-testid="office-frame"]');
  // canvas はカーソル点滅で "stable" 判定にならないため force クリックする。
  await inner
    .locator("#main-document-content, #document-container")
    .first()
    .click({ force: true, position: { x: 60, y: 40 } });
  await page.keyboard.type("選択テスト本文アルファベータ", { delay: 40 });
  await page.waitForTimeout(1200);
  await page.keyboard.press("Control+a");
  await page.waitForTimeout(1200);

  // 「AI に依頼」→ Collabora から選択テキストを取得 → チップ＋パネル表示。
  await page.getByTestId("office-ask-ai").click();
  await expect(page.getByTestId("selection-chip")).toBeVisible({ timeout: 15_000 });
  await expect(page.getByTestId("selection-chip")).toContainText("文書の選択範囲");

  // 送信 → ユーザーメッセージに選択チップが残る（サーバ受理の裏取り）。
  const input = page.getByTestId("office-chat-panel").getByPlaceholder(/尋ねて|メッセージ|指示/);
  await input.fill("この部分を丁寧語に直して");
  await input.press("Enter");
  await expect(page.getByTestId("message-selection-chip")).toBeVisible({ timeout: 20_000 });
});
