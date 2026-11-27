/// インストール済みアプリ（コード）のカードが、アプリの状態を言い分ける（BUG-201・BUG-203・GAP-60）。
///
/// - 管理者が Keycloak で B2 の登録を止めたアプリ（`oauth_client_disabled`）: サーバーで動く処理が止まっていると言い、
///   入れ直しは勧めない（入れ直すと同じ secret で戻る）。
/// - 自動実行が失敗しているアプリ（`function_failures`・持ち主にだけ届く）: 関数・きっかけ・日時・続けて失敗した回数と
///   理由を出す。
/// - 画面を持たないアプリ: 「開く」を押せない（Button asChild の Link には disabled が効かなかった）。
///
/// どの状態も Keycloak の操作やサーバーの自動実行を待たないと作れないので、インストールの一覧の応答を差し替えて、
/// 画面の出し方だけを見る（印の付け方・失敗の残し方・持ち主にだけ返すことは、Rust の結合テスト install_it と
/// miniapp_b2_tests が実 Keycloak・実 OpenFGA で確かめる）。

import { expect, test, type Route } from "@playwright/test";

import { loginViaKeycloak } from "./helpers";

const SHOTS = process.env.E2E_SHOTS_DIR;

type Extra = Record<string, unknown>;

function installation(id: string, name: string, extra: Extra) {
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
    server_spec: { functions: ["weekly"] },
    oauth_client_missing: false,
    oauth_client_disabled: false,
    table_count: 0,
    ...extra,
  };
}

test("インストール済みアプリ: 止められた登録・自動実行の失敗・画面のないアプリを、カードで言い分ける", async ({ page }) => {
  await loginViaKeycloak(page);
  const disabled = installation("00000000-0000-4000-8000-000000000711", "経費の締め（止められた）", {
    oauth_client_disabled: true,
  });
  const failing = installation("00000000-0000-4000-8000-000000000712", "日報の集計（自動実行が失敗）", {
    function_failures: [
      {
        function: "weekly",
        trigger: "cron",
        code: "script_error",
        message: "集計するテーブルが空です",
        failed_at: "2026-11-02T00:00:00Z",
        first_failed_at: "2026-10-31T00:00:00Z",
        failures: 3,
      },
    ],
  });
  const withScreen = installation("00000000-0000-4000-8000-000000000713", "備品の貸出", {
    frontend_bundle: "1".repeat(64),
  });
  const list = async (route: Route) => {
    if (route.request().method() !== "GET") return route.fallback();
    return route.fulfill({
      status: 200,
      contentType: "application/json",
      body: JSON.stringify({ items: [disabled, failing, withScreen] }),
    });
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
      const card = (name: string) =>
        page.getByRole("listitem").filter({ has: page.getByRole("heading", { name }) });

      // 止められた登録: サーバーで動く処理が止まっていると言い、入れ直しは勧めない（BUG-201）。
      const stopped = card(disabled.app_name).getByTestId("app-client-disabled");
      await expect(stopped).toContainText("このアプリのサーバーで動く処理が止まっています");
      await expect(stopped).toContainText("管理者に確かめてください。");
      await expect(stopped).not.toContainText("入れ直");

      // 自動実行の失敗: 関数・きっかけ・続けて失敗した回数と理由（GAP-60）。内部の言葉（コード）は出さない。
      const failed = card(failing.app_name).getByTestId("app-function-failures");
      await expect(failed).toContainText("自動実行が失敗しています");
      await expect(failed).toContainText("「weekly」（スケジュール）");
      await expect(failed).toContainText("3 回続けて失敗");
      await expect(failed).toContainText("集計するテーブルが空です");
      await expect(failed).not.toContainText("script_error");
      await expect(card(withScreen.app_name).getByRole("status")).toHaveCount(0);

      // 画面のないアプリは「開く」を押せない（リンクにしない）。画面のあるアプリは開ける（BUG-203）。
      for (const app of [disabled, failing]) {
        const open = card(app.app_name).getByRole("button", { name: "開く" });
        await expect(open).toBeDisabled();
        await expect(open).toHaveAccessibleDescription("画面のないアプリです");
        await expect(card(app.app_name).getByRole("link", { name: "開く" })).toHaveCount(0);
      }
      await expect(card(withScreen.app_name).getByRole("link", { name: "開く" })).toHaveAttribute(
        "href",
        `/apps/b1/${withScreen.app_id}`,
      );

      const overflow = await page.evaluate(() => document.documentElement.scrollWidth - window.innerWidth);
      expect(overflow).toBeLessThanOrEqual(1);
      if (SHOTS) {
        await card(failing.app_name).scrollIntoViewIfNeeded();
        await page.screenshot({ path: `${SHOTS}/miniapp-card-states-${label}.png`, fullPage: true });
      }
    }
  } finally {
    await page.unroute(/\/api\/apps\/installations$/, list);
    await page.unroute(/\/api\/apps\/registry$/, registry);
  }
});
