import { test, expect } from "@playwright/test";

import { loginAs, loginViaKeycloak } from "./helpers";

/// 管理コンソール（AI）とチャットのモデル選択（#553）の E2E。
/// 前提: dev seed で alice が組織管理者（organization#admin）、bob は一般メンバー。LLM=stub。
test.describe("管理コンソール", () => {
  test("管理者はアカウントメニューから入り、AI の画面を開ける", async ({ page }) => {
    await loginViaKeycloak(page);
    await page.getByRole("button", { name: "アカウントメニューを開く" }).click();
    await page.getByRole("menuitem", { name: "管理コンソール" }).click();
    await page.waitForURL(/\/admin$/);
    await expect(page.getByRole("heading", { name: "ホーム" })).toBeVisible();

    // レールから AI へ（利用量が入口）。
    await page.getByRole("link", { name: "AI" }).click();
    await page.waitForURL(/\/admin\/ai\/usage$/);
    await expect(page.getByRole("heading", { name: "利用量" })).toBeVisible();
    await expect(page.getByRole("region", { name: "指標" })).toContainText("呼び出し");

    // モデル: カタログ表と Auto 行が出る。
    await page.getByRole("link", { name: "モデル" }).click();
    await expect(page.getByRole("heading", { name: "モデル" })).toBeVisible();
    await expect(page.getByRole("radio", { name: "Auto を既定にする" })).toBeChecked();

    // 円換算レートを入れて保存できる（保存後は読み直した値が入っている）。
    // 再実行でも差分が出るよう、いまの値と違う値を入れる。
    const rate = page.getByLabel("円換算レート");
    const next = (await rate.inputValue()) === "150" ? "151" : "150";
    await rate.fill(next);
    await page.getByRole("button", { name: "変更を保存" }).click();
    await expect(page.getByText("AI の設定を保存しました", { exact: true })).toBeVisible();
    await expect(rate).toHaveValue(next);

    // 範囲外は保存ボタンが無効で理由が出る。
    await rate.fill("5000");
    await expect(page.getByText("円換算レートは 1〜1000 円で入力してください")).toBeVisible();
    await expect(page.getByRole("button", { name: "変更を保存" })).toBeDisabled();
    await page.getByRole("button", { name: "取り消す" }).click();

    // プロバイダ: 接続先が 1 つ以上出る（秘密は出さない）。
    await page.getByRole("link", { name: "プロバイダ" }).click();
    await expect(page.getByRole("heading", { name: "プロバイダ" })).toBeVisible();
    await expect(page.getByText("API キー").first()).toBeVisible();
  });

  test("一般メンバーには管理コンソールが開かない", async ({ page }) => {
    await loginAs(page, "bob");
    await page.getByRole("button", { name: "アカウントメニューを開く" }).click();
    await expect(page.getByRole("menuitem", { name: "管理コンソール" })).toHaveCount(0);
    await page.keyboard.press("Escape");
    await page.goto("/admin/ai/models");
    await expect(page.getByText("組織の管理者だけが使えます")).toBeVisible();
  });
});

test.describe("チャットのモデル選択", () => {
  test("既定は Auto でピルを出さず、＋メニューで選ぶと送信ボタンの左に出る", async ({ page }) => {
    await loginViaKeycloak(page);
    await page.goto("/");
    await expect(page.getByTestId("model-pill")).toHaveCount(0);

    // ＋ → モデル → 先頭のモデルを選ぶ。
    await page.getByRole("button", { name: "追加メニューを開く" }).click();
    await page.getByTestId("composer-model-menu").click();
    const first = page.locator('[data-testid^="composer-model-"]:not([data-testid="composer-model-menu"])').first();
    const label = (await first.locator("span span").first().innerText()).trim();
    await first.click();
    const pill = page.getByTestId("model-pill");
    await expect(pill).toBeVisible();
    await expect(pill).toContainText(label);

    // 送信すると新しいスレッドへ選択が引き継がれる。
    await page.getByLabel("メッセージを入力").fill("こんにちは");
    await page.getByRole("button", { name: "送信" }).click();
    await page.waitForURL(/\/c\/[0-9a-f-]+/i, { timeout: 20_000 });
    await expect(page.getByTestId("model-pill")).toContainText(label);

    // ピルから Auto に戻すとピルが消える。
    await page.getByTestId("model-pill").click();
    await page.getByRole("radio", { name: /Auto（おまかせ）/ }).click();
    await expect(page.getByTestId("model-pill")).toHaveCount(0);
  });
});
