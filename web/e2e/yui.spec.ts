import { test, expect } from "@playwright/test";

import type { Page } from "@playwright/test";

import { loginViaKeycloak } from "./helpers";

/// ホームから会話を開始して `text` を送信する。
async function send(page: Page, text: string) {
  await page.goto("/");
  const input = page.getByLabel("メッセージを入力");
  await input.fill(text);
  await page.getByRole("button", { name: "送信" }).click();
  await page.waitForURL(/\/c\/[0-9a-f-]+/i, { timeout: 20_000 });
}

/// 回答の Markdown 内の ```ui ブロック（Yui）を図にする（#530）。
/// 前提: LLM=stub。`yui:<kind>` で ```ui ブロックを含む本文が 1 行ずつ流れる。
test.describe("```ui ブロックの図", () => {
  test("図が描かれ、生成完了後は表でも見られる", async ({ page }) => {
    await loginViaKeycloak(page);
    await send(page, "yui:bar");

    const figure = page.getByTestId("ui-figure");
    await expect(figure).toBeVisible({ timeout: 30_000 });
    await expect(figure).toHaveAttribute("data-ui-kind", "bar");
    await expect(figure.getByRole("img", { name: /四半期ごとの売上と利益/ })).toBeVisible();
    // 最後の行まで届いてから（生成完了後）だけ操作が出る。
    await expect(page.getByText("以上です。")).toBeVisible({ timeout: 30_000 });
    await expect(figure.getByRole("button", { name: "表で表示" })).toBeVisible({ timeout: 30_000 });

    // 表に切り替えると、値が表のセルとして読める。
    await figure.getByRole("button", { name: "表で表示" }).click();
    await expect(figure.getByRole("cell", { name: "210" })).toBeVisible();
    await figure.getByRole("button", { name: "図で表示" }).click();
    await expect(figure.getByRole("img")).toBeVisible();
  });

  test("流れ図・構成図・数値カードも描ける", async ({ page }) => {
    await loginViaKeycloak(page);
    await send(page, "yui:all");
    await expect(page.getByText("以上です。")).toBeVisible({ timeout: 60_000 });
    const kinds = await page
      .getByTestId("ui-figure")
      .evaluateAll((els) => els.map((e) => e.getAttribute("data-ui-kind")));
    expect(kinds).toEqual(["bar", "line", "stat", "flow", "stack"]);
  });

  test("図にできないブロックはコードとして表示される", async ({ page }) => {
    await loginViaKeycloak(page);
    await send(page, "yui:bad");
    await expect(page.getByText("以上です。")).toBeVisible({ timeout: 30_000 });
    await expect(page.getByTestId("ui-figure")).toHaveCount(0);
    await expect(page.getByText("これは図ではありません")).toBeVisible();
  });
});
