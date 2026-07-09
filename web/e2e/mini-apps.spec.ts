import { test, expect, type Page } from "@playwright/test";

import { loginAs, loginViaKeycloak, uniqueName } from "./helpers";

/// ミニアプリ（Phase 6 Task 6.10/6.11）の E2E。
/// UI スペックはチャット生成経由が本流だが、E2E では保存 API（/ui-specs・保存時検証つき）で
/// 直接作り、①UI からアプリを作成 ②実行画面で描画 ③**本体だけ共有した相手が実行できる**
/// （バンドル権限・部品の個別共有なし）を実ブラウザで検証する。

/// ログイン済みページのセッションで backend API を叩く（CSRF 二重送信つき）。
async function apiPost(page: Page, path: string, body: unknown): Promise<unknown> {
  const cookies = await page.context().cookies();
  const csrf = cookies.find((c) => c.name === "shiki_csrf")?.value ?? "";
  const res = await page.request.post(`/api${path}`, {
    headers: { "Content-Type": "application/json", "X-CSRF-Token": csrf },
    data: body,
  });
  expect(res.ok(), `${path} → ${res.status()}`).toBeTruthy();
  return res.json();
}

test.describe("ミニアプリ（作成・実行・バンドル権限の共有）", () => {
  test("作成 → 実行画面で描画 → 共有相手が部品の個別共有なしで実行できる", async ({
    page,
    browser,
  }) => {
    await loginViaKeycloak(page);

    // ① UI スペックを保存 API で作成（表示専用テーブル・保存時検証を通る）。
    const specName = uniqueName("e2e-spec");
    await apiPost(page, "/ui-specs", {
      name: specName,
      spec: {
        version: 1,
        root: {
          component: "table",
          title: "月次サマリ",
          columns: [{ label: "項目" }, { label: "値", align: "right" }],
          rows: [
            ["売上", 120],
            ["経費", 45],
          ],
        },
      },
    });

    // ② UI からアプリを作成（UI スペックを選んで束ねる）。
    await page.goto("/apps");
    await page.getByRole("button", { name: "アプリを作成" }).click();
    const dialog = page.getByRole("dialog");
    const appName = uniqueName("e2e-app");
    await dialog.getByLabel(/名前/).fill(appName);
    await dialog.getByLabel(/説明/).fill("E2E 検証用アプリ");
    await dialog.getByLabel(/UI スペック/).selectOption({ label: `${specName}（v1）` });
    await dialog.getByRole("button", { name: "作成" }).click();

    // 一覧に現れ、実行画面でテーブルが描画される。
    const card = page.locator("li", { has: page.getByRole("heading", { name: appName }) });
    await expect(card).toBeVisible({ timeout: 15_000 });
    await card.getByRole("link", { name: "開く" }).click();
    await page.waitForURL(/\/apps\/[0-9a-f-]+/i, { timeout: 15_000 });
    await expect(page.getByTestId("genui-table")).toBeVisible({ timeout: 15_000 });
    await expect(page.getByText("月次サマリ")).toBeVisible();
    const appUrl = page.url();

    // ③ アプリ**本体だけ**を bob に共有する（UI スペックは共有しない）。
    await page.getByRole("button", { name: "共有" }).click();
    await page.getByPlaceholder("名前・メールで検索").fill("bob");
    const bobRow = page.getByRole("dialog").locator("li", { hasText: "bob" }).first();
    await bobRow.getByRole("button", { name: "共有" }).click();
    await expect(page.getByRole("dialog").getByText("共有中の相手")).toBeVisible();

    // ④ bob が実行画面を開ける（部品はバンドル権限で読まれる・Task 6.10 受け入れ条件）。
    const bobContext = await browser.newContext();
    const bobPage = await bobContext.newPage();
    try {
      await loginAs(bobPage, "bob");
      await bobPage.goto(appUrl);
      await expect(bobPage.getByTestId("genui-table")).toBeVisible({ timeout: 15_000 });
      await expect(bobPage.getByText("月次サマリ")).toBeVisible();
    } finally {
      await bobContext.close();
    }
  });
});
