import { test, expect } from "@playwright/test";

import type { Page } from "@playwright/test";

/// generative UI ギャラリー（/reference/genui）の描画検証＋スクショ改善ループ基盤。
/// 認証不要（middleware は /reference を除外）＝バックエンド無しで回せる。
/// `SHOTS_DIR` を渡すとチャート/スタットの PNG を保存する（web/e2e/visual.spec.ts と同方針）。
const SHOTS = process.env.SHOTS_DIR;

const CHART_KINDS = [
  "bar",
  "line",
  "area",
  "combo",
  "pie",
  "donut",
  "scatter",
  "radar",
  "radial_bar",
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

    // 各チャート種が SVG を伴って描画される（data-chart-kind で選択）。
    for (const kind of CHART_KINDS) {
      const chart = page.locator(`[data-chart-kind="${kind}"]`);
      await expect(chart, `chart kind ${kind}`).toBeVisible();
      await expect(chart.locator("svg.recharts-surface").first()).toBeVisible();
    }

    // KPI スタットタイル（値・sparkline）。
    await expect(page.getByTestId("gallery-stat-up")).toBeVisible();
    await expect(page.getByTestId("gallery-stat-up").getByText("¥1.28M")).toBeVisible();

    if (SHOTS) {
      for (const kind of CHART_KINDS) {
        await page
          .locator(`[data-chart-kind="${kind}"]`)
          .screenshot({ path: `${SHOTS}/chart-${kind}.png` });
      }
      for (const id of ["stat-up", "stat-down", "stat-plain"]) {
        await page.getByTestId(`gallery-${id}`).screenshot({ path: `${SHOTS}/${id}.png` });
      }
    }
  });
});
