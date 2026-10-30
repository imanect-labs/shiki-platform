import { test, expect, type Page } from "@playwright/test";

/// ```ui の map（地図）を、背景の地図つきで描けることを確かめる（/reference/yui のギャラリー）。
/// 認証・バックエンド不要。地図の設定（/api/map/config）とスタイルは差し替え、外部のタイル配信には出ない。
/// `SHOTS_DIR` を渡すと PNG を保存する。
const SHOTS = process.env.SHOTS_DIR;

const STYLE_URL = "https://map.invalid/style.json";

/// 最小のスタイル（地の色・水の面・地名の文字のレイヤー）。色は Shiki の配色に塗り直される。
const STYLE = {
  version: 8,
  glyphs: "https://map.invalid/fonts/{fontstack}/{range}.pbf",
  sources: {
    base: {
      type: "geojson",
      data: {
        type: "FeatureCollection",
        features: [
          {
            type: "Feature",
            properties: {},
            geometry: { type: "Polygon", coordinates: [[[139.7, 35.7], [139.8, 35.7], [139.8, 35.76], [139.7, 35.76], [139.7, 35.7]]] },
          },
        ],
      },
    },
  },
  layers: [
    { id: "background", type: "background", paint: { "background-color": "#ff0000" } },
    { id: "water", type: "fill", source: "base", paint: { "fill-color": "#0000ff" } },
    { id: "place_label", type: "symbol", source: "base", layout: { "text-field": "{name_en}", "text-font": ["Noto Sans Bold"] } },
  ],
};

async function mockMap(page: Page, config: "style" | "disabled" | "broken") {
  const requests: string[] = [];
  await page.route("**/api/map/config", (r) =>
    r.fulfill({
      json:
        config === "disabled"
          ? { provider: null, style_url_light: null, style_url_dark: null, mapbox_token: null }
          : { provider: "style", style_url_light: STYLE_URL, style_url_dark: STYLE_URL, mapbox_token: null },
    }),
  );
  await page.route("https://map.invalid/**", (r) => {
    requests.push(r.request().url());
    if (r.request().url() === STYLE_URL) {
      return config === "broken" ? r.fulfill({ status: 503, body: "" }) : r.fulfill({ json: STYLE });
    }
    // 文字のグリフは空（このテストは文字の見た目を見ない）。
    return r.fulfill({ status: 200, contentType: "application/x-protobuf", body: Buffer.alloc(0) });
  });
  return requests;
}

const section = (page: Page) => page.locator('section[data-testid="yui-map-stations"]');

test.describe("地図（```ui の map）", () => {
  test("背景の地図・番号のピン・地点のカードを描き、カードとピンの選択が連動する", async ({ page }) => {
    const requests = await mockMap(page, "style");
    await page.goto("/reference/yui");
    const sec = section(page);
    await sec.scrollIntoViewIfNeeded();
    const surface = sec.getByTestId("yui-map-surface");
    await expect(surface).toHaveAttribute("data-map-status", "ready", { timeout: 30_000 });
    expect(requests).toContain(STYLE_URL);

    // 地点ごとの番号のピンと、同じ番号のカード。
    const pins = sec.getByTestId("yui-map-pin");
    await expect(pins).toHaveCount(5);
    await expect(pins.first()).toHaveText("1");
    const cards = sec.getByTestId("yui-map-places").getByRole("listitem");
    await expect(cards).toHaveCount(5);
    await expect(cards.nth(3)).toContainText("町屋駅");

    // カードを選ぶと、そのピンが強調される。もう一度押すと外れる。
    const card = cards.nth(3).getByRole("button", { name: /町屋駅/ });
    await card.click();
    await expect(card).toHaveAttribute("aria-pressed", "true");
    await expect(pins.nth(3)).toHaveAttribute("data-selected", "true");
    await expect(pins.nth(0)).toHaveAttribute("data-selected", "false");
    // ピンを押すと、そのカードが選ばれる。
    await pins.nth(1).click();
    await expect(cards.nth(1).getByRole("button", { name: /王子駅/ })).toHaveAttribute("aria-pressed", "true");
    // カードの上では ←→ で地点を移る。
    await cards.nth(1).getByRole("button", { name: /王子駅/ }).focus();
    await page.keyboard.press("ArrowRight");
    await expect(pins.nth(2)).toHaveAttribute("data-selected", "true");

    // 外部の地図アプリへのリンクは緯度・経度だけから作る。
    await expect(cards.nth(0).getByRole("link")).toHaveAttribute(
      "href",
      "https://www.google.com/maps/search/?api=1&query=35.7243,139.753",
    );

    if (SHOTS) await sec.screenshot({ path: `${SHOTS}/yui-map.png` });

    // 全画面で開き、Esc で閉じる。
    await sec.getByTestId("yui-map-expand").click();
    const dialog = page.getByRole("dialog", { name: "本駒込駅の周辺の駅" });
    await expect(dialog).toBeVisible();
    await expect(dialog.getByTestId("yui-map-surface")).toHaveAttribute("data-map-status", "ready", { timeout: 30_000 });
    await expect(dialog.getByTestId("yui-map-pin")).toHaveCount(5);
    if (SHOTS) await page.screenshot({ path: `${SHOTS}/yui-map-full.png` });
    await page.keyboard.press("Escape");
    await expect(dialog).toBeHidden();
  });

  test("道順は線で結び、経路を外部の地図で開ける", async ({ page }) => {
    await mockMap(page, "style");
    await page.goto("/reference/yui");
    const sec = page.locator('section[data-testid="yui-map"]');
    await sec.scrollIntoViewIfNeeded();
    await expect(sec.getByTestId("yui-map-surface")).toHaveAttribute("data-map-status", "ready", { timeout: 30_000 });
    await expect(sec.getByText("線は地点を順に結んだもので、実際の道筋ではありません。")).toBeVisible();
    const href = await sec.getByRole("link", { name: "Google マップで経路を見る" }).getAttribute("href");
    const url = new URL(href ?? "");
    expect(url.searchParams.get("origin")).toBe("34.7025,135.4959");
    expect(url.searchParams.get("destination")).toBe("34.6937,135.5023");
    expect(url.searchParams.get("travelmode")).toBe("walking");
  });

  test("地図が無効な構成では、Yui の静止画（位置関係だけ）を描く", async ({ page }) => {
    const requests = await mockMap(page, "disabled");
    await page.goto("/reference/yui");
    const sec = section(page);
    await sec.scrollIntoViewIfNeeded();
    await expect(sec.getByTestId("yui-map-static")).toBeVisible();
    await expect(sec.getByTestId("yui-map-card")).toHaveCount(0);
    expect(requests).toEqual([]);
  });

  test("スタイルを読めないときは静止画に戻し、その旨を添える", async ({ page }) => {
    await mockMap(page, "broken");
    await page.goto("/reference/yui");
    const sec = section(page);
    await sec.scrollIntoViewIfNeeded();
    await expect(sec.getByTestId("yui-map-static")).toBeVisible({ timeout: 30_000 });
    await expect(sec.getByText("地図を読み込めなかったため、位置関係だけを表示しています。")).toBeVisible();
  });
});
