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

  await page.getByTestId("office-ask-ai").click();
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

test("ファイルレベル AI 編集→承認で提案バージョンが作成される（編集セッション中・#328）", async ({
  page,
}) => {
  await loginViaKeycloak(page);
  await openNewDocument(page);
  // URL から fileId を取る（officeedit: プレフィックスで office.edit を明示駆動する）。
  const fileId = page.url().match(/\/office\/([^/?#]+)/)?.[1];
  expect(fileId).toBeTruthy();

  // 文書を開いている＝Collabora が WOPI ロックを保持している状態。ここで**ファイルレベル**の
  // AI 編集（office.edit）を通すと、上書きせず提案バージョンへ迂回する（PIT-44）ことを検証する。
  await page.getByTestId("office-ask-ai").click();
  const input = page.getByTestId("office-chat-panel").getByPlaceholder(/尋ねて|メッセージ|指示/);
  await input.fill(`officeedit:${fileId}`);
  await input.press("Enter");

  // 破壊系（ファイル内容を書き換える）ため承認カードが出る。承認して実行させる。
  const approve = page.getByRole("button", { name: "承認して続行" });
  await expect(approve).toBeVisible({ timeout: 25_000 });
  await approve.click();

  // 編集セッション中（ロック）なので、上書きではなく提案バージョンとして保存される。
  await expect(page.getByTestId("office-chat-panel").getByText(/提案バージョン/)).toBeVisible({
    timeout: 30_000,
  });
});

test("選択→AI→承認で開いているセッションへライブ反映される（Action_Paste・#328）", async ({
  page,
}) => {
  await loginViaKeycloak(page);
  await openNewDocument(page);

  // 本文を打って全選択 → 選択→AI（office_selection）。開いているセッションなので、承認後は
  // office.live_edit が Collabora の Action_Paste で現在の選択を置換し、その場でライブ反映される
  // （ファイルレベルの版競合を回避）。
  const inner = page.frameLocator('[data-testid="office-frame"]');
  await inner
    .locator("#main-document-content, #document-container")
    .first()
    .click({ force: true, position: { x: 60, y: 40 } });
  await page.keyboard.type("差し替え対象の本文", { delay: 40 });
  await page.waitForTimeout(1200);
  await page.keyboard.press("Control+a");
  await page.waitForTimeout(1200);

  await page.getByTestId("office-ask-ai").click();
  await expect(page.getByTestId("selection-chip")).toBeVisible({ timeout: 15_000 });

  // 編集キーワードを含む依頼 → stub が office.live_edit を呼ぶ。
  const input = page.getByTestId("office-chat-panel").getByPlaceholder(/尋ねて|メッセージ|指示/);
  await input.fill("この選択範囲を、丁寧な文章に書き直して");
  await input.press("Enter");

  const approve = page.getByRole("button", { name: "承認して続行" });
  await expect(approve).toBeVisible({ timeout: 25_000 });
  await approve.click();

  // 承認後、office.live_edit → SSE → Action_Paste/.uno:InsertText でセッション内の選択が置換される。
  // Collabora は canvas 描画のため DOM テキストでは検証できない。置換後に文書を全選択すると、
  // アシスタントパネルの選択ポーリング（Action_Copy）が新しい本文を拾ってチップへ反映するので、
  // そのチップ本文に AI の差し替え内容が含まれることでセッションへ反映されたことを裏取りする。
  // （前提: Collabora の welcome オーバーレイが無効化された構成。有効だと文書が覆われ注入不可。）
  await page.waitForTimeout(4000);
  await page.keyboard.press("Escape");
  await inner
    .locator("#main-document-content, #document-container")
    .first()
    .click({ force: true, position: { x: 300, y: 200 } });
  await page.waitForTimeout(500);
  await page.keyboard.press("Control+a");
  await expect(page.getByTestId("selection-chip")).toContainText("AI が置き換えた本文です。", {
    timeout: 20_000,
  });
});
