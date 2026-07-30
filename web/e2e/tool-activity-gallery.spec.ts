import { test, expect } from "@playwright/test";

/// ツール実行表示ギャラリー（/reference/tool-activity）の描画検証（issue #386）。
/// 認証・バックエンド・LLM 不要（middleware は /reference を除外）＝ web 単体で回せる。
///
/// **実行中（ローリング）は実チャットでは検証できない**（stub LLM はミリ秒で完了し、
/// 実 LLM はタイミングが非決定的）。固定フィクスチャのギャラリーが唯一の回帰点。
/// `SHOTS_DIR` を渡すと各状態の PNG を保存する（genui-gallery.spec.ts と同方針）。
const SHOTS = process.env.SHOTS_DIR;

test.describe("ツール実行表示ギャラリー", () => {
  test.beforeEach(async ({ page }) => {
    await page.goto("/reference/tool-activity");
  });

  test("実行中はフェーズ行＋直近 3 件のローリングを出す", async ({ page }) => {
    const running = page.getByTestId("tool-activity-running");
    await expect(running).toBeVisible();
    // フィクスチャは 5 件だが、折りたたみ時に見えるのは直近 3 件だけ。
    await expect(running.getByText(/を閲覧中/)).toHaveCount(3);
    // フェーズ行はツールの種別から導く（旧実装の日本語プレフィックス一致に依存しない）。
    await expect(running.getByText("ページを読んでいます")).toBeVisible();
    if (SHOTS) await running.screenshot({ path: `${SHOTS}/tool-activity-running.png` });
  });

  test("計画のサブタスクがあればフェーズ行に優先して出る", async ({ page }) => {
    const withPlan = page.getByTestId("tool-activity-running-with-plan");
    await expect(withPlan.getByText("市場規模と成長率を調べています")).toBeVisible();
  });

  test("完了後は 1 行要約に畳み、展開で全件・並行・成否・結果要約を出す", async ({ page }) => {
    const done = page.getByTestId("tool-activity-done");
    // 折りたたみ時は件数と内訳だけ。
    await expect(done.getByText("5 件の操作")).toBeVisible();
    await expect(done.getByText(/web 3/)).toBeVisible();
    if (SHOTS) await done.screenshot({ path: `${SHOTS}/tool-activity-collapsed.png` });

    await done.getByRole("button").first().click();
    // 同一ステップの並列実行がまとまって見える（backend の tool_call.step 由来）。
    await expect(done.getByText("並行して 2 件")).toBeVisible();
    // 対象込みの具体ラベル（URL・ファイル名）。
    await expect(done.getByText(/nikkei\.com\/article\/.* を閲覧しました/)).toBeVisible();
    await expect(done.getByText("notes.md に書き込みました")).toBeVisible();
    // 失敗は成功と区別され、完了形にしない（#358/#386）。
    await expect(done.getByText(/example\.invalid.* を閲覧できませんでした/)).toBeVisible();
    // ツール結果の要約が出る（office 編集の適用件数・保存版など・#358）。
    await expect(done.getByText(/1\/1 件適用・新バージョン v12/)).toBeVisible();
    if (SHOTS) await done.screenshot({ path: `${SHOTS}/tool-activity-expanded.png` });
  });

  test("全ツール語彙に日本語ラベルがある（生の英識別子を出さない）", async ({ page }) => {
    const vocab = page.getByTestId("tool-activity-vocab");
    await vocab.getByRole("button").first().click();
    // 語彙の網羅自体は Record<ToolName, …> が型で保証する。ここでは
    // 「ツール名がそのまま出ていない」ことを代表例で確認する。
    for (const raw of ["office.live_edit", "save_document", "csv.query", "doc_search"]) {
      await expect(vocab.getByText(raw, { exact: false })).toHaveCount(0);
    }
    await expect(vocab.getByText("Office ファイルをライブ編集しました")).toBeVisible();
    await expect(vocab.getByText("「報告書」を Word で作成しました")).toBeVisible();
    if (SHOTS) await vocab.screenshot({ path: `${SHOTS}/tool-activity-vocab.png` });
  });
});
