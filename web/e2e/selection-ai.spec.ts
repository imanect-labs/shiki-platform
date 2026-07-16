import { expect, test } from "@playwright/test";

import { loginViaKeycloak } from "./helpers";

/// 選択→AI 指示（Task 11.10）: ノートで選択→「AI に依頼」→チップ→送信で
/// ユーザーメッセージに選択コンテキストが付くことを一気通貫で検証する。

test("ノートの選択が AI への依頼チップになり送信される", async ({ page }) => {
  await loginViaKeycloak(page);
  await page.goto("/drive");
  await page.getByRole("button", { name: "新規作成" }).click();
  await page.getByTestId("new-note").click();
  await page.waitForURL(/\/notes\//, { timeout: 20_000 });

  // 本文を入力して全選択 → バブルメニューの「AI に依頼」。
  const editor = page.locator(".tiptap").first();
  await editor.click();
  await page.keyboard.type("第一四半期の売上は好調に推移した。");
  await page.keyboard.press("ControlOrMeta+a");
  await page.getByTestId("note-ask-ai").click();

  // アシスタントパネルが開き、Composer に選択チップが出る。
  await expect(page.getByTestId("selection-chip")).toBeVisible({ timeout: 10_000 });
  await expect(page.getByTestId("selection-chip")).toContainText("ノートの選択範囲");

  // 指示を送信 → ユーザーメッセージに選択チップが表示される（サーバ受理の裏取り）。
  const input = page.getByPlaceholder(/尋ねて|指示|メッセージ/).first();
  await input.fill("この部分を要約して");
  await input.press("Enter");
  await expect(page.getByTestId("message-selection-chip")).toBeVisible({ timeout: 20_000 });
  // 送信後はチップが消費されている。
  await expect(page.getByTestId("selection-chip")).toHaveCount(0);
});
