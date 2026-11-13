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

test("下書きの競合後に本文とrevisionを読み直し再提出できる", async ({ page }) => {
  await loginReport(page);
  let current = { ...report, status: "draft", rev: 1 };
  let patches = 0;
  let submitted = false;
  await page.route(/\/api\/bug-reports(?:\/.*)?(?:\?.*)?$/, async (route) => {
    const path = new URL(route.request().url()).pathname;
    if (path.endsWith("/policy"))
      return route.fulfill({ json: { policy, destination_available: false, destination_version: null } });
    if (route.request().method() === "PATCH") {
      patches++;
      if (route.request().postDataJSON().expected_rev !== current.rev)
        return route.fulfill({ status: 409, json: {} });
      current = { ...current, content: route.request().postDataJSON().content, rev: current.rev + 1 };
      return route.fulfill({ json: current });
    }
    if (path.endsWith("/submit")) {
      expect(route.request().postDataJSON().expected_rev).toBe(current.rev);
      submitted = true;
      current = { ...current, status: "held" };
      return route.fulfill({ json: current });
    }
    return route.fulfill({ json: { report: current, submission: null } });
  });
  await page.goto(`/bug-reports/${report.id}`);
  await expect(page.getByLabel("題名（必須）")).toHaveValue(content.title);
  await page.getByLabel("題名（必須）").fill("このタブの編集");
  await page.getByRole("checkbox").check();
  current = { ...current, rev: 2, content: { ...content, title: "別タブで更新した題名", symptom: "別タブで更新した症状" } };
  await page.getByRole("button", { name: "送信内容を確認" }).click();
  await page.getByRole("button", { name: "この内容で報告する" }).click();
  await expect(page.getByRole("alert").filter({ hasText: "読み直しました" })).toBeVisible();
  await expect(page.getByLabel("題名（必須）")).toHaveValue(current.content.title);
  await expect(page.getByLabel("どのようなことが起きましたか")).toHaveValue(current.content.symptom);
  await expect(page.getByRole("checkbox")).not.toBeChecked();
  await page.getByRole("alert").filter({ hasText: "読み直しました" }).scrollIntoViewIfNeeded();
  await shot(page, "draft-conflict");
  await page.getByRole("checkbox").check();
  await page.getByRole("button", { name: "送信内容を確認" }).click();
  await page.getByRole("button", { name: "この内容で報告する" }).click();
  await expect.poll(() => submitted).toBe(true);
  await expect(page.getByText("報告を受け付けました", { exact: true })).toBeVisible();
  expect(patches).toBe(2);
  await page.reload();
  await expect(page.getByText("保留（外部には未送信）", { exact: true })).toBeVisible();
});

for (const admin of [false, true]) {
  test(`${admin ? "管理者" : "本人"}一覧は追加ページとカーソルを自動更新後も保持する`, async ({ page }) => {
    await loginReport(page);
    await page.clock.install();
    const items = Array.from({ length: 120 }, (_, i) => ({
      id: `report-${i}`, title: `報告 ${i}`, status: "pending_approval", rev: 1,
    }));
    let refreshes = 0;
    await page.route(`**/api/${admin ? "org-admin/" : ""}bug-reports*`, async (route) => {
      const cursor = new URL(route.request().url()).searchParams.get("cursor");
      const start = cursor === "page-2" ? 50 : cursor === "page-3" ? 100 : 0;
      if (!cursor) refreshes++;
      return route.fulfill({ json: {
        items: items.slice(start, start + 50),
        next_cursor: start === 0 ? "page-2" : start === 50 ? "page-3" : null,
      } });
    });
    await page.goto(`${admin ? "/admin" : ""}/bug-reports`);
    const reports = page.locator('main li a[href*="/bug-reports/report-"]');
    await expect(reports).toHaveCount(50);
    await page.getByRole("button", { name: "続きを表示" }).click();
    await expect(reports).toHaveCount(100);
    items[0] = { ...items[0], title: "更新された報告" };
    await page.evaluate(() => window.dispatchEvent(new Event("focus")));
    await expect(reports.first()).toContainText("更新された報告");
    await expect(reports).toHaveCount(100);
    const before = refreshes;
    await page.clock.fastForward(30_000);
    await expect.poll(() => refreshes).toBeGreaterThan(before);
    await expect(reports).toHaveCount(100);
    await page.getByRole("button", { name: "続きを表示" }).click();
    await expect(reports).toHaveCount(120);
    await expect(page.getByRole("button", { name: "続きを表示" })).toHaveCount(0);
    await page.evaluate(() => window.dispatchEvent(new Event("focus")));
    await expect.poll(() => refreshes).toBeGreaterThan(before + 1);
    await expect(reports).toHaveCount(120);
    await expect(page.getByRole("button", { name: "続きを表示" })).toHaveCount(0);
  });
}

