import { test } from "@playwright/test";

import { loginViaKeycloak } from "./helpers";

/// 視覚確認専用キャプチャ。SHOTS_DIR を設定したときだけ実行する（CI ではスキップ）。
/// 例: SHOTS_DIR=/tmp/shots pnpm exec playwright test visual
const SHOTS = process.env.SHOTS_DIR;

test("主要画面のスクリーンショットを撮る", async ({ page }) => {
  test.skip(!SHOTS, "SHOTS_DIR 未設定（視覚確認専用）");

  // ログイン画面（未認証）。
  await page.goto("/login");
  await page.waitForLoadState("networkidle");
  await page.screenshot({ path: `${SHOTS}/01-login.png`, fullPage: true });

  // ログイン → ホーム（画像1: 中央コンポーザ＋ショートカット）。
  await loginViaKeycloak(page);
  await page.goto("/");
  await page.waitForLoadState("networkidle");
  await page.screenshot({ path: `${SHOTS}/02-home.png`, fullPage: true });

  // 検索モーダル（画像2）。
  await page.keyboard.press("Control+k");
  await page.getByPlaceholder("チャットを検索...").waitFor();
  await page.screenshot({ path: `${SHOTS}/03-search-empty.png` });
  await page.keyboard.press("Escape");

  // ダミーチャット送信 → 会話画面。
  const composer = page.getByPlaceholder("何でも尋ねて、社内文書も検索");
  await composer.fill("四半期の売上レポートを要約して");
  await composer.press("Enter");
  await page.waitForURL(/\/c\//);
  await page.getByText(/権限考慮 RAG と自律エージェント/).waitFor({ timeout: 15_000 });
  await page.screenshot({ path: `${SHOTS}/04-conversation.png`, fullPage: true });

  // 履歴ができた状態の検索モーダル（日付グループ）。
  await page.keyboard.press("Control+k");
  await page.getByPlaceholder("チャットを検索...").waitFor();
  await page.screenshot({ path: `${SHOTS}/05-search-history.png` });

  // ワークフロー（Phase 10）: 一覧 → dnd エディタ → 実行履歴。
  await page.keyboard.press("Escape");
  await page.goto("/workflows");
  await page.waitForLoadState("networkidle");
  await page.screenshot({ path: `${SHOTS}/06-workflows-list.png`, fullPage: true });
  await page.getByRole("button", { name: "新しいワークフロー" }).click();
  await page.waitForURL(/\/workflows\/[0-9a-f-]+$/i, { timeout: 20_000 });
  await page.getByRole("button", { name: "最初のブロック" }).click();
  await page.getByRole("button", { name: /^スクリプト/ }).click();
  await page.waitForTimeout(800);
  await page.screenshot({ path: `${SHOTS}/07-workflow-editor.png` });
  await page.getByRole("button", { name: "保存", exact: true }).click();
  await page.getByText(/保存しました/).waitFor({ timeout: 15_000 });
  await page.getByRole("button", { name: "実行", exact: true }).click();
  await page.getByRole("button", { name: "実行する" }).click();
  await page.waitForURL(/\/runs\?run=/, { timeout: 20_000 });
  await page.waitForTimeout(2500);
  await page.screenshot({ path: `${SHOTS}/08-workflow-runs.png`, fullPage: true });
});
