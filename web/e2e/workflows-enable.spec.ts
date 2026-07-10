import { expect, test, type Page } from "@playwright/test";

import { loginViaKeycloak, uniqueName } from "./helpers";

/// 有効化・同意フローの e2e（Task 10.4a/10.12 DoD）:
/// スケジュールトリガを UI で設定 → 保存 → 同意ダイアログ（日本語スコープ説明）→
/// 有効化 → バッジ表示 → 停止 → バッジ解除。

/// BFF 経由でワークフローを作る（エディタ検証は workflows-editor.spec に集約）。
async function createFlow(page: Page, name: string): Promise<string> {
  const cookies = await page.context().cookies();
  const csrf = cookies.find((c) => c.name === "shiki_csrf")?.value ?? "";
  const res = await page.request.post("/api/workflows", {
    headers: { "x-csrf-token": csrf, "content-type": "application/json" },
    data: {
      ir: {
        ir_version: 1,
        // display_name も一意にする（開発 DB では過去実行の同名フローが残り strict 違反になる）。
        name,
        display_name: name,
        triggers: [{ kind: "interactive" }],
        nodes: [
          {
            id: "compute",
            type: "script.run",
            label: "計算する",
            params: { source: { inline: "function main(i){ return { ok: true }; }" } },
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

test("有効化: スケジュール設定→同意→有効→停止", async ({ page }) => {
  test.setTimeout(180_000);
  await loginViaKeycloak(page);
  const name = uniqueName("enable-e2e");
  const id = await createFlow(page, name);
  await page.goto(`/workflows/${id}`);

  // トリガ（きっかけ）ノードをクリック → 設定パネルでスケジュールに切替。
  const canvas = page.locator(".react-flow");
  await canvas.getByText("手動で実行").first().click();
  await page.getByRole("combobox").first().click();
  await page.getByRole("option", { name: "スケジュール" }).click();
  // cron プリセット（毎日 9:00 既定）の次回実行プレビューが出る。
  await expect(page.getByText(/次回の実行/)).toBeVisible({ timeout: 10_000 });

  // 保存してから有効化（dirty 中は有効化できないガードの裏取り）。
  const enableButton = page.getByRole("button", { name: "自動実行を有効化" });
  await expect(enableButton).toBeDisabled();
  await page.getByRole("button", { name: "保存", exact: true }).click();
  await expect(page.getByText(/保存しました/)).toBeVisible({ timeout: 15_000 });
  await enableButton.click();

  // 同意ダイアログ: 何ができるかの日本語説明 → 同意して有効化。
  const dialog = page.locator('[role="dialog"]');
  await expect(dialog.getByText("自動実行の設定")).toBeVisible();
  await expect(dialog.getByText("このワークフローができること")).toBeVisible();
  await dialog.getByRole("button", { name: "同意して有効化" }).click();
  await expect(page.getByText(/自動実行を有効にしました/)).toBeVisible({ timeout: 15_000 });
  await expect(page.getByText(/自動実行 有効/)).toBeVisible({ timeout: 10_000 });

  // 一覧にも有効バッジが出る。
  await page.goto("/workflows");
  const row = page.getByRole("button", { name: new RegExp(name) });
  await expect(row.getByText("有効", { exact: true })).toBeVisible();

  // 停止: 設定ダイアログから無効化 → バッジが消える。
  await row.click();
  await page.waitForURL(/\/workflows\/[0-9a-f-]+$/i);
  await page.getByRole("button", { name: "自動実行の設定" }).click();
  await page
    .locator('[role="dialog"]')
    .getByRole("button", { name: "自動実行を止める" })
    .click();
  await expect(page.getByText(/自動実行を無効にしました/)).toBeVisible({ timeout: 15_000 });
  await expect(page.getByText(/自動実行 有効/)).toHaveCount(0);
});
