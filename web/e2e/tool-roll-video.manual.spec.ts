import { test } from "@playwright/test";

/// ローリング表示の**動き**を録る手動スペック（issue #386）。
///
/// 参照 UI と並べて見比べるためのもので、判定はしない（動きの良し悪しは目で見るしかない）。
/// `/reference/tool-activity` の再生デモを回すだけなので、バックエンドも LLM も要らない。
///
/// ```bash
/// RECORD=1 E2E_BASE_URL=http://localhost:10386 \
///   pnpm exec playwright test e2e/tool-roll-video.manual.spec.ts
/// ```
test.use({
  deviceScaleFactor: 2,
  locale: "ja-JP",
  viewport: { width: 900, height: 620 },
  video: { mode: "on", size: { width: 900, height: 620 } },
});
test.skip(process.env.RECORD !== "1", "録画は手動（RECORD=1 で実行）");

test("ツール実行のロール（参照 UI との比較用）", async ({ page }) => {
  await page.goto("/reference/tool-activity");
  const play = page.getByRole("button", { name: "再生" });
  await play.scrollIntoViewIfNeeded();
  // 見出しごと入るよう、デモの少し上を画面の上端に置く。
  await page.mouse.wheel(0, -140);
  await play.click();
  // 8 件を 1.2 秒間隔で 2 周ぶん。送りとスケルトン → 事実行の入れ替わりが両方見える。
  await page.waitForTimeout(22_000);
  console.log(`=== VIDEO === ${await page.video()?.path()}`);
});