test("下書きは自動更新でも編集と同意を保持し保存・取り下げできる", async ({
  page,
}) => {
  await loginReport(page);
  await page.clock.install();
  let current = { ...report, status: "draft", rev: 1 };
  let failure = 0;
  let gets = 0;
  let patches = 0;
  let cancelled = false;
  await page.route(/\/api\/bug-reports(?:\/.*)?(?:\?.*)?$/, async (route) => {
    const path = new URL(route.request().url()).pathname;
    if (path.endsWith("/policy"))
      return route.fulfill({
        json: {
          policy,
          destination_available: false,
          destination_version: null,
        },
      });
    if (path.endsWith("/cancel")) {
      expect(route.request().postDataJSON().expected_rev).toBe(current.rev);
      cancelled = true;
      current = { ...current, status: "cancelled", rev: current.rev + 1 };
      return route.fulfill({ json: current });
    }
    if (route.request().method() === "PATCH") {
      expect(route.request().postDataJSON().expected_rev).toBe(current.rev);
      patches++;
      current = {
        ...current,
        rev: current.rev + 1,
        content: route.request().postDataJSON().content,
      };
      return route.fulfill({ json: current });
    }
    if (path.endsWith(report.id)) {
      gets++;
      if (failure) return route.fulfill({ status: failure, json: {} });
      return route.fulfill({ json: { report: current, submission: null } });
    }
    return route.fulfill({ json: { items: [], next_cursor: null } });
  });
  await page.goto(`/bug-reports/${report.id}`);
  const title = page.getByLabel("題名（必須）");
  await expect(title).toHaveValue(content.title);
  await title.fill("未保存の下書き");
  await page.getByRole("checkbox").check();
  failure = 503;
  const before = gets;
  await page.evaluate(() => window.dispatchEvent(new Event("focus")));
  await expect.poll(() => gets).toBeGreaterThan(before);
  await expect(
    page.getByRole("alert").filter({ hasText: "読み込み・保存" }),
  ).toBeVisible();
  await expect(title).toHaveValue("未保存の下書き");
  await expect(page.getByRole("checkbox")).toBeChecked();
  failure = 0;
  await page.getByRole("button", { name: "もう一度読み込む" }).click();
  await expect(
    page.getByRole("alert").filter({ hasText: "読み込み・保存" }),
  ).toHaveCount(0);
  const timer = gets;
  await page.clock.fastForward(30_000);
  await expect.poll(() => gets).toBeGreaterThan(timer);
  await expect(title).toHaveValue("未保存の下書き");
  await expect(page.getByRole("checkbox")).toBeChecked();
  await page.getByRole("button", { name: "下書きを保存", exact: true }).focus();
  await page.keyboard.press("Enter");
  await expect.poll(() => patches).toBe(1);
  expect(current.content.title).toBe("未保存の下書き");
  await page.reload();
  await expect(title).toHaveValue("未保存の下書き");
  await expect(page.getByRole("checkbox")).not.toBeChecked();
  await title.fill("保存後の編集");
  await page.getByRole("button", { name: "下書きを保存", exact: true }).click();
  await expect.poll(() => patches).toBe(2);
  const shots = process.env.BUG_REPORT_SHOTS;
  if (shots) {
    await page.getByRole("button", { name: "閉じる", exact: true }).last().click();
    await expect(page.getByText("下書きを保存しました", { exact: true })).toHaveCount(0);
    await page.screenshot({
      path: `${shots}/draft-edit-${process.env.BUG_REPORT_MOBILE ? "mobile" : "desktop"}-${process.env.BUG_REPORT_DARK ? "dark" : "light"}.png`,
      fullPage: true,
      animations: "disabled",
    });
    await page.getByRole("heading", { name: "下書きを編集する" }).scrollIntoViewIfNeeded();
    await page.screenshot({
      path: `${shots}/draft-edit-${process.env.BUG_REPORT_MOBILE ? "mobile" : "desktop"}-${process.env.BUG_REPORT_DARK ? "dark" : "light"}-top.png`,
      fullPage: true, animations: "disabled",
    });
  }
  await page.getByRole("button", { name: "報告を取り下げる" }).click();
  await expect(page).toHaveURL(/\/bug-reports$/);
  expect(cancelled).toBe(true);
});

for (const status of [403, 404]) {
  test(`下書きの自動更新が${status}なら表示を解除する`, async ({ page }) => {
    await loginReport(page);
    let denied = false;
    await page.route("**/api/bug-reports/policy", (route) =>
      route.fulfill({
        json: {
          policy,
          destination_available: false,
          destination_version: null,
        },
      }),
    );
    await page.route(`**/api/bug-reports/${report.id}`, (route) =>
      route.fulfill(
        denied
          ? { status, json: {} }
          : {
              json: {
                report: { ...report, status: "draft" },
                submission: null,
              },
            },
      ),
    );
    await page.goto(`/bug-reports/${report.id}`);
    await expect(page.getByLabel("題名（必須）")).toBeVisible();
    denied = true;
    await page.evaluate(() => window.dispatchEvent(new Event("focus")));
    await expect(page.getByLabel("題名（必須）")).toHaveCount(0);
    await expect(
      page.getByRole("button", { name: "下書きを保存" }),
    ).toHaveCount(0);
  });
}
