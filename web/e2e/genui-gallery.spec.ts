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

    // レイアウト/コンテンツ基盤（PR2）。
    for (const id of [
      "callout",
      "accordion",
      "tabs",
      "stepper",
      "badge_list",
      "key_value",
      "code_block",
    ]) {
      await expect(page.getByTestId(`gallery-${id}`), `layout ${id}`).toBeVisible();
    }
    // 代表的な中身の描画確認（callout の 4 トーン・code_block・accordion 展開）。
    await expect(page.getByTestId("gallery-callout").getByTestId("genui-callout")).toHaveCount(4);
    await expect(page.getByTestId("gallery-code_block").locator("code")).toBeVisible();

    // リッチ入力フォーム（PR3）: 各フィールド種が描画される。
    const richForm = page.getByTestId("gallery-rich-form").getByTestId("genui-form-survey");
    await expect(richForm).toBeVisible();
    await expect(richForm.locator('input[type="checkbox"]')).toHaveCount(4); // 3 選択肢＋その他
    await expect(richForm.locator('input[type="radio"]')).toHaveCount(2);
    await expect(richForm.locator('input[type="range"]')).toBeVisible();
    await expect(richForm.locator('input[type="date"]')).toHaveCount(3); // 開始日＋期間(開始/終了)
    await expect(richForm.getByRole("radio", { name: "4" })).toBeVisible(); // rating の星

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
