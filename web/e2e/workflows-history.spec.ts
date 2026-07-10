import { expect, test, type Page } from "@playwright/test";

import { loginViaKeycloak, uniqueName } from "./helpers";

/// 実行履歴 UI の e2e（Task 10.14 DoD）:
/// 失敗する run → 詳細シートで失敗ステップ強調＋エラー内容 → 状態フィルタ →
/// 「失敗したところから再開」→ ライブ更新（失敗 → 実行中/順番待ち → 再失敗・試行 2 回目）。

async function createFailingFlow(page: Page, name: string): Promise<string> {
  const cookies = await page.context().cookies();
  const csrf = cookies.find((c) => c.name === "shiki_csrf")?.value ?? "";
  const res = await page.request.post("/api/workflows", {
    headers: { "x-csrf-token": csrf, "content-type": "application/json" },
    data: {
      ir: {
        ir_version: 1,
        name,
        display_name: "失敗フロー",
        triggers: [{ kind: "interactive" }],
        nodes: [
          {
            id: "boom",
            type: "script.run",
            label: "爆発する",
            params: {
              source: { inline: "function main(){ throw new Error('boom-e2e'); }" },
            },
          },
        ],
        edges: [],
      },
    },
  });
  expect(res.ok()).toBeTruthy();
  const body = (await res.json()) as { id: string };
  return body.id;
}

test("実行履歴: 失敗詳細→フィルタ→失敗ステップから再開→ライブ更新", async ({ page }) => {
  test.setTimeout(240_000);
  await loginViaKeycloak(page);
  const id = await createFailingFlow(page, uniqueName("history-e2e"));

  // エディタから実行 → 実行履歴の deep-link へ。
  await page.goto(`/workflows/${id}`);
  await page.getByRole("button", { name: "実行", exact: true }).click();
  await page.getByRole("button", { name: "実行する" }).click();
  await page.waitForURL(/\/runs\?run=/, { timeout: 20_000 });

  // 詳細シート: SSE ライブ更新で「失敗」に到達し、失敗ステップにエラー内容が出る。
  const sheet = page.locator('[role="dialog"]');
  await expect(sheet.locator("h2").getByText("失敗")).toBeVisible({ timeout: 60_000 });
  await expect(sheet.getByText(/boom-e2e/).first()).toBeVisible();

  // 「失敗したところから再開」→ 失敗 step が即 ready に戻り再実行される。
  // 決定的に再失敗するため過渡状態（実行中）は数秒で消える。再実行の証跡は
  // step タイムラインの「試行 2 回目」バッジ（SSE ライブ更新で反映）で確認する。
  await sheet.getByRole("button", { name: "失敗したところから再開" }).click();
  // toast は aria-live 領域にも複製されるため first() で strict 違反を避ける。
  await expect(page.getByText(/失敗したところから再開しました/).first()).toBeVisible({
    timeout: 15_000,
  });
  await expect(sheet.getByText(/2 回目/)).toBeVisible({ timeout: 60_000 });
  await expect(sheet.locator("h2").getByText("失敗")).toBeVisible({ timeout: 60_000 });
  await sheet.getByRole("button", { name: "閉じる" }).click();

  // テーブルの状態フィルタ「失敗」で絞れる。
  await page.getByRole("button", { name: /^状態/ }).click();
  await page.getByRole("button", { name: "失敗" }).click();
  await page.keyboard.press("Escape");
  const table = page.locator("table");
  await expect(table.getByText("失敗").first()).toBeVisible({ timeout: 10_000 });

  // 行クリックで詳細シートが再び開く（?run= deep-link 復元）。
  await table.locator("tbody tr").first().click();
  await expect(page.locator('[role="dialog"]').getByText("実行の詳細")).toBeVisible();
});
