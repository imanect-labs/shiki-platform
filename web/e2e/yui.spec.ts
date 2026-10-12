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
/// 前提: LLM=stub。`yui:<kind>` で ```ui ブロックを含む本文が数文字ずつ（行の途中でも区切れて）流れる。
test.describe("```ui ブロックの図", () => {
  test("図が描かれ、生成完了後は表でも見られる", async ({ page }) => {
    await loginViaKeycloak(page);
    // 生成中に図へ一度でも入ったデータを記録する。図の <desc> には描いている値の表が入るので、
    // 書きかけの値が描かれていないかを後で確かめられる。
    await page.addInitScript(() => {
      const seen = new Set<string>();
      (window as unknown as { __yuiSeen: Set<string> }).__yuiSeen = seen;
      new MutationObserver(() => {
        document
          .querySelectorAll('[data-testid="ui-figure"] svg desc')
          .forEach((d) => seen.add(d.textContent ?? ""));
      }).observe(document, { subtree: true, childList: true, characterData: true });
    });
    await send(page, "yui:bar");

    const figure = page.getByTestId("ui-figure");
    await expect(figure).toBeVisible({ timeout: 30_000 });
    await expect(figure).toHaveAttribute("data-ui-kind", "bar");
    await expect(figure.getByRole("img", { name: /四半期ごとの売上と利益/ })).toBeVisible();
    // 最後の行まで届いてから（生成完了後）だけ操作が出る。
    await expect(page.getByText("以上です。")).toBeVisible({ timeout: 30_000 });
    await expect(figure.getByRole("button", { name: "表で表示" })).toBeVisible({ timeout: 30_000 });
    // 3 文字ずつ届くので「Q2 | 15」（150 の途中）や「Q1 | 120 | 3」（30 の途中）で区切れる瞬間がある。
    // それでも書きかけの値（1・2・3・4・15）が表のセルとして入ったことは一度も無い。
    const seen = await page.evaluate(() =>
      [...(window as unknown as { __yuiSeen: Set<string> }).__yuiSeen].join("\n"),
    );
    expect(seen).toMatch(/\|\s*Q4\s*\|\s*210\s*\|\s*44\s*\|/);
    expect(seen).not.toMatch(/\|\s*(1|2|3|4|15)\s*\|/);

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
    const fallback = page.getByTestId("ui-figure-fallback");
    await expect(fallback.getByText("これは図ではありません")).toBeVisible();
    await expect(fallback.getByText("図にできなかったため、記述をそのまま表示しています。")).toBeVisible();
  });
});
