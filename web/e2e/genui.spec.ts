import { test, expect } from "@playwright/test";

import type { Page } from "@playwright/test";

import { loginViaKeycloak, uniqueName } from "./helpers";

/// ホームから会話を開始して `text` を送信する。通常チャットがモデル裁量ループ
/// （issue #102）になったため、generative UI はトグルなしでモデルが emit_ui を呼んで出す。
async function sendInAgentMode(page: Page, text: string) {
  await page.goto("/");
  const input = page.getByLabel("メッセージを入力");
  await input.fill(text);
  await page.getByRole("button", { name: "送信" }).click();
  await page.waitForURL(/\/c\/[0-9a-f-]+/i, { timeout: 20_000 });
}

/// generative UI（Phase 6 Task 6.4/6.5/6.6）の E2E。
/// 前提: LLM=stub。`genui:` プレフィックスで emit_ui が決定論的に駆動される:
/// - `genui:form` … chat.submit 束縛つきフォーム / `genui:table`・`genui:chart` … 表示系
/// - `genui:bad` … カタログ外コンポーネント（検証拒否 → テキストフォールバック）
test.describe("generative UI（検証済みスペックの描画とアクション）", () => {
  test("フォームが描画され、送信すると新しい発話と応答が生まれる", async ({ page }) => {
    await loginViaKeycloak(page);
    await sendInAgentMode(page, "genui:form");

    // 検証済みフォームが描画される（stub の 固定 スペック: コメント入力＋送信）。
    const comment = page.getByLabel(/コメント/);
    await expect(comment).toBeVisible({ timeout: 30_000 });

    // フォーム送信 → chat.submit が新しい user メッセージを作り、生成が走る。
    const feedback = uniqueName("とても参考になりました");
    await comment.fill(feedback);
    await page
      .getByTestId("genui-form-feedback")
      .getByRole("button", { name: "送信" })
      .click();

    // 送信内容が新しい発話として現れ、stub の応答が続く（フォーム内の入力値と発話の 2 箇所に出る）。
    await expect(page.getByText(feedback, { exact: false }).first()).toBeVisible({ timeout: 30_000 });
    await expect(page.getByText(/回答/).first()).toBeVisible({ timeout: 30_000 });
  });

  test("テーブルとチャートが描画される", async ({ page }) => {
    await loginViaKeycloak(page);

    // テーブル。
    await sendInAgentMode(page, "genui:table");
    await expect(page.getByTestId("genui-table")).toBeVisible({ timeout: 30_000 });
    await expect(page.getByRole("columnheader", { name: "項目" })).toBeVisible();

    // チャート（recharts の SVG が描画される）。
    await sendInAgentMode(page, "genui:chart");
    await expect(page.getByTestId("genui-chart")).toBeVisible({ timeout: 30_000 });
    await expect(page.getByTestId("genui-chart").locator("svg").first()).toBeVisible();
  });

  test("不正スペックは UI にならずテキストで応答される（検証拒否のフォールバック）", async ({
    page,
  }) => {
    await loginViaKeycloak(page);
    await sendInAgentMode(page, "genui:bad");

    // テキスト応答（stub の 2 ターン目）は返る＝クラッシュせず縮退。
    await expect(page.getByText(/回答/).first()).toBeVisible({ timeout: 30_000 });
    // 検証拒否されたスペックは generative UI として描画されない。
    await expect(page.getByTestId("genui-root")).toHaveCount(0);
  });
});
