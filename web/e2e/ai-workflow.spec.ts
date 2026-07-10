import { expect, test } from "@playwright/test";

import { loginViaKeycloak, uniqueName } from "./helpers";

/// AI ワークフロー編集の e2e（Task 10.13 DoD）:
/// 自然言語（stub の `emitwf:` 決定的駆動）→ 保存パイプライン検証 → workflow_ref カード →
/// 「エディタで開く」→ dnd エディタで描画・編集可能。不正 IR はカード化されない。

test("AI 編集: emit_workflow → カード → エディタで開く / 不正 IR は拒否", async ({ page }) => {
  test.setTimeout(180_000);
  await loginViaKeycloak(page);

  // 正常系: stub が emit_workflow を検証を通る IR で呼ぶ。
  const name = uniqueName("ai-flow");
  await page.goto("/");
  const input = page.getByLabel("メッセージを入力");
  await input.click();
  await input.fill(`emitwf:ok ${name}`);
  await page.getByRole("button", { name: "送信" }).click();
  await page.waitForURL(/\/c\/[0-9a-f-]+/i, { timeout: 20_000 });

  // 検証済み参照カードがストリーミングで届く。
  await expect(
    page.getByText("ワークフローを保存しました。エディタで確認・編集できます。").first(),
  ).toBeVisible({ timeout: 30_000 });
  await expect(page.getByText("スタブフロー").first()).toBeVisible();

  // 「エディタで開く」→ dnd エディタが保存済み IR を描画（レイアウトは自動導出）。
  await page.getByRole("link", { name: "エディタで開く" }).first().click();
  await page.waitForURL(/\/workflows\//, { timeout: 15_000 });
  const canvas = page.locator(".react-flow");
  await expect(canvas.getByText("スクリプト").first()).toBeVisible({ timeout: 15_000 });
  // 編集可能（ノードクリックで設定パネルが開く）。
  await canvas.getByText("スクリプト").first().click();
  await expect(page.getByRole("button", { name: "このブロックを削除" })).toBeVisible();

  // 異常系: 語彙外ノードを含む不正 IR は検証拒否され、カード化されずテキストで返る。
  await page.goto("/");
  const input2 = page.getByLabel("メッセージを入力");
  await input2.click();
  await input2.fill(`emitwf:bad ${uniqueName("ai-bad")}`);
  await page.getByRole("button", { name: "送信" }).click();
  await page.waitForURL(/\/c\/[0-9a-f-]+/i, { timeout: 20_000 });
  await expect(page.getByRole("main").getByText(/回答/).first()).toBeVisible({
    timeout: 30_000,
  });
  await expect(
    page.getByText("ワークフローを保存しました。エディタで確認・編集できます。"),
  ).toHaveCount(0);
});
