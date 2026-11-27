import { test, expect, type Page, type Route } from "@playwright/test";

/// ```ui の map に背景地図を重ねる部品（design.md §4.7）を、図のギャラリー（/reference/yui）で確かめる。
/// 地図の API（/api/map/*）は差し替える。地図データは中身の無い最小の PMTiles（タイル 0 枚）を
/// テストの中で組み、範囲指定どおりに切り出して返す（配信の形を本物と同じにする）。
/// 認証・バックエンド・LLM 不要。

const ATTRIBUTION = [
  { text: "© OpenStreetMap contributors", url: "https://www.openstreetmap.org/copyright" },
  { text: "© OpenMapTiles", url: "https://openmaptiles.org/" },
];

/// 最小の PMTiles v3（ルートディレクトリ 0 件・メタデータ `{}`・圧縮なし）。
function emptyPmtiles(): Buffer {
  const HEADER = 127;
  const rootDir = Buffer.from([0]); // 項目数 0（varint）
  const metadata = Buffer.from("{}");
  const h = Buffer.alloc(HEADER);
  h.write("PMTiles", 0, "ascii");
  h.writeUInt8(3, 7);
  const u64 = (v: number, at: number) => h.writeBigUInt64LE(BigInt(v), at);
  u64(HEADER, 8); // root dir offset
  u64(rootDir.length, 16);
  u64(HEADER + rootDir.length, 24); // metadata offset
  u64(metadata.length, 32);
  u64(HEADER + rootDir.length + metadata.length, 40); // leaf dirs
  u64(0, 48);
  u64(HEADER + rootDir.length + metadata.length, 56); // tile data
  u64(0, 64);
  u64(0, 72); // addressed tiles
  u64(0, 80); // tile entries
  u64(0, 88); // tile contents
  h.writeUInt8(1, 96); // clustered
  h.writeUInt8(1, 97); // internal compression: none
  h.writeUInt8(1, 98); // tile compression: none
  h.writeUInt8(1, 99); // tile type: mvt
  h.writeUInt8(0, 100); // min zoom
  h.writeUInt8(14, 101); // max zoom
  const i32 = (v: number, at: number) => h.writeInt32LE(Math.round(v * 1e7), at);
  i32(-180, 102);
  i32(-85, 106);
  i32(180, 110);
  i32(85, 114);
  h.writeUInt8(0, 118);
  i32(0, 119);
  i32(0, 123);
  return Buffer.concat([h, rootDir, metadata]);
}

const ARCHIVE = emptyPmtiles();

async function serveArchive(route: Route) {
  const m = /bytes=(\d+)-(\d+)/.exec(route.request().headers()["range"] ?? "");
  if (!m) return route.fulfill({ status: 416, headers: { "content-range": `bytes */${ARCHIVE.length}` } });
  const start = Number(m[1]);
  const end = Math.min(Number(m[2]), ARCHIVE.length - 1);
  return route.fulfill({
    status: 206,
    headers: {
      "content-type": "application/vnd.pmtiles",
      "content-range": `bytes ${start}-${end}/${ARCHIVE.length}`,
      "accept-ranges": "bytes",
      etag: '"test-archive"',
    },
    body: ARCHIVE.subarray(start, end + 1),
  });
}

/// 外へ接続し得るもの（glyphs・sprite・自分以外を指すソースとそのレイヤ）。web が読む前に取り除くこと。
const EXTERNAL_PARTS = {
  glyphs: "https://glyphs.invalid/{fontstack}/{range}.pbf",
  sprite: "https://sprites.invalid/sprite",
  sources: { outside: { type: "vector", url: "pmtiles://https://tiles.invalid/x.pmtiles" } },
  layers: [{ id: "outside", type: "line", source: "outside", "source-layer": "roads" }],
};

