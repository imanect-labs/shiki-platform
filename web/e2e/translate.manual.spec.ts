import { test, expect } from "@playwright/test";

import type { components } from "@/generated/api";

import { loginViaKeycloak } from "./helpers";

/// first-party skill `translate`（web_translate・docs/agent/web-translate.md）を実機で確認する。
///
/// **手動実行専用**（実 LLM と外部ページへの取得、署名 import 済みの環境が要る＝CI の前提にできない）。
/// `translate` が未 import の環境では skip する。実行手順:
///
/// ```bash
/// # 1. shiki-server を実 LLM で起動する（例: OpenAI 互換のプロバイダ）
/// SHIKI__LLM__BACKEND=openai SHIKI__LLM__BASE_URL=... SHIKI__LLM__DEFAULT_MODEL=... \
///   .claude/skills/pr/scripts/dev-up.sh
/// # 2. 管理者が信頼鍵を登録し、バンドルを署名 import する
/// SHIKI_COOKIE='shiki_session=...; shiki_csrf=...' SHIKI_SIGNING_KEY=<hex> \
///   node sdk/cli/src/index.ts skill import-first-party --api http://localhost:3000
/// # 3. 訳すページを渡して実行する（既定は Go のブログ記事）
/// TRANSLATE_URL=https://go.dev/blog/range-functions SHOTS_DIR=test-results \
///   E2E_BASE_URL=http://localhost:3000 pnpm exec playwright test e2e/translate.manual.spec.ts
/// ```
///
/// 見るのは「`/translate <URL>` で訳文が会話に出て、モデルの回答は訳文を書き写さない短い案内に留まるか」と、
/// 訳文のコード・リンク・画像が原文どおりに残るか。
const SHOTS = process.env.SHOTS_DIR;
const PAGE_URL = process.env.TRANSLATE_URL ?? "https://go.dev/blog/range-functions";
const THEME = (process.env.THEME ?? "light") as "light" | "dark";

test.use({ deviceScaleFactor: 2, locale: "ja-JP", viewport: { width: 1280, height: 1000 } });
// 実 LLM で数十チャンクを訳すので長めに待つ。
test.setTimeout(15 * 60_000);

test("/translate <URL> で訳文が会話に出る", async ({ page }) => {
  await page.emulateMedia({ colorScheme: THEME });
  await loginViaKeycloak(page);
  await page.goto("/");

  const listed = await page.evaluate(async () => {
    const res = await fetch("/api/skills/catalog", { credentials: "include" });
    if (!res.ok) return false;
    const body = (await res.json()) as components["schemas"]["SkillCatalogResponse"];
    return body.skills.some((s) => s.name === "translate");
  });
  test.skip(!listed, "translate が未 import（sdk/cli の import-first-party を先に通すこと）");

  const input = page.getByLabel("メッセージを入力");
  await input.fill("/translate");
  await expect(page.getByTestId("slash-command-menu")).toBeVisible({ timeout: 10_000 });
  await input.press("Enter");
  await expect(page.getByTestId("slash-command-pill")).toContainText("/translate");
  await input.fill(PAGE_URL);
  if (SHOTS) await page.screenshot({ path: `${SHOTS}/translate-${THEME}-input.png` });
  await page.getByRole("button", { name: "送信" }).click();

  const card = page.getByTestId("translation-block");
  await expect(card).toBeVisible({ timeout: 14 * 60_000 });
  await expect(page.getByTestId("translation-source")).toHaveText(new URL(PAGE_URL).host);
  await expect(page.getByTestId("translation-body")).not.toBeEmpty();
  // 生成が終わるまで待つ（停止ボタンが消える）。
  await expect(page.getByRole("button", { name: "生成を停止" })).toHaveCount(0, { timeout: 5 * 60_000 });
  if (SHOTS) {
    await card.scrollIntoViewIfNeeded();
    await page.screenshot({ path: `${SHOTS}/translate-${THEME}-answer.png` });
  }
});
