import { test, expect } from "@playwright/test";

import { loginAs, uniqueName } from "./helpers";

/// カスタム指示（設定 → パーソナライズ・#900）の E2E。
/// 前提: chat 有効＋ LLM=stub。stub は発話が `echosystem:` で始まると受け取った system プロンプトを
/// そのまま本文で返すので、保存した指示が実際にモデルへ届くことまで確かめられる。
/// 他の spec（alice）の会話へ影響させないよう bob で行い、最後に空へ戻す。
test.describe("カスタム指示", () => {
  test("保存して再訪しても残り、チャットの system プロンプトに届く", async ({ page }) => {
    test.setTimeout(120_000);
    await loginAs(page, "bob");
    await page.goto("/settings/ai");

    const about = page.getByLabel("あなたについて");
    const style = page.getByLabel("答え方の希望");
    await expect(about).toBeEditable({ timeout: 20_000 });

    const marker = uniqueName("営業企画");
    await about.fill(`${marker}。提案書の作成が多い。`);
    await style.fill("結論を先に。");
    await page.getByRole("combobox", { name: "口調" }).click();
    await page.getByRole("option", { name: "簡潔" }).click();

    await expect(page.getByText("保存していない変更があります")).toBeVisible();
    await page.getByRole("button", { name: "変更を保存" }).click();
    await expect(page.getByText("カスタム指示を保存しました").first()).toBeVisible();
    await expect(page.getByText("保存していない変更があります")).toBeHidden();

    // 再訪しても残る（サーバに保存されている）。
    await page.reload();
    await expect(about).toHaveValue(`${marker}。提案書の作成が多い。`, { timeout: 20_000 });
    await expect(style).toHaveValue("結論を先に。");
    await expect(page.getByRole("combobox", { name: "口調" })).toHaveText("簡潔");

    // チャットの system プロンプトへ届く（stub が system をそのまま返す）。
    await page.goto("/");
    const input = page.getByLabel("メッセージを入力");
    await input.click();
    await input.fill("echosystem: 確認");
    await page.getByRole("button", { name: "送信" }).click();
    await page.waitForURL(/\/c\/[0-9a-f-]+/i, { timeout: 20_000 });
    await expect(page.getByText(/ユーザーのカスタム指示/).first()).toBeVisible({ timeout: 30_000 });
    await expect(page.getByText(new RegExp(marker)).first()).toBeVisible();

    // 上限を超えると保存できず、理由が出る。
    await page.goto("/settings/ai");
    await expect(about).toBeEditable({ timeout: 20_000 });
    await about.fill("あ".repeat(1501));
    await expect(page.getByText("1,501 / 1,500")).toBeVisible();
    await expect(page.getByText("「あなたについて」は 1,500 文字以内で入力してください")).toBeVisible();
    await expect(page.getByRole("button", { name: "変更を保存" })).toBeDisabled();
    await page.getByRole("button", { name: "取り消す" }).click();
    await expect(about).toHaveValue(`${marker}。提案書の作成が多い。`);

    // 空に戻すと未設定になる（後片付けを兼ねる）。
    await about.fill("");
    await style.fill("");
    await page.getByRole("combobox", { name: "口調" }).click();
    await page.getByRole("option", { name: "標準" }).click();
    await page.getByRole("button", { name: "変更を保存" }).click();
    await expect(page.getByText("カスタム指示を保存しました").first()).toBeVisible();
    await page.reload();
    await expect(about).toBeEditable({ timeout: 20_000 });
    await expect(about).toHaveValue("");
    await expect(style).toHaveValue("");
  });
});
