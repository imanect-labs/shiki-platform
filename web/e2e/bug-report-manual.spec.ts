import { test, expect } from "@playwright/test";

// UI の状態・通信失敗は決定的なローカル応答で検証する。API/認可/DB は Rust の HTTP/IT が担当。
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

test("手動報告：空状態、同意、確認、未設定で保留、キーボード操作", async ({
  page,
}) => {
  await loginReport(page);
  let submitted = false;
  await page.route(/\/api\/bug-reports(?:\/.*)?(?:\?.*)?$/, async (route) => {
    const path = new URL(route.request().url()).pathname;
    const body = path.endsWith("/policy")
      ? { policy, destination_available: false, destination_version: null }
      : path.endsWith("/submit")
        ? ((submitted = true), report)
        : path.endsWith(report.id)
          ? {
              report,
              submission: {
                content,
                content_consent: true,
                sink_profile_id: "disabled",
                sink_config_version: 1,
                notice_version: "bug-report-v1",
                approved_by: null,
                rejection_reason: null,
              },
            }
          : route.request().method() === "POST"
            ? { ...report, status: "draft", rev: 1 }
            : {
                items: submitted ? [{ ...report, title: content.title }] : [],
                next_cursor: null,
              };
    await route.fulfill({ json: body });
  });
  await page.goto("/bug-reports");
  await expect(page.getByText("報告はまだありません")).toBeVisible();
  await shot(page, "empty");
  await page
    .getByRole("link", { name: "新しく報告する", exact: true })
    .last()
    .click();
  await page.getByLabel("題名（必須）").fill(content.title);
  await page.getByLabel("どのようなことが起きましたか").fill(content.symptom);
  await page
    .getByLabel("試したこと", { exact: true })
    .fill(content.steps_tried);
  await page
    .getByLabel("本来どう動いてほしいですか")
    .fill(content.expected_behavior);
  await page.getByRole("checkbox").check();
  await shot(page, "form");
  await page.getByRole("button", { name: "送信内容を確認" }).focus();
  await page.keyboard.press("Enter");
  await expect(page.getByRole("dialog")).toBeVisible();
  await shot(page, "confirmation");
  await page.keyboard.press("Escape");
  await expect(page.getByRole("dialog")).not.toBeVisible();
  await page.getByRole("button", { name: "送信内容を確認" }).click();
  await page.keyboard.press("Tab");
  await expect(
    page.getByRole("button", { name: "この内容で報告する" }),
  ).toBeFocused();
  await page.getByRole("button", { name: "この内容で報告する" }).click();
  await expect(
    page.getByText("保留（外部には未送信）", { exact: true }),
  ).toBeVisible();
  await expect(
    page.getByText(
      "外部には送信していません。送り先の設定後、本人による再確認が必要です。",
      { exact: true },
    ),
  ).toBeVisible();
  await expect(page.getByRole("button", { name: "下書きを保存" })).toHaveCount(
    0,
  );
  await shot(page, "submitted");
  await page.goto("/bug-reports");
  await expect(
    page.getByRole("link", { name: content.title, exact: false }),
  ).toBeVisible();
  await shot(page, "list");
});

test("共有禁止では本文入力を表示せず、カテゴリだけを送る", async ({ page }) => {
  await loginReport(page);
  await page.route("**/api/bug-reports/policy", (route) =>
    route.fulfill({
      json: {
        policy: { ...policy, allow_content_sharing: false },
        destination_available: false,
        destination_version: null,
      },
    }),
  );
  await page.goto("/bug-reports/new");
  await expect(
    page.getByText("組織の設定で本文の共有が禁止されています。", {
      exact: false,
    }),
  ).toBeVisible();
  await expect(page.getByRole("textbox")).toHaveCount(0);
  await page.getByRole("button", { name: "送信内容を確認" }).click();
  await expect(page.getByRole("dialog")).toContainText("本文は共有しません");
});

test("読み込み・エラー状態を表示して再試行できる", async ({ page }) => {
  await loginReport(page);
  let release!: () => void;
  const pending = new Promise<void>((resolve) => {
    release = resolve;
  });
  await page.route("**/api/bug-reports", async (route) => {
    await pending;
    await route.fulfill({ status: 503, json: {} });
  });
  await page.goto("/bug-reports");
  await expect(page.getByText("読み込み中…", { exact: true })).toBeVisible();
  release();
  await expect(page.getByRole("alert").filter({ hasText: "。" })).toContainText(
    "読み込み・保存ができませんでした",
  );
  await expect(
    page.getByRole("button", { name: "もう一度読み込む" }),
  ).toBeVisible();
  await shot(page, "error");
});
