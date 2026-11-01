import { test, expect } from "@playwright/test";

/// 回答の画像の出所チェック（/reference/answer-images・#1005・PIT-77）。
/// 認証・バックエンド・LLM 不要（middleware は /reference を除外）＝ web 単体で回せる。
///
/// 守ること: 出典（ツール結果）に現れた画像だけを読み込み、LLM が組み立てた URL へは
/// **ブラウザが 1 度もリクエストを送らない**（DOM に出ないことではなく、通信が起きないことを見る）。
/// `SHOTS_DIR` を渡すとライト/ダークの PNG を保存する。
const SHOTS = process.env.SHOTS_DIR;

/// 1x1 の PNG（画像のホストは `.test` で実在しないので、ここで応答する）。
const PNG = Buffer.from(
  "iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAYAAAAfFcSJAAAADUlEQVR42mNk+M9QDwADhgGAWjR9awAAAABJRU5ErkJggg==",
  "base64",
);

test.use({ deviceScaleFactor: 2 });

for (const theme of ["light", "dark"] as const) {
  test(`出典に現れた画像だけを読み込み、それ以外へは通信しない（${theme}）`, async ({ page }) => {
    await page.emulateMedia({ colorScheme: theme });
    const requested: string[] = [];
    await page.route(/\.test\//, async (route) => {
      requested.push(route.request().url());
      await route.fulfill({ status: 200, contentType: "image/png", body: PNG });
    });

    await page.goto("/reference/answer-images");
    const timeline = page.getByTestId("answer-images-timeline");

    // 出典（web_fetch の結果）に現れた画像は描く。Referer を送らない。
    const shown = timeline.getByTestId("answer-image");
    await expect(shown).toHaveCount(1);
    await expect(shown).toHaveAttribute("src", "https://images.example.test/market/saas-share-2026.png");
    await expect(shown).toHaveAttribute("referrerpolicy", "no-referrer");
    await expect(shown).toHaveAttribute("alt", "SaaS 市場の分野別シェア");
    await expect.poll(() => shown.evaluate((img: HTMLImageElement) => img.complete && img.naturalWidth)).toBe(1);

    // 出典の URL に何かを足したもの・出典に無いホストは、読み込まずにホストを添えたリンクにする。
    const blocked = timeline.getByTestId("answer-image-blocked");
    await expect(blocked).toHaveCount(2);
    await expect(blocked.nth(0)).toContainText("補足の図");
    await expect(blocked.nth(0)).toContainText("(images.example.test)");
    await expect(blocked.nth(1)).toContainText("画像");
    await expect(blocked.nth(1)).toContainText("(collector.attacker.test)");
    await expect(blocked.nth(1)).toHaveAttribute("href", "https://collector.attacker.test/p?d=Q3%20revenue%2012.4B");
    await expect(timeline.locator("img")).toHaveCount(1);

    // 通信は出典の画像の 1 件だけ（組み立てた URL へは 1 度も送っていない）。
    await page.waitForLoadState("networkidle");
    expect(requested).toEqual(["https://images.example.test/market/saas-share-2026.png"]);

    if (SHOTS) await timeline.screenshot({ path: `${SHOTS}/answer-images-${theme}.png` });
  });
}
