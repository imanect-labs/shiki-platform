import { test, expect } from "@playwright/test";

import { loginAs, loginViaKeycloak } from "./helpers";

/// 監査ログ（#634）の E2E。
/// 前提: dev seed で alice は組織管理者かつ監査閲覧者、charlie（b-corp）は管理者でない監査閲覧者、
/// bob はどちらでもない。
test.describe("監査ログ", () => {
  test("監査閲覧者は絞り込み・詳細・書き出しができ、読んだことも記録に残る", async ({ page }) => {
    await loginViaKeycloak(page);
    await page.goto("/admin/audit");
    await expect(page.getByRole("heading", { name: "監査ログ", exact: true })).toBeVisible();
    // 初回の検索（とその記録）が終わってから絞り込む（新しい DB でも org_audit.search が 1 行ある）。
    await expect(page.getByText(/・\d[\d,]* 件/)).toBeVisible();

    // 開いただけで検索が 1 回記録される。自分の閲覧の記録で絞り込める。
    await page.getByLabel("操作の種類").fill("org_audit.");
    await page.getByRole("button", { name: "絞り込む" }).click();
    await expect(page).toHaveURL(/action=org_audit\./);
    const table = page.getByRole("table", { name: "監査ログ（新しい順）" });
    await expect(table.getByRole("cell", { name: "org_audit.search" }).first()).toBeVisible();

    // 詳細から「この操作で絞る」（完全一致）に切り替えられる。
    await page.getByRole("button", { name: "この操作で絞る" }).click();
    await expect(page).toHaveURL(/action=org_audit\.search(&|$)/);
    await expect(page.getByLabel("操作の種類")).toHaveValue("org_audit.search");

    // 書き出し（CSV・BOM 付き・ヘッダ行）。
    await page.getByRole("button", { name: "書き出す" }).click();
    const [download] = await Promise.all([
      page.waitForEvent("download"),
      page.getByRole("menuitem", { name: /CSV/ }).click(),
    ]);
    expect(download.suggestedFilename()).toMatch(/^audit-log_\d{4}-\d{2}-\d{2}_\d{4}-\d{2}-\d{2}\.csv$/);
    const path = await download.path();
    const fs = await import("node:fs/promises");
    const text = await fs.readFile(path, "utf8");
    expect(text.startsWith("﻿id,created_at,actor")).toBeTruthy();
    expect(text).toContain('"org_audit.search"');

    // 書き出したことも記録に残る（再検索で org_audit.export が出る）。
    await page.getByLabel("操作の種類").fill("org_audit.export");
    await page.getByRole("button", { name: "絞り込む" }).click();
    await expect(table.getByRole("cell", { name: "org_audit.export" }).first()).toBeVisible();

    // ハッシュチェーンを先頭から検証できる（#828）。記録したままなら崩れていない。
    await page.getByRole("button", { name: "チェーンを検証", exact: true }).click();
    const dialog = page.getByTestId("audit-verify");
    // 新しい DB では連結された記録がまだ無いこともある。
    await expect(dialog.getByText(/崩れていません（[\d,]+ 件を確認）|連結された記録はまだありません/)).toBeVisible({
      timeout: 20_000,
    });
    await page.keyboard.press("Escape");
    await expect(dialog).toBeHidden();
  });

  test("管理者でない監査閲覧者は監査ログだけを開ける", async ({ page }) => {
    await loginAs(page, "charlie");
    await page.getByRole("button", { name: "アカウントメニューを開く" }).click();
    await expect(page.getByRole("menuitem", { name: "管理コンソール" })).toHaveCount(0);
    await page.getByRole("menuitem", { name: "監査ログ" }).click();
    await page.waitForURL(/\/admin\/audit$/);
    await expect(page.getByRole("heading", { name: "監査ログ", exact: true })).toBeVisible();
    // レールには監査ログだけ。管理画面のホームは監査ログへ送られる。
    const rail = page.getByRole("navigation", { name: "管理メニュー" });
    await expect(rail.getByRole("link", { name: "AI" })).toHaveCount(0);
    await page.goto("/admin");
    await page.waitForURL(/\/admin\/audit$/);
    await page.goto("/admin/ai/models");
    await expect(page.getByText("組織の管理者だけが使えます")).toBeVisible();
  });

  test("監査閲覧者でなければ監査ログは開かない", async ({ page }) => {
    await loginAs(page, "bob");
    await page.goto("/admin/audit");
    await expect(page.getByText("監査ログは監査担当者だけが見られます")).toBeVisible();
    const res = await page.request.get("/api/org-audit/events");
    expect(res.status()).toBe(403);
  });
});
