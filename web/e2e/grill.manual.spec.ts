import { test, expect } from "@playwright/test";

import type { components } from "@/generated/api";

import { loginViaKeycloak } from "./helpers";

/// first-party skill `grilling`（#428）の起動導線を実機で確認する。
///
/// **手動実行専用**（署名 import 済みの環境が要る＝CI の前提にできない）。`sdk/cli` の
/// `skill import-first-party` を通していない環境では skip する。実行手順:
///
/// ```bash
/// # 1. 管理者が信頼鍵を登録し、バンドルを署名 import する（インストール操作はしない）
/// SHIKI_COOKIE='shiki_session=...; shiki_csrf=...' SHIKI_SIGNING_KEY=<hex> \
///   node sdk/cli/src/index.ts skill import-first-party --api http://localhost:3000
/// # 2. 起動済みの web に対して実行する
/// E2E_BASE_URL=http://localhost:3000 pnpm exec playwright test e2e/grill.manual.spec.ts
/// ```
///
/// 見るのは「**インストールせずに** `/grill` が補完へ出て、宣言した 2 variant がそのまま
/// 並ぶか」。first-party は publish 時点で同一 tenant＋org のカタログに載る（#387）ので、
/// ここが崩れると「公式スキルが最初から在る」前提が壊れる。
const SHOTS = process.env.SHOTS_DIR;

test.use({ deviceScaleFactor: 2, locale: "ja-JP", viewport: { width: 1280, height: 900 } });

test("インストールなしで /grill が補完に出て確定できる", async ({ page }) => {
  await loginViaKeycloak(page);
  await page.goto("/");

  // 署名 import されていない環境では前提が無いので skip する（失敗にしない）。
  const listed = await page.evaluate(async () => {
    const res = await fetch("/api/skills/catalog", { credentials: "include" });
    if (!res.ok) return false;
    const body = (await res.json()) as components["schemas"]["SkillCatalogResponse"];
    return body.skills.some((s) => s.name === "grilling");
  });
  test.skip(!listed, "grilling が未 import（sdk/cli の import-first-party を先に通すこと）");

  const input = page.getByLabel("メッセージを入力");
  await input.fill("/grill");

  const menu = page.getByTestId("slash-command-menu");
  await expect(menu).toBeVisible({ timeout: 10_000 });
  await expect(page.getByTestId("slash-command-option")).toHaveCount(2);
  await expect(menu).toContainText("共通理解に達するまで詰める（推奨）");
  await expect(menu).toContainText("1 ラウンドだけ聞いて切り上げる");
  if (SHOTS) await page.screenshot({ path: `${SHOTS}/grill-slash-menu.png` });

  // Enter で確定するとピルになり、command.hint がプレースホルダに出る。
  await input.press("Enter");
  const pill = page.getByTestId("slash-command-pill");
  await expect(pill).toBeVisible();
  await expect(pill).toContainText("/grill");
  await expect(page.getByPlaceholder("詰めたい計画・決定・アイデアを入力")).toBeVisible();
  if (SHOTS) await page.screenshot({ path: `${SHOTS}/grill-slash-pill.png` });
});
