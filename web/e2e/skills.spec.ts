import { test, expect } from "@playwright/test";

import { loginViaKeycloak, uniqueName } from "./helpers";

/// skill 管理と適用（Phase 6 Task 6.7/6.9/6.11）の E2E。
/// 前提: LLM=stub（skill を選んでも stub は「回答: <質問>」を返す＝適用経路の疎通を検証）。
test.describe("skill（作成・共有・チャット適用）", () => {
  test("UI から skill を作成し、選択してチャットを開始できる", async ({ page }) => {
    await loginViaKeycloak(page);
    await page.goto("/skills");

    // 作成ダイアログ。
    await page.getByRole("button", { name: "スキルを作成" }).click();
    const dialog = page.getByRole("dialog");
    await expect(dialog).toContainText("スキルを作成");
    const name = uniqueName("e2e-skill");
    await dialog.getByLabel(/名前/).fill(name);
    await dialog.getByLabel(/説明/).fill("E2E 検証用のスキル");
    await dialog.getByLabel(/指示文/).fill("あなたは E2E テストのアシスタントです。");
    await dialog.getByRole("button", { name: "作成" }).click();

    // 一覧に現れる。
    await expect(page.getByRole("heading", { name })).toBeVisible({ timeout: 15_000 });

    // 共有ダイアログが開き、bob に閲覧権限を付与できる（Task 6.7 ロール共有の実経路）。
    const card = page.locator("li", { has: page.getByRole("heading", { name }) });
    await card.getByRole("button", { name: `${name} を共有` }).click();
    await expect(page.getByRole("dialog")).toContainText("を共有");
    await page.getByPlaceholder("名前・メールで検索").fill("bob");
    const bobRow = page.getByRole("dialog").locator("li", { hasText: "bob" }).first();
    await bobRow.getByRole("button", { name: "共有" }).click();
    await expect(page.getByRole("dialog").getByText("共有中の相手")).toBeVisible();
    await page.keyboard.press("Escape");

    // バージョン履歴が開く。
    await card.getByRole("button", { name: `${name} のバージョン履歴` }).click();
    await expect(page.getByRole("dialog")).toContainText("バージョン履歴");
    await expect(page.getByRole("dialog").getByText("v1")).toBeVisible();
    await page.keyboard.press("Escape");

    // このスキルでチャット → skill ピン付きスレッドが作られ、生成が通る（適用経路の疎通）。
    await card.getByRole("button", { name: "このスキルでチャット" }).click();
    await page.waitForURL(/\/c\/[0-9a-f-]+/i, { timeout: 20_000 });
    const input = page.getByLabel("メッセージを入力");
    await input.fill("経費について教えて");
    await page.getByRole("button", { name: "送信" }).click();
    await expect(page.getByText(/回答/).first()).toBeVisible({ timeout: 30_000 });
  });

  test("ホームでスキルを選んでチャットを開始できる", async ({ page }) => {
    await loginViaKeycloak(page);
    await page.goto("/");

    // 前テストで作成したスキルがピッカーに出る（読み込みは非同期なので待つ）。
    const picker = page.getByLabel("スキルを選択");
    await picker.waitFor({ timeout: 10_000 });

    await picker.getByRole("button").first().click();
    const input = page.getByLabel("メッセージを入力");
    await input.fill("こんにちは");
    await page.getByRole("button", { name: "送信" }).click();
    await page.waitForURL(/\/c\/[0-9a-f-]+/i, { timeout: 20_000 });
    await expect(page.getByText(/回答/).first()).toBeVisible({ timeout: 30_000 });
  });
});
