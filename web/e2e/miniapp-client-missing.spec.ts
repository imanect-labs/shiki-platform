/// インストール済みアプリ（コード）のカードが、アプリの登録が無く動かない状態を知らせる（GAP-70）。
///
/// サーバーは起動時の照合で、Keycloak に無くなったアプリの登録を作り直す。作り直せない（保管した secret を読めない
/// など）ときは、インストールに `oauth_client_missing` の印を付ける。その状態は Keycloak のクライアントを消してから
/// サーバーを起動し直さないと作れないので、ここではインストールの一覧の応答を差し替えて、画面の出し方だけを見る
/// （印の付け方と一覧の応答の項目は、Rust の結合テスト install_it が実 Keycloak で確かめる）。

import { expect, test, type Route } from "@playwright/test";

import { loginViaKeycloak } from "./helpers";

const SHOTS = process.env.E2E_SHOTS_DIR;

function installation(id: string, name: string, missing: boolean) {
  return {
    id,
    app_id: id,
    app_name: name,
    installed_version: "1.0.0",
    granted_scopes: ["data.read", "data.write"],
    client_id_b1: `app-${id}-b1`,
    client_id_b2: `app-${id}-b2`,
    installed_by: "alice",
    created_at: "2026-11-01T00:00:00Z",
    ai: { budget_models: [], budget_daily_usd_micros: null, budget_max_tokens: null, agent_tools: [] },
    frontend_bundle: null,
    server_bundle: "0".repeat(64),
    server_spec: { functions: ["remind"] },
    oauth_client_missing: missing,
  };
}

test("インストール済みアプリ: 登録が見つからないアプリのカードは、動かない状態だと言う（GAP-70）", async ({ page }) => {
  await loginViaKeycloak(page);
  const missing = installation("00000000-0000-4000-8000-000000000701", "経費の締め（登録なし）", true);
  const healthy = installation("00000000-0000-4000-8000-000000000702", "備品の貸出", false);
  const list = async (route: Route) => {
    if (route.request().method() !== "GET") return route.fallback();
    return route.fulfill({ status: 200, contentType: "application/json", body: JSON.stringify({ items: [missing, healthy] }) });
  };
  const registry = async (route: Route) => {
    if (route.request().method() !== "GET") return route.fallback();
    return route.fulfill({ status: 200, contentType: "application/json", body: JSON.stringify({ items: [] }) });
  };
  await page.route(/\/api\/apps\/installations$/, list);
  await page.route(/\/api\/apps\/registry$/, registry);
  try {
    for (const [width, height, label] of [
      [1440, 900, "pc"],
      [390, 844, "phone"],
    ] as const) {
      await page.setViewportSize({ width, height });
      await page.goto("/apps");
      await expect(page.getByRole("heading", { name: "インストール済みアプリ（コード）" })).toBeVisible({ timeout: 20_000 });
      const card = page.getByRole("listitem").filter({ has: page.getByRole("heading", { name: missing.app_name }) });
      const notice = card.getByRole("status");
      await expect(notice).toContainText("このアプリは動かない状態です");
      await expect(notice).toContainText("入れ直すと直りますが、これまでのレコードは使えなくなります。");
      // 印の無いアプリのカードには出さない。
      const other = page.getByRole("listitem").filter({ has: page.getByRole("heading", { name: healthy.app_name }) });
      await expect(other).toBeVisible();
      await expect(other.getByRole("status")).toHaveCount(0);
      // 入れ直し（アンインストール）の操作はカードに残る。
      await expect(card.getByRole("button", { name: `${missing.app_name} をアンインストール` })).toBeVisible();
      const overflow = await page.evaluate(() => document.documentElement.scrollWidth - window.innerWidth);
      expect(overflow).toBeLessThanOrEqual(1);
      if (SHOTS) {
        await card.scrollIntoViewIfNeeded();
        await page.screenshot({ path: `${SHOTS}/miniapp-client-missing-${label}.png`, fullPage: true });
      }
    }
  } finally {
    await page.unroute(/\/api\/apps\/installations$/, list);
    await page.unroute(/\/api\/apps\/registry$/, registry);
  }
});
