import { test, expect } from "@playwright/test";

/// 製品ヘルプの出典の表示（/reference/citations・#1244）。
/// 認証・バックエンド・LLM 不要（middleware は /reference を除外）＝ web 単体で回せる。
///
/// 守ること:
/// - 本文の `[Hn]` は、押すとその見出しのヘルプを新しいタブで開く番号チップになる。
/// - 社内文書の `[n]` と隣り合っても（`[H1][1]`・`[1][H1]`）、両方ともチップになる。コード中は変えない。
/// - 出典は回答の下に 1 行で畳み、開くとページごとに引用箇所を並べる（キーボードでも開ける）。
test("ヘルプの出典は本文のチップと回答下の 1 行に出る", async ({ page }) => {
  await page.goto("/reference/citations");

  const mixed = page.getByTestId("citations-help-mixed");
  const helpChips = mixed.getByRole("link", { name: /^ヘルプ \d+:/ });
  await expect(helpChips).toHaveCount(2);
  await expect(helpChips.nth(0)).toHaveAttribute("href", /^https:\/\/shiki\.imanect\.app\/help\/account\/security\/password\/#/);
  await expect(helpChips.nth(0)).toHaveAttribute("target", "_blank");
  await expect(helpChips.nth(1)).toHaveText("H3");
  // 社内文書の番号も、ヘルプの番号の前後どちらにあってもチップになる。
  await expect(mixed.getByRole("button", { name: /^出典 1:/ })).toBeVisible();
  await expect(mixed.getByRole("button", { name: /^出典 2:/ })).toBeVisible();
  await expect(mixed.locator("code", { hasText: "[H1][1]" })).toBeVisible();
  // 数えるのは本文で引いた箇所だけ（H2 は検索したが使わなかったので最後に畳む）。
  await mixed.getByRole("button", { name: "ヘルプ 2 ページから 2 箇所を引用" }).click();
  await mixed.getByRole("button", { name: "検索したが回答に使わなかった 1 件" }).click();
  await expect(
    mixed.getByRole("region", { name: "ヘルプの出典" }).getByRole("link", { name: /始める前に/ }),
  ).toHaveAttribute("href", /#%E5%A7%8B%E3%82%81%E3%82%8B%E5%89%8D%E3%81%AB$/);

  const help = page.getByTestId("citations-help");
  const summary = help.getByRole("button", { name: "ヘルプ 2 ページから 3 箇所を引用" });
  await expect(summary).toHaveAttribute("aria-expanded", "false");
  await expect(help.getByRole("region", { name: "ヘルプの出典" })).toHaveCount(0);
  await summary.focus();
  await page.keyboard.press("Enter");
  const region = help.getByRole("region", { name: "ヘルプの出典" });
  await expect(region).toBeVisible();
  // ページの「ヘルプで開く」はページへ（見出しなし）。
  await expect(region.getByRole("link", { name: /ヘルプで開く/ }).first()).toHaveAttribute(
    "href",
    "https://shiki.imanect.app/help/account/security/password/",
  );
});
