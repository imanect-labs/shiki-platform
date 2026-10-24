import { expect, test } from "@playwright/test";

import { loginAs, loginViaKeycloak, uniqueName } from "./helpers";

/// 管理コンソールの「データ」（監査 B3-5・`/org-admin/data/*`）: テナントの構造化データの上限と、
/// テーブル別のクエリ最適化の報告・固定計画の切替。
/// 前提: dev seed で alice が組織管理者、bob は一般メンバー。

test.describe("管理コンソール: データ", () => {
  test("上限を変えて保存でき、範囲外は保存できない", async ({ page }) => {
    await loginViaKeycloak(page);
    await page.goto("/admin");
    await page.getByRole("link", { name: "データ" }).first().click();
    await page.waitForURL(/\/admin\/data\/settings$/);
    await expect(page.getByRole("heading", { name: "データの設定" })).toBeVisible();
    // 権限の反映の遅れは製品の約束として表示だけする（既定 5 秒）。
    await expect(page.getByTestId("data-revocation-delay")).toHaveText(/秒$/);

    const cap = page.getByLabel("件数の上限");
    const next = (await cap.inputValue()) === "20000" ? "30000" : "20000";
    await cap.fill(next);
    await page.getByTestId("data-settings-save").click();
    await expect(page.getByText("データの設定を保存しました", { exact: true })).toBeVisible();
    await page.reload();
    await expect(page.getByLabel("件数の上限")).toHaveValue(next);

    // 範囲外は保存ボタンが無効で理由が出る。
    await page.getByLabel("索引列の上限（テーブルあたり）").fill("0");
    await expect(page.getByText(/索引列の上限は 1〜1,000 の整数/)).toBeVisible();
    await expect(page.getByTestId("data-settings-save")).toBeDisabled();
    await page.getByRole("button", { name: "取り消す" }).click();

    // 空欄で既定に戻す（件数の上限は省略すると既定へ戻る API なので、他の項目だけ変えても保たれること）。
    // 前の実行の値が残っていても変更になるよう、今の値と違う値を入れる。
    const lookups = page.getByLabel("ルックアップ・ロールアップ列の上限（ビューあたり）");
    await lookups.fill((await lookups.inputValue()) === "15" ? "16" : "15");
    await page.getByTestId("data-settings-save").click();
    await expect(page.getByText("データの設定を保存しました", { exact: true }).first()).toBeVisible();
    await page.reload();
    await expect(page.getByLabel("件数の上限")).toHaveValue(next);
    await page.getByLabel("ルックアップ・ロールアップ列の上限（ビューあたり）").fill("");
    await page.getByLabel("件数の上限").fill("");
    const saved = page.waitForResponse((r) => r.url().includes("/org-admin/data/") && r.request().method() !== "GET");
    await page.getByTestId("data-settings-save").click();
    // 保存の完了を待ってから読み直す（待たないと読み直しが保存より先に走り、古い値を読む）。
    expect((await saved).ok()).toBeTruthy();
    await page.reload();
    await expect(page.getByLabel("件数の上限")).toHaveValue("");
  });

  test("テーブル別の報告を開き、固定計画を切り替えられる", async ({ page }) => {
    await loginViaKeycloak(page);
    const csrf = (await page.context().cookies()).find((c) => c.name === "shiki_csrf")?.value ?? "";
    const name = uniqueName("e2e-最適化");
    const res = await page.request.post("/api/data/tables", {
      headers: { "X-CSRF-Token": csrf },
      data: { name, schema: { fields: [{ key: "name", display_name: "件名", type: "text", indexed: true }] } },
    });
    expect(res.status()).toBe(201);
    const tableId = ((await res.json()) as { id: string }).id;

    await page.goto("/admin/data/optimizer");
    await expect(page.getByRole("heading", { name: "クエリ最適化" })).toBeVisible();
    await expect(page.getByTestId("optimizer-unit-costs")).toContainText("DB 往復");

    await page.getByTestId("optimizer-table-select").click();
    await page.getByRole("option", { name }).click();
    await page.waitForURL(new RegExp(`table=${tableId}`));
    const report = page.getByTestId("optimizer-table-report");
    await expect(report).toBeVisible();
    await expect(report).toContainText("走査効率 p95");

    const fixed = page.getByLabel("固定計画で読む");
    await expect(fixed).not.toBeChecked();
    await fixed.click();
    await expect(page.getByText("固定計画に切り替えました", { exact: true })).toBeVisible();
    await page.reload();
    await expect(page.getByLabel("固定計画で読む")).toBeChecked();
    await page.getByLabel("固定計画で読む").click();
    await expect(page.getByText("自動の計画に戻しました", { exact: true })).toBeVisible();
  });

  test("一般メンバーには開かない", async ({ page }) => {
    await loginAs(page, "bob");
    await page.goto("/admin/data/settings");
    await expect(page.getByText("組織の管理者だけが使えます").first()).toBeVisible();
  });
});