async function mockMapApi(
  page: Page,
  available: boolean,
  withExternal = false,
  routing: "off" | "ok" | "no_route" = "off",
) {
  await page.route("**/api/map/config", (route) =>
    route.fulfill({
      json: {
        available,
        style_url: available ? "/map/style.json" : null,
        attribution: ATTRIBUTION,
        routing: routing !== "off",
      },
    }),
  );
  await page.route("**/api/map/route", (route) => {
    if (routing === "no_route") return route.fulfill({ status: 422, json: { code: "no_route" } });
    // ギャラリーの図の道順（大阪駅 → 本社・徒歩）を、途中で折れる線にして返す。
    const body = route.request().postDataJSON() as { mode: string; stops: { lat: number; lng: number }[] };
    const [a, b] = body.stops;
    return route.fulfill({
      json: {
        coordinates: [
          [a!.lng, a!.lat],
          [a!.lng, b!.lat],
          [b!.lng, b!.lat],
        ],
        distance_m: 1240,
        duration_s: 930,
      },
    });
  });
  await page.route("**/api/map/style.json*", (route) =>
    route.fulfill({
      json: {
        version: 8,
        ...(withExternal ? { glyphs: EXTERNAL_PARTS.glyphs, sprite: EXTERNAL_PARTS.sprite } : {}),
        sources: {
          openmaptiles: { type: "vector", url: "pmtiles:///map/basemap.pmtiles" },
          ...(withExternal ? EXTERNAL_PARTS.sources : {}),
        },
        layers: [
          { id: "background", type: "background", paint: { "background-color": "#efece6" } },
          { id: "water", type: "fill", source: "openmaptiles", "source-layer": "water", paint: { "fill-color": "#bcd5e6" } },
          ...(withExternal ? EXTERNAL_PARTS.layers : []),
        ],
      },
    }),
  );
  await page.route("**/api/map/basemap.pmtiles", serveArchive);
}

