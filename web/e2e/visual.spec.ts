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
  const composer = page.getByPlaceholder("何でも尋ねて、何でも作成");
  await composer.fill("四半期の売上レポートを要約して");
  await composer.press("Enter");
  await page.waitForURL(/\/c\//);
  await page.getByText(/権限考慮 RAG と自律エージェント/).waitFor({ timeout: 15_000 });
  await page.screenshot({ path: `${SHOTS}/04-conversation.png`, fullPage: true });

  // 履歴ができた状態の検索モーダル（日付グループ）。
  await page.keyboard.press("Control+k");
  await page.getByPlaceholder("チャットを検索...").waitFor();
  await page.screenshot({ path: `${SHOTS}/05-search-history.png` });
});
