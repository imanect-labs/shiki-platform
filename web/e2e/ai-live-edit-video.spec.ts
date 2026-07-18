import { expect, test } from "@playwright/test";
import { loginViaKeycloak } from "./helpers";

/// 「AI が本物のパイプラインで編集する様子」の動画（AI_LIVE=1）。
/// stub LLM の生成結果をモックし、document.edit ツールを**実際に実行**して Yjs 経由で
/// 本文がライブに書き換わるところを撮る（フロントの再現ではない）。
test.skip(process.env.AI_LIVE !== "1", "動画撮影専用");
test.use({
  locale: "ja-JP",
  viewport: { width: 1360, height: 860 },
  video: { mode: "on", size: { width: 1360, height: 860 } },
});
const OUT =
  "/tmp/claude-1000/-home-shuya--agent-start-worktrees-cc-shiki-platform-1784147848/dc5bfedf-88f0-4306-bdc1-83d5a303bd32/scratchpad";
const beat = (p: import("@playwright/test").Page, ms = 800) => p.waitForTimeout(ms);

test("note: AI が document.edit で本文を編集（本物のパイプライン）", async ({ page }) => {
  await loginViaKeycloak(page);
  await page.goto("/drive");
  await page.getByRole("button", { name: "新規作成" }).click();
  await page.getByTestId("new-note").click();
  await page.waitForURL(/\/notes\//, { timeout: 30_000 });
  const editor = page.locator(".tiptap").first();
  await editor.click();
  await page.keyboard.type("# 週次レポート\n\n売上は好調に推移。西日本エリアは横ばいで課題。\n", { delay: 16 });
  await beat(page, 1000);

  // 「AI に依頼」でパネルを開く。
  await page.getByTestId("note-ask-ai").click();
  await beat(page, 800);
  // 本文の一部を選択（自動でチャットへ挿入され node_id が渡る）。
  await editor.getByText("西日本エリアは横ばい", { exact: false }).click({ clickCount: 3 });
  await beat(page, 800);
  await expect(page.getByTestId("selection-chip")).toBeVisible({ timeout: 10_000 });

  // 依頼を送信 → stub が document.edit を実行 → 本文がライブで書き換わる。
  const input = page.getByPlaceholder(/尋ねて|指示|メッセージ/).last();
  await input.click();
  await page.keyboard.type("この内容を、要点を整理して見出し付きで追記して", { delay: 40 });
  await beat(page, 600);
  await input.press("Enter");

  // document.edit は破壊的なので human-in-the-loop の承認カードが出る（要確認ツールの設計意図）。
  const approve = page.getByRole("button", { name: "承認して続行" });
  await expect(approve).toBeVisible({ timeout: 20_000 });
  await beat(page, 1600); // 承認カード（ツール名・引数プレビュー）が動画に映る間。
  await approve.click();

  // AI 編集が Yjs 経由で本文へ反映される（見出し「サマリー」が現れる）。
  await expect(editor.getByRole("heading", { name: "サマリー" })).toBeVisible({ timeout: 25_000 });
  await beat(page, 3200);
  await page.screenshot({ path: `${OUT}/ailive-note.png` });
});
