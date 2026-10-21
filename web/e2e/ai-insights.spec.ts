import { test, expect } from "@playwright/test";

import { loginAs, loginViaKeycloak } from "./helpers";

/// AI 活用分析（#751）と回答の評価の E2E。
/// 前提: dev seed で alice が組織管理者、bob は一般メンバー。LLM=stub。
test.describe("AI 活用分析", () => {
  test("管理者は 4 画面を行き来でき、個人別は設定でオンにしたときだけ見られる", async ({ page }) => {
    await loginViaKeycloak(page);
    await page.goto("/admin/ai/insights");
    await expect(page.getByRole("heading", { name: "活用分析" })).toBeVisible();
    await expect(page.getByRole("region", { name: "指標" })).toContainText("利用率");

    // サブメニュー（AI のプロダクトメニュー）で移動しても期間の絞り込みは保たれる。
    const menu = page.getByRole("navigation", { name: "AI" });
    await page.getByRole("tab", { name: "90日" }).click();
    await menu.getByRole("link", { name: "利用者と部署" }).click();
    await expect(page.getByRole("heading", { name: "利用者と部署" })).toBeVisible();
    await expect(page.getByRole("tab", { name: "90日" })).toHaveAttribute("aria-selected", "true");
    await menu.getByRole("link", { name: "用途", exact: true }).click();
    await expect(page.getByRole("heading", { name: "用途", exact: true })).toBeVisible();
    await expect(page.getByText("使われている機能")).toBeVisible();
    await menu.getByRole("link", { name: "効果", exact: true }).click();
    await expect(page.getByRole("heading", { name: "効果", exact: true })).toBeVisible();
    await expect(page.getByText("削減時間の内訳")).toBeVisible();

    // 個人別はオフの間は案内だけ。設定でオンにすると「表示する」で一覧が開ける。
    await page.goto("/admin/ai/insights/adoption");
    await expect(page.getByText("個人単位の表示はオフです")).toBeVisible();
    await page.goto("/admin/ai/insights/settings");
    const individual = page.getByRole("switch", { name: "個人単位の表示" });
    await expect(individual).not.toBeChecked();
    await individual.click();
    await page.getByRole("button", { name: "変更を保存" }).click();
    await expect(page.getByText("分析の設定を保存しました", { exact: true })).toBeVisible();

    await page.goto("/admin/ai/insights/adoption");
    await page.getByRole("button", { name: "表示する" }).click();
    await expect(page.getByRole("table", { name: "個人別の利用" }).or(page.getByText("この期間に使った人はいません"))).toBeVisible();

    // 後片付け（再実行でも同じ前提にする）。
    await page.goto("/admin/ai/insights/settings");
    await page.getByRole("switch", { name: "個人単位の表示" }).click();
    await page.getByRole("button", { name: "変更を保存" }).click();
    await expect(page.getByText("分析の設定を保存しました", { exact: true })).toBeVisible();
  });

  test("前提の時間は範囲外だと保存できない", async ({ page }) => {
    await loginViaKeycloak(page);
    await page.goto("/admin/ai/insights/settings");
    await page.getByLabel("時給（円）").fill("0");
    await expect(page.getByText("時給は 1〜100,000 円の整数で入力してください")).toBeVisible();
    await expect(page.getByRole("button", { name: "変更を保存" })).toBeDisabled();
    await page.getByRole("button", { name: "取り消す" }).click();
  });

  test("一般メンバーには開かない", async ({ page }) => {
    await loginAs(page, "bob");
    await page.goto("/admin/ai/insights");
    await expect(page.getByText("組織の管理者だけが使えます")).toBeVisible();
  });
});

test.describe("回答の評価", () => {
  test("👍/👎 を付け直せて、👎 には理由を選べ、再読込しても残る", async ({ page }) => {
    await loginViaKeycloak(page);
    await page.getByLabel("メッセージを入力").fill("評価の E2E");
    await page.getByRole("button", { name: "送信" }).click();
    await page.waitForURL(/\/c\//);
    const up = page.getByRole("button", { name: "役に立った" }).last();
    const down = page.getByRole("button", { name: "役に立たなかった" }).last();
    await expect(down).toBeVisible({ timeout: 30_000 });

    await up.click();
    await expect(up).toHaveAttribute("aria-pressed", "true");
    await down.click();
    await expect(up).toHaveAttribute("aria-pressed", "false");
    await expect(down).toHaveAttribute("aria-pressed", "true");
    await page.getByRole("button", { name: "社内の情報が見つからない" }).click();

    await page.reload();
    await expect(page.getByRole("button", { name: "役に立たなかった" }).last()).toHaveAttribute(
      "aria-pressed",
      "true",
    );
    // もう一度押すと取り消せる。
    await page.getByRole("button", { name: "役に立たなかった" }).last().click();
    await page.reload();
    await expect(page.getByRole("button", { name: "役に立たなかった" }).last()).toHaveAttribute(
      "aria-pressed",
      "false",
    );
  });
});
