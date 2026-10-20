import { test, expect } from "@playwright/test";

import { loginViaKeycloak, uniqueName } from "./helpers";

/// チャットモード整理・ワークスペース選択（Phase 6 UX）の E2E。
/// 会話の図を「アプリとして保存」する経路は mini-apps.spec.ts（```ui app・#604）で確かめる。
/// 前提: LLM=stub・通常チャットはモデル裁量ループ（issue #102）。
test.describe("チャットモード整理", () => {
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
});
