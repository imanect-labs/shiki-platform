import { test, expect } from "@playwright/test";
import {
  content,
  report,
  policy,
  shot,
  loginReport,
} from "./bug-report-fixtures";

test.use({
  deviceScaleFactor: 2,
  colorScheme: process.env.BUG_REPORT_DARK ? "dark" : "light",
  ...(process.env.BUG_REPORT_MOBILE
    ? { viewport: { width: 390, height: 844 } }
    : { viewport: { width: 1440, height: 1000 } }),
});

test("管理者の受信箱、競合409、再取得と失権", async ({ page }) => {
  await loginReport(page);
  let state = "pending_approval";
  let revision = 2;
  let revoked = false;
  await page.route(
    /\/api\/org-admin\/bug-reports(?:\/.*)?(?:\?.*)?$/,
    async (route) => {
      if (revoked) return route.fulfill({ status: 403, json: {} });
      const path = new URL(route.request().url()).pathname;
      if (path.endsWith("/approve")) {
        state = "queued";
        revision++;
        return route.fulfill({ status: 409, json: {} });
      }
      await route.fulfill({
        json: path.endsWith(report.id)
          ? {
              report: { ...report, status: state, rev: revision },
              submission: {
                content,
                content_consent: true,
                sink_profile_id: "github",
                sink_config_version: 1,
                notice_version: "bug-report-v1",
                approved_by: state === "queued" ? "admin" : null,
                rejection_reason: null,
              },
            }
          : {
              items: [
                {
                  ...report,
                  title: content.title,
                  status: state,
                  rev: revision,
                },
              ],
              next_cursor: null,
            },
      });
    },
  );
  await page.goto("/admin/bug-reports");
  await shot(page, "admin-inbox");
  await page.getByRole("link", { name: content.title, exact: false }).click();
  await expect(
    page.getByText("差し戻しの理由（報告した本人にも表示されます）"),
  ).toBeVisible();
  await shot(page, "admin-detail");
  await page.getByRole("button", { name: "承認する" }).click();
  await expect(page.getByRole("alert").filter({ hasText: "。" })).toContainText(
    "別の画面で内容が更新されました",
  );
  await expect(page.getByText("送信待ち", { exact: true })).toBeVisible();
  await shot(page, "conflict");
  await expect(page.getByRole("button", { name: "承認する" })).toHaveCount(0);
  revoked = true;
  await page.reload();
  await expect(page.getByRole("alert").filter({ hasText: "。" })).toContainText(
    "権限がありません",
  );
  await expect(page.getByText(content.symptom, { exact: true })).toHaveCount(0);
});

test("承認必須のON/OFFと設定の409を表示する", async ({ page }) => {
  await loginReport(page);
  let current = { ...policy };
  let writes = 0;
  await page.route("**/api/org-admin/bug-reports/settings", async (route) => {
    if (route.request().method() === "PUT") {
      writes++;
      if (writes === 2) return route.fulfill({ status: 409, json: {} });
      current = route.request().postDataJSON();
      current.rev++;
    }
    await route.fulfill({ json: current });
  });
  await page.goto("/admin/bug-reports/settings");
  const approval = page.getByLabel("送信前に管理者の承認を必須にする");
  await expect(approval).toBeChecked();
  await shot(page, "settings");
  await approval.uncheck();
  await page.getByRole("button", { name: "設定を保存", exact: true }).click();
  await expect(
    page.getByText("設定を保存しました。", { exact: true }),
  ).toBeVisible();
  await expect(approval).not.toBeChecked();
  await approval.check();
  await page.getByRole("button", { name: "設定を保存", exact: true }).click();
  await expect(page.getByRole("alert").filter({ hasText: "。" })).toContainText(
    "別の画面で内容が更新されました",
  );
  await expect(approval).not.toBeChecked();
});
