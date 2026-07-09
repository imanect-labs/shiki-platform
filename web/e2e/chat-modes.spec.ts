import { test, expect } from "@playwright/test";

import { loginViaKeycloak, uniqueName } from "./helpers";

/// チャットモード整理・ワークスペース選択・アプリ保存（Phase 6 UX）の E2E。
/// 前提: LLM=stub・通常チャットはモデル裁量ループ（issue #102）。
test.describe("チャットモード整理とアプリ保存", () => {
  test("トグルは「エージェントモード」1 つで、ON にするとワークスペース場所を選べる", async ({
    page,
  }) => {
    await loginViaKeycloak(page);
    await page.goto("/");

    // 「エージェントモード」トグルが 1 つある（旧「エージェント/自律」の 2 トグルではない）。
    const toggle = page.getByRole("switch", { name: "エージェントモード" });
    await expect(toggle).toBeVisible();
    await expect(page.getByRole("switch", { name: "自律モード" })).toHaveCount(0);

    // OFF ではワークスペースチップは出ない。ON にすると出る。
    await expect(page.getByRole("button", { name: /マイドライブ/ })).toHaveCount(0);
    await toggle.click();
    const chip = page.getByRole("button", { name: /マイドライブ/ });
    await expect(chip).toBeVisible();

    // チップからフォルダ選択ダイアログが開く（「配下に作成」「このフォルダを使う」の 2 アクション）。
    await chip.click();
    await expect(page.getByRole("dialog")).toContainText("ワークスペースの場所");
    await page.keyboard.press("Escape");
  });

  test("通常チャットで生成した表示系 UI を「アプリとして保存」できる", async ({ page }) => {
    await loginViaKeycloak(page);
    await page.goto("/");

    // 通常チャット（トグルなし）で chart を生成する＝モデル裁量ループ。
    await page.getByLabel("メッセージを入力").fill("genui:chart");
    await page.getByRole("button", { name: "送信" }).click();
    await page.waitForURL(/\/c\/[0-9a-f-]+/i, { timeout: 20_000 });
    await expect(page.getByTestId("genui-chart")).toBeVisible({ timeout: 30_000 });

    // 「アプリとして保存」→ 名前を付けて保存 → アプリ実行画面へ遷移する。
    await page.getByRole("button", { name: "アプリとして保存" }).first().click();
    const dialog = page.getByRole("dialog");
    await expect(dialog).toContainText("アプリとして保存");
    const name = uniqueName("保存アプリ");
    await dialog.getByLabel(/名前/).fill(name);
    await dialog.getByRole("button", { name: "保存" }).click();
    await page.waitForURL(/\/apps\/[0-9a-f-]+/i, { timeout: 20_000 });
    await expect(page.getByTestId("genui-chart")).toBeVisible({ timeout: 15_000 });

    // 一覧にも出る。
    await page.goto("/apps");
    await expect(page.getByRole("heading", { name })).toBeVisible({ timeout: 15_000 });
  });

  test("チャット専用アクション（chat.submit）を含む UI はアプリ保存が非活性", async ({ page }) => {
    await loginViaKeycloak(page);
    await page.goto("/");
    await page.getByLabel("メッセージを入力").fill("genui:form");
    await page.getByRole("button", { name: "送信" }).click();
    await page.waitForURL(/\/c\/[0-9a-f-]+/i, { timeout: 20_000 });
    await expect(page.getByLabel(/コメント/)).toBeVisible({ timeout: 30_000 });

    // form（chat.submit 束縛）はミニアプリにできないため保存ボタンが disabled。
    await expect(page.getByRole("button", { name: "アプリとして保存" }).first()).toBeDisabled();
  });
});
