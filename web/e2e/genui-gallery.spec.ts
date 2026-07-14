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

    // 質問カード（PR4）: Claude Code 風の 1 問 1 ステップ・説明付き選択肢カード。
    const questionCard = page.getByTestId("gallery-question-card");
    await expect(questionCard).toBeVisible();
    // 1 問目（目的・単一選択＋その他）だけが表示され、選択肢はカード（説明付き）。
    await expect(questionCard.getByText("今回の旅行の主な目的は何ですか？")).toBeVisible();
    await expect(questionCard.getByTestId("genui-question-option")).toHaveCount(4); // 3 択＋その他
    await expect(questionCard.getByText("名所や自然、グルメなど旅先を楽しむのが中心")).toBeVisible();
    // スライダーは使わない（数値入力のバーが無いこと）。
    await expect(questionCard.locator('input[type="range"]')).toHaveCount(0);
    // ウィザードの進行（次へ）。1 問ずつなので 2 問目はまだ出ていない。
    await expect(questionCard.getByRole("button", { name: "次へ" })).toBeVisible();
    await expect(questionCard.getByText("旅のペースはどれくらいが好みですか？")).toHaveCount(0);
    // 選択肢を選ぶと aria-checked が立ち、次へで 2 問目へ進む。
    const firstOption = questionCard.getByTestId("genui-question-option").first();
    await firstOption.click();
    await expect(firstOption).toHaveAttribute("aria-checked", "true");
    await questionCard.getByRole("button", { name: "次へ" }).click();
    await expect(questionCard.getByText("旅のペースはどれくらいが好みですか？")).toBeVisible();

    // 地図（PR5）: maplibre キャンバス＋ルート順のマーカーがオフライン既定スタイルで描画される。
    const mapCell = page.getByTestId("gallery-map");
    await expect(mapCell).toBeVisible();
    await expect(mapCell.getByTestId("genui-map-canvas")).toBeVisible();
    await expect(mapCell.locator("canvas.maplibregl-canvas")).toBeVisible();
    // 5 地点のマーカー DOM（番号ピン＋ラベル）が乗る。
    await expect(mapCell.locator(".genui-map-marker")).toHaveCount(5);
    await expect(mapCell.getByText("東京駅")).toBeVisible();

    // ドメインカード（PR6）: 5 種が描画される。
    for (const [id, testId] of [
      ["source_card", "genui-source-card"],
      ["itinerary", "genui-itinerary"],
      ["weather", "genui-weather"],
      ["comparison", "genui-comparison"],
      ["timeline", "genui-timeline"],
    ] as const) {
      await expect(page.getByTestId(`gallery-${id}`).getByTestId(testId), `domain ${id}`).toBeVisible();
    }
    // 代表的な中身: 出典リンク（https・外部）／比較の推し列強調。
    await expect(
      page.getByTestId("gallery-source_card").getByRole("link", { name: /二段 authz/ }),
    ).toHaveAttribute("href", /^https:\/\//);
    await expect(page.getByTestId("gallery-comparison").getByText("¥1,480")).toBeVisible();

    if (SHOTS) {
      // 直前の操作（質問カードのステップ遷移等）を捨て初期状態から撮る。
      await page.reload({ waitUntil: "networkidle" });
      await waitForCharts(page);
      // 地図の maplibre キャンバスとマーカーが乗るまで待つ（オフライン既定＝ネット不要）。
      await page
        .getByTestId("gallery-map")
        .locator(".genui-map-marker")
        .first()
        .waitFor({ timeout: 20_000 });
      // 全コンポーネントをライト/ダーク両方で撮る（デザイン改善ループの棚卸し用）。
      const ALL_CELLS = [
        ...CHART_CELL_IDS,
        "stat-up",
        "stat-down",
        "stat-plain",
        "callout",
        "accordion",
        "tabs",
        "stepper",
        "badge_list",
        "key_value",
        "code_block",
        "rich-form",
        "question-card",
        "map",
        "source_card",
        "itinerary",
        "weather",
        "comparison",
        "timeline",
      ];
      for (const scheme of ["light", "dark"] as const) {
        await page.emulateMedia({ colorScheme: scheme });
        for (const id of ALL_CELLS) {
          await page
            .getByTestId(`gallery-${id}`)
            .screenshot({ path: `${SHOTS}/${scheme}-${id}.png` });
        }
      }
    }
  });
});