test.describe("背景地図（map の renderMap）", () => {
  test.use({ viewport: { width: 1280, height: 900 } });

  test("地図データが無ければ Yui の背景なしの図を描く", async ({ page }) => {
    await mockMapApi(page, false);
    await page.goto("/reference/yui");
    const section = page.getByTestId("yui-map");
    await section.scrollIntoViewIfNeeded();
    await expect(section.locator("[data-yui=map]")).toBeVisible({ timeout: 30_000 });
    await expect(section.getByTestId("basemap-figure")).toHaveCount(0);
  });

  test("地図データがあれば背景地図に番号付きのピンを重ね、shiki 以外へ接続しない", async ({ page, baseURL }) => {
    const origin = new URL(baseURL ?? "http://localhost:3000").origin;
    const outside: string[] = [];
    page.on("request", (req) => {
      const u = req.url();
      if (u.startsWith("data:") || u.startsWith("blob:")) return;
      if (new URL(u).origin !== origin) outside.push(u);
    });
    await mockMapApi(page, true);
    await page.goto("/reference/yui");
    const section = page.getByTestId("yui-map");
    await section.scrollIntoViewIfNeeded();
    const figure = section.getByTestId("basemap-figure");
    await expect(figure).toHaveAttribute("data-state", "ready", { timeout: 30_000 });
    await expect(figure.locator("canvas.maplibregl-canvas")).toBeVisible();
    // 地図が枠いっぱいに広がっている（MapLibre の position: relative で高さ 0 に潰れない）。
    const canvasBox = await figure.locator("canvas.maplibregl-canvas").boundingBox();
    expect(canvasBox?.height ?? 0).toBeGreaterThan(250);
    // 出典は常に出ている。
    const attribution = figure.getByTestId("basemap-attribution");
    await expect(attribution).toContainText("© OpenStreetMap contributors");
    await expect(attribution).toContainText("© OpenMapTiles");
    // 地点の一覧（番号・名前）と操作ボタン。
    await expect(figure.getByTestId("basemap-legend").getByRole("listitem")).toHaveCount(3);
    await expect(figure.getByTestId("basemap-legend").getByRole("listitem").first()).toContainText("大阪駅");
    // 地図の上の札（名前）が地点の数だけある。
    await expect(figure.getByTestId("basemap-tag")).toHaveCount(3);
    for (const name of ["拡大", "縮小", "全地点を表示", "全画面で表示"]) {
      await expect(figure.getByRole("button", { name, exact: true })).toBeEnabled();
    }
    // 一覧から地点を選ぶとカードが開く（名前は文字として入る）。
    await figure.getByTestId("basemap-legend").getByRole("listitem").nth(2).getByRole("button").click();
    const card = figure.getByTestId("basemap-card");
    await expect(card).toContainText("本社");
    await expect(card).toContainText("10:00 から打ち合わせ");
    // 地図の上の札に乗せると、その地点のカードに替わる（一覧から外れても選んだ地点に戻る）。
    await figure.getByTestId("basemap-tag").first().hover();
    await expect(card).toContainText("大阪駅");
    await expect(card).toContainText("集合場所");

    // 地図の上でホイールを回してもページがスクロールする（地図はスクロールを奪わない）。
    const box = await figure.getByTestId("basemap-canvas").boundingBox();
    expect(box).not.toBeNull();
    const before = await page.evaluate(() => window.scrollY);
    await page.mouse.move(box!.x + box!.width / 2, box!.y + box!.height / 2);
    await page.mouse.wheel(0, 400);
    await expect.poll(() => page.evaluate(() => window.scrollY)).toBeGreaterThan(before);

    expect(outside).toEqual([]);
  });

  test("スタイルに外部の URL が混ざっていても、読まずに取り除いて描く", async ({ page, baseURL }) => {
    const origin = new URL(baseURL ?? "http://localhost:3000").origin;
    const outside: string[] = [];
    page.on("request", (req) => {
      const u = req.url();
      if (!u.startsWith("data:") && !u.startsWith("blob:") && new URL(u).origin !== origin) outside.push(u);
    });
    await mockMapApi(page, true, true);
    await page.goto("/reference/yui");
    const section = page.getByTestId("yui-map");
    await section.scrollIntoViewIfNeeded();
    await expect(section.getByTestId("basemap-figure")).toHaveAttribute("data-state", "ready", { timeout: 30_000 });
    expect(outside).toEqual([]);
  });

  test("道路沿いの道順が取れたら線を差し替え、距離と時間を出す", async ({ page }) => {
    const requests: string[] = [];
    page.on("request", (r) => {
      if (r.url().includes("/api/map/route")) requests.push(r.postData() ?? "");
    });
    await mockMapApi(page, true, false, "ok");
    await page.goto("/reference/yui");
    const section = page.getByTestId("yui-map");
    await section.scrollIntoViewIfNeeded();
    const figure = section.getByTestId("basemap-figure");
    await expect(figure).toHaveAttribute("data-state", "ready", { timeout: 30_000 });
    await expect(figure.getByTestId("basemap-route")).toHaveText("道順: 徒歩 1.2 km・約 16 分（道路沿い）");
    await expect(figure).toHaveAttribute("data-route", "road");
    // 徒歩の道順の地点（大阪駅 → 本社）を順に送る。
    expect(requests).toHaveLength(1);
    const sent = JSON.parse(requests[0]!) as { mode: string; stops: { lat: number; lng: number }[] };
    expect(sent.mode).toBe("walk");
    expect(sent.stops).toEqual([
      { lat: 34.7025, lng: 135.4959 },
      { lat: 34.6937, lng: 135.5023 },
    ]);
  });

  test("道順を出せないときは直線のまま、その旨を出す", async ({ page }) => {
    await mockMapApi(page, true, false, "no_route");
    await page.goto("/reference/yui");
    const section = page.getByTestId("yui-map");
    await section.scrollIntoViewIfNeeded();
    const figure = section.getByTestId("basemap-figure");
    await expect(figure).toHaveAttribute("data-state", "ready", { timeout: 30_000 });
    await expect(figure.getByTestId("basemap-route")).toHaveText(
      "道路沿いの道順を出せないため、地点を直線で結んでいます",
    );
    await expect(figure).toHaveAttribute("data-route", "straight");
  });

  test("画面から離れた地図は破棄し、戻ると作り直す", async ({ page }) => {
    await mockMapApi(page, true);
    await page.goto("/reference/yui");
    const section = page.getByTestId("yui-map");
    await section.scrollIntoViewIfNeeded();
    const figure = section.getByTestId("basemap-figure");
    await expect(figure).toHaveAttribute("data-state", "ready", { timeout: 30_000 });
    await page.evaluate(() => window.scrollTo(0, document.body.scrollHeight));
    await expect(figure.locator("canvas.maplibregl-canvas")).toHaveCount(0, { timeout: 10_000 });
    await expect(figure).toHaveAttribute("data-state", "idle");
    await section.scrollIntoViewIfNeeded();
    await expect(figure).toHaveAttribute("data-state", "ready", { timeout: 30_000 });
  });
});
