import { test, expect } from "@playwright/test";
import { loginViaKeycloak } from "./helpers";

// 公開コーパスを同梱した server と stub が必要。通常の corpus なし CI は起動を妨げない。
test("ヘルプ検索の節リンクと専用出典が再訪後も残る", async ({ page }) => {
  test.skip(process.env.HELP_CORPUS_E2E !== "1", "同梱コーパスが必要");
  await loginViaKeycloak(page);
  await page.goto("/");
  await page.getByLabel("メッセージを入力").fill("helpsearch:ログイン用のパスキーを追加する手順は？");
  await page.getByRole("button", { name: "送信", exact: true }).click();
  await page.waitForURL(/\/c\/[0-9a-f-]+/i);
  // 本文の [H1] は押すとその見出しのヘルプを新しいタブで開く番号チップになる。
  const chip = page.getByRole("link", { name: /^ヘルプ 1:/ });
  await expect(chip).toBeVisible({ timeout: 30000 });
  await expect(chip).toHaveAttribute("href", /^https:\/\/shiki\.imanect\.app\/help\/account\/security\/add-passkey\/#/);
  await expect(chip).toHaveAttribute("target", "_blank");
  await expect(page.getByText("ヘルプの手順を確認してください。", { exact: false })).toBeVisible();
  // 出典は回答の下に 1 行で畳み、開くとページごとに引用箇所を並べる。
  await page.getByRole("button", { name: /ヘルプ \d+ ページから \d+ 箇所を引用/ }).click();
  const sources = page.getByRole("region", { name: "ヘルプの出典" });
  const link = sources.getByRole("link", { name: "ヘルプで開く" }).first();
  await expect(link).toHaveAttribute("href", /^https:\/\/shiki\.imanect\.app\/help\/account\/security\/add-passkey\/$/);
  const href = await chip.getAttribute("href");
  await page.reload();
  await expect(chip).toHaveAttribute("href", href!);
  await page.screenshot({ path: "test-results/help-search.png", fullPage: true });
});
