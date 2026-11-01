import { test, expect } from "@playwright/test";

/// 回答の画像の出所チェック（/reference/answer-images・#1005・PIT-77）。
/// 認証・バックエンド・LLM 不要（middleware は /reference を除外）＝ web 単体で回せる。
///
/// 守ること: 出典（ツール結果）に現れた画像だけを読み込み、LLM が組み立てた URL へは
/// **ブラウザが 1 度もリクエストを送らない**（DOM に出ないことではなく、通信が起きないことを見る）。
/// `SHOTS_DIR` を渡すとライト/ダークの PNG を保存する。
const SHOTS = process.env.SHOTS_DIR;

/// 出典の画像（ホストは `.test` で実在しないので、ここで応答する）。見た目の確認用に図らしい大きさにする。
const FIGURE = `<svg xmlns="http://www.w3.org/2000/svg" width="640" height="300" viewBox="0 0 640 300">
<rect width="640" height="300" fill="#f4f6fb"/>
<rect x="60" y="60" width="300" height="48" rx="6" fill="#2f6bff"/>
<rect x="60" y="126" width="200" height="48" rx="6" fill="#41d6f2"/>
<rect x="60" y="192" width="236" height="48" rx="6" fill="#9aa4b8"/>
</svg>`;

test.use({ deviceScaleFactor: 2 });

for (const theme of ["light", "dark"] as const) {
  test(`出典に現れた画像だけを読み込み、それ以外へは通信しない（${theme}）`, async ({ page }) => {
    await page.emulateMedia({ colorScheme: theme });
    const requested: string[] = [];
    await page.route(/\.test\//, async (route) => {
      requested.push(route.request().url());
      await route.fulfill({ status: 200, contentType: "image/svg+xml", body: FIGURE });
    });

    await page.goto("/reference/answer-images");
    const timeline = page.getByTestId("answer-images-timeline");

    // 出典（web_fetch の結果）に現れた画像は描く。Referer を送らない。
    const all = timeline.getByTestId("answer-image");
    await expect(all).toHaveCount(2);
    const shown = all.first();
    await expect(shown).toHaveAttribute("src", "https://images.example.test/market/saas-share-2026.png");
    await expect(shown).toHaveAttribute("referrerpolicy", "no-referrer");
    await expect(shown).toHaveAttribute("alt", "SaaS 市場の分野別シェア");
    await expect.poll(() => shown.evaluate((img: HTMLImageElement) => img.complete && img.naturalWidth)).toBe(640);
    // 描いた画像には出典のホストを添える（どこから来た画像かを読み手が判断できる）。
    await expect(timeline.getByTestId("answer-image-caption").first()).toHaveText(
      "SaaS 市場の分野別シェア · images.example.test",
    );
    // htmd が括弧をエスケープした URL・react-markdown が `|` をエンコードした URL も、出典と同じ画像として描く。
    await expect(all.nth(1)).toHaveAttribute("alt", "内訳グラフ");
    await expect.poll(() => all.nth(1).evaluate((img: HTMLImageElement) => img.complete && img.naturalWidth)).toBe(640);

    // 出典の URL に何かを足したもの・出典に無いホストは、読み込まずにホストを添えたリンクにする。
    const blocked = timeline.getByTestId("answer-image-blocked");
    await expect(blocked).toHaveCount(4);
    await expect(blocked.nth(0)).toContainText("外部の画像「補足の図」");
    await expect(blocked.nth(0)).toContainText("images.example.test");
    await expect(blocked.nth(1)).toHaveText(/^外部の画像\s*collector\.attacker\.test$/);
    await expect(blocked.nth(1)).toHaveAttribute("href", "https://collector.attacker.test/p?d=Q3%20revenue%2012.4B");
    // モデル自身が書いた文字列（シェルの出力・封筒の外の案内）にだけ現れる URL は出所に数えない。
    await expect(blocked.nth(2)).toHaveText(/^外部の画像\s*collector\.attacker\.test$/);
    await expect(blocked.nth(2)).toHaveAttribute("href", "https://collector.attacker.test/e?d=Q3%20margin%2018%25");
    await expect(timeline.locator("img")).toHaveCount(2);
    // リンクの中の画像は、外側のリンクを残してラベルだけにする（<a> を入れ子にしない）。
    await expect(blocked.nth(3)).toHaveText(/^外部の画像「build」\s*badge\.example\.test$/);
    await expect(timeline.locator("a a")).toHaveCount(0);
    await expect(timeline.getByRole("link", { name: /build/ })).toHaveAttribute("href", "https://ci.example.test/runs/1");

    // 通信は出典の画像の 2 件だけ（組み立てた URL へは 1 度も送っていない）。
    await page.waitForLoadState("networkidle");
    expect(requested.map((u) => new URL(u).host).sort()).toEqual(["charts.example.test", "images.example.test"]);
    expect(requested).toContain("https://images.example.test/market/saas-share-2026.png");

    if (SHOTS) await timeline.screenshot({ path: `${SHOTS}/answer-images-${theme}.png` });
  });
}
