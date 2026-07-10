import { expect, test } from "@playwright/test";

import { loginViaKeycloak } from "./helpers";

/// dnd ワークフローエディタの e2e（Task 10.12 DoD）:
/// 新規作成 → ブロック追加（ヘッダ導線＋尻尾プラス）→ 検証エラーが該当ノードにバッジ表示
/// → 修正（削除）→ 保存 → 実行 → 実行履歴で succeeded まで。
/// 前提: compose の shiki-server が SHIKI__WORKFLOW__ENABLED=true・LLM は stub。

test("エディタ: 作成→追加→エラーバッジ→修正→保存→実行→履歴成功", async ({ page }) => {
  test.setTimeout(180_000);
  await loginViaKeycloak(page);

  // 一覧から新規作成 → エディタへ。
  await page.goto("/workflows");
  await page.getByRole("button", { name: "新しいワークフロー" }).click();
  await page.waitForURL(/\/workflows\/[0-9a-f-]+$/i, { timeout: 20_000 });

  // 最初のブロック（スクリプト）をヘッダ導線から追加。
  await page.getByRole("button", { name: "最初のブロック" }).click();
  await page.getByRole("button", { name: /^スクリプト/ }).click();
  const canvas = page.locator(".react-flow");
  await expect(canvas.getByText("スクリプト").first()).toBeVisible();

  // 尻尾のプラスボタンから「ファイルを保存」を追加（主導線）。
  await canvas.getByLabel("次のブロックを追加").first().click();
  await page.getByRole("button", { name: /^ファイルを保存/ }).click();
  await expect(canvas.getByText("ファイルを保存").first()).toBeVisible();

  // ライブ検証（600ms debounce）が declared_scopes 不足を検出し、該当ノードにバッジが出る。
  await expect(canvas.locator('[aria-label^="検証エラー"]').first()).toBeVisible({
    timeout: 10_000,
  });

  // 修正: 問題のブロックを設定パネルから削除 → バッジが消える。
  await canvas.getByText("ファイルを保存").first().click();
  await page.getByRole("button", { name: "このブロックを削除" }).click();
  await expect(canvas.locator('[aria-label^="検証エラー"]')).toHaveCount(0, {
    timeout: 10_000,
  });

  // 保存 → 実行 → 実行履歴の deep-link へ遷移し、ライブ更新で成功する。
  await page.getByRole("button", { name: "保存", exact: true }).click();
  // toast は aria-live 領域にも複製されるため first() で strict 違反を避ける。
  await expect(page.getByText(/保存しました/).first()).toBeVisible({ timeout: 15_000 });
  await page.getByRole("button", { name: "実行", exact: true }).click();
  await page.getByRole("button", { name: "実行する" }).click();
  await page.waitForURL(/\/runs\?run=/, { timeout: 20_000 });
  const sheet = page.locator('[role="dialog"]');
  await expect(sheet.getByText("実行の詳細")).toBeVisible();
  await expect(sheet.locator("h2").getByText("成功")).toBeVisible({ timeout: 60_000 });
});
