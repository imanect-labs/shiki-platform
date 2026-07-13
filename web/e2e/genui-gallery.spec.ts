import { test, expect } from "@playwright/test";

import type { Page } from "@playwright/test";

/// generative UI ギャラリー（/reference/genui）の描画検証＋スクショ改善ループ基盤。
/// 認証不要（middleware は /reference を除外）＝バックエンド無しで回せる。
/// `SHOTS_DIR` を渡すとチャート/スタットの PNG を保存する（web/e2e/visual.spec.ts と同方針）。
const SHOTS = process.env.SHOTS_DIR;

/// ギャラリーのチャートセル id（一意）。同一 kind が複数（bar と bar-stacked 等）あるため
/// data-chart-kind ではなくセル id で選択する。
const CHART_CELL_IDS = [
  "bar",
  "bar-stacked",
  "line",
  "area",
  "area-stacked",
  "combo",
  "pie",
  "donut",
  "scatter",
  "scatter-cat",
  "radar",
  "radial",
  "funnel",
  "treemap",
] as const;

/// ResponsiveContainer のサイズ確定後、実際のマークが出るまで待つ。
async function waitForCharts(page: Page) {
  await page.waitForFunction(
    () =>
      document.querySelectorAll(
        ".recharts-surface .recharts-bar-rectangle, .recharts-surface .recharts-curve, .recharts-surface .recharts-sector, .recharts-surface .recharts-radar-polygon, .recharts-surface .recharts-scatter-symbol",
      ).length > 0,
    { timeout: 20_000 },
  );
}

test.describe("generative UI ギャラリー（検証済みスペックの描画）", () => {
  test("全チャート種と KPI スタットタイルが描画される", async ({ page }) => {
    await page.goto("/reference/genui", { waitUntil: "networkidle" });
    await waitForCharts(page);

    // 各チャートセルが recharts の SVG を伴って描画される（セル id で一意選択）。
    for (const id of CHART_CELL_IDS) {
      const cell = page.getByTestId(`gallery-${id}`);
      await expect(cell, `chart cell ${id}`).toBeVisible();
      await expect(cell.locator("svg.recharts-surface").first()).toBeVisible();
    }

    // KPI スタットタイル（値・sparkline）。
    await expect(page.getByTestId("gallery-stat-up")).toBeVisible();
    await expect(page.getByTestId("gallery-stat-up").getByText("¥1.28M")).toBeVisible();

    if (SHOTS) {
      for (const id of CHART_CELL_IDS) {
        await page.getByTestId(`gallery-${id}`).screenshot({ path: `${SHOTS}/chart-${id}.png` });
      }
      for (const id of ["stat-up", "stat-down", "stat-plain"]) {
        await page.getByTestId(`gallery-${id}`).screenshot({ path: `${SHOTS}/${id}.png` });
      }
    }
  });
});
