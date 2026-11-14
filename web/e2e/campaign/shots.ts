import path from "node:path";

import { expect, type Page } from "@playwright/test";

/// 画面を撮る（`E2E_SHOTS_DIR` を渡したときだけ）と、スマホ幅で横にはみ出さないことの確かめ。
/// 判定用のモデルのシナリオの画面（ワークフローの判定のノード・実行履歴・受信箱）は decide-scenarios.ts の
/// shootViews をそのまま使う（ここから再び出す・あちらは変えない）。

export { shootViews } from "../decide-scenarios";

export const SHOTS = process.env.E2E_SHOTS_DIR;

/// `E2E_SHOTS_DIR` があれば `<prefix>-<name>.png` で撮る。
export async function shot(page: Page, prefix: string, name: string): Promise<void> {
  if (!SHOTS) return;
  await page.screenshot({ path: path.join(SHOTS, `${prefix}-${name}.png`), fullPage: true });
}

/// 横にはみ出さない（スマホ幅の確かめ）。
export async function noSideScroll(page: Page): Promise<void> {
  expect(await page.evaluate(() => document.documentElement.scrollWidth <= window.innerWidth + 1), "横にはみ出さない").toBe(true);
}
