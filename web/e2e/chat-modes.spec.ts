import { test, expect } from "@playwright/test";

import { loginViaKeycloak } from "./helpers";

/// ホームのモード切替（#964）・ワークスペース選択（Phase 6 UX）の E2E。
/// 会話の図を「アプリとして保存」する経路は mini-apps.spec.ts（```ui app・#604）で確かめる。
/// 前提: LLM=stub・通常チャットはモデル裁量ループ（issue #102）。
test.describe("ホームのモード切替", () => {
  test("モードは入力欄の上で選び、エージェントにすると作業フォルダと承認モードを選べる", async ({
    page,
  }) => {
    await loginViaKeycloak(page);
    await page.goto("/");

    // ホームの入力欄にはエージェントモードのトグルを置かない（モードは入力欄の上の切替）。
    await expect(page.getByRole("tablist", { name: "モード" })).toBeVisible();
    await expect(page.getByRole("switch", { name: "エージェントモード" })).toHaveCount(0);
    await expect(page.getByTestId("home-mode-chat")).toHaveAttribute("aria-selected", "true");

    // チャットでは作業フォルダは出ない。エージェントにすると出る。
    const folder = page.getByRole("button", { name: /作業フォルダ/ });
    await expect(folder).toBeHidden();
    await page.getByTestId("home-mode-agent").click();
    await expect(folder).toBeVisible();
    await expect(folder).toContainText("マイドライブ");
    await expect(page.getByTestId("home-approval-require_approval")).toHaveAttribute(
      "aria-checked",
      "true",
    );

    // ←→ でモードを移れる（tablist の作法）。
    await page.getByTestId("home-mode-agent").press("ArrowRight");
    await expect(page.getByTestId("home-mode-create")).toBeFocused();
    await expect(page.getByTestId("home-mode-create")).toHaveAttribute("aria-selected", "true");
    await expect(page.getByRole("radiogroup", { name: "作るもの" })).toBeVisible();
    await page.getByTestId("home-mode-create").press("ArrowLeft");
    await expect(page.getByTestId("home-mode-agent")).toHaveAttribute("aria-selected", "true");

    // 作業フォルダのチップからフォルダ選択ダイアログが開く（「配下に作成」「このフォルダを使う」の 2 アクション）。
    await folder.click();
    await expect(page.getByRole("dialog")).toContainText("ワークスペースの場所");
    await page.keyboard.press("Escape");

    // エージェントのモードで送ると、エージェントモードの会話として始まる。
    const input = page.getByLabel("メッセージを入力");
    await input.fill(`エージェントで始める ${Date.now().toString(36)}`);
    await page.getByRole("button", { name: "送信" }).click();
    await page.waitForURL(/\/c\/[0-9a-f-]+/i, { timeout: 20_000 });
    await expect(page.getByRole("switch", { name: "エージェントモード" })).toHaveAttribute(
      "aria-checked",
      "true",
      { timeout: 20_000 },
    );
  });

  test("作成のモードで送ると、作るものの種類が発話の先頭に入る", async ({ page }) => {
    await loginViaKeycloak(page);
    await page.goto("/");

    await page.getByTestId("home-mode-create").click();
    await page.getByTestId("home-create-sheet").click();
    await expect(page.getByTestId("home-create-sheet")).toHaveAttribute("aria-checked", "true");
    const input = page.getByLabel("メッセージを入力");
    await expect(input).toHaveAttribute("placeholder", /Excel/);

    const what = `月次の経費集計表 ${Date.now().toString(36)}`;
    await input.fill(what);
    await page.getByRole("button", { name: "送信" }).click();
    await page.waitForURL(/\/c\/[0-9a-f-]+/i, { timeout: 20_000 });
    await expect(page.getByText(`Excel で作成：${what}`).first()).toBeVisible({ timeout: 20_000 });
  });
});
