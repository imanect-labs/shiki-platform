import { test, expect, type Page } from "@playwright/test";

import { loginAs } from "./helpers";

/// ユーザー設定（設定 → チャット／表示・#952）の E2E。
/// 前提: chat 有効。設定はアカウント単位なので、ほかの spec（alice・bob）の入力欄に効かないよう
/// charlie で行い、前後で既定値へ戻す（途中で落ちても次の実行・他の spec に残さない）。

/// 自分の設定を既定値へ戻す（欠けた項目はすべて既定値になる）。
async function resetPreferences(page: Page) {
  const status = await page.evaluate(async () => {
    const csrf = document.cookie.match(/(?:^|;\s*)shiki_csrf=([^;]+)/)?.[1];
    const res = await fetch("/api/me/preferences", {
      method: "PUT",
      headers: { "Content-Type": "application/json", "X-CSRF-Token": decodeURIComponent(csrf ?? "") },
      body: "{}",
      credentials: "include",
    });
    return res.status;
  });
  expect(status).toBe(200);
}

test.describe("ユーザー設定", () => {
  test.beforeEach(async ({ page }) => {
    await loginAs(page, "charlie");
    await resetPreferences(page);
  });

  test.afterEach(async ({ page }) => {
    await resetPreferences(page);
  });

  test("Ctrl+Enter で送信にすると Enter は改行になり、再訪しても残る", async ({ page }) => {
    test.setTimeout(120_000);
    await page.goto("/settings/chat");
    const modEnter = page.getByTestId("submit-key-mod-enter");
    await expect(modEnter).toBeVisible({ timeout: 20_000 });
    const saved = page.waitForResponse(
      (r) => r.url().endsWith("/api/me/preferences") && r.request().method() === "PUT",
    );
    await modEnter.click();
    expect((await saved).status()).toBe(200);
    await expect(page.getByText("Ctrl+Enter で送信し、Enter で改行します。")).toBeVisible();

    // 再訪しても残る（サーバに保存されている）。
    await page.reload();
    await expect(page.getByTestId("submit-key-mod-enter")).toHaveAttribute("aria-selected", "true", {
      timeout: 20_000,
    });

    // ホームの入力欄: Enter は改行（送らない）、Ctrl+Enter で送る。
    await page.goto("/");
    const input = page.getByLabel("メッセージを入力");
    await input.click();
    await input.pressSequentially("一行目");
    await input.press("Enter");
    await input.pressSequentially("二行目");
    // 入力欄は Markdown の WYSIWYG なので、改行は段落の区切りになる。
    await expect(input.locator("p")).toHaveText(["一行目", "二行目"]);
    await expect(page).toHaveURL(/\/$/);
    await input.press("Control+Enter");
    await page.waitForURL(/\/c\/[0-9a-f-]+/i, { timeout: 20_000 });
  });

  test("Web 検索を既定にすると新しいチャットで選んだ状態になる", async ({ page }) => {
    await page.goto("/settings/chat");
    const toggle = page.getByRole("switch", { name: "Web 検索を使う" });
    await expect(toggle).toBeVisible({ timeout: 20_000 });
    const saved = page.waitForResponse(
      (r) => r.url().endsWith("/api/me/preferences") && r.request().method() === "PUT",
    );
    await toggle.click();
    expect((await saved).status()).toBe(200);
    await expect(toggle).toBeChecked();

    await page.goto("/");
    await expect(page.getByRole("button", { name: "Web 検索を外す" })).toBeVisible({ timeout: 20_000 });
  });

  test("入力の形式をテキストにすると記法を変換せず、打ったとおりに送る", async ({ page }) => {
    test.setTimeout(120_000);
    await page.goto("/settings/chat");
    const plain = page.getByTestId("input-format-plain");
    await expect(page.getByTestId("input-format-markdown")).toHaveAttribute("aria-selected", "true", {
      timeout: 20_000,
    });
    const saved = page.waitForResponse(
      (r) => r.url().endsWith("/api/me/preferences") && r.request().method() === "PUT",
    );
    await plain.click();
    expect((await saved).status()).toBe(200);
    await expect(plain).toHaveAttribute("aria-selected", "true");

    // ホームの入力欄は素の textarea（`**` は太字にならずに残る）。設定を読むまでは既定の Markdown の
    // 入力欄が出て、読めたら（本文が空なので）textarea に替わる。
    await page.goto("/");
    const input = page.locator('textarea[aria-label="メッセージを入力"]');
    await expect(input).toBeVisible({ timeout: 20_000 });
    await input.click();
    await input.pressSequentially("**そのまま**");
    await expect(input).toHaveValue("**そのまま**");
    await input.press("Enter");
    await page.waitForURL(/\/c\/[0-9a-f-]+/i, { timeout: 20_000 });
    // 吹き出しも打ったとおり（太字にしない）。
    const turn = page.locator("[data-user-turn]").first();
    await expect(turn).toContainText("**そのまま**");
    await expect(turn.locator("strong")).toHaveCount(0);
  });

  test("既定の承認モードはホームのエージェントのモードの初期値になり、そのモードで始まる", async ({ page }) => {
    test.setTimeout(120_000);
    await page.goto("/settings/chat");
    const select = page.getByRole("combobox", { name: "既定の承認モード" });
    await expect(select).toBeVisible({ timeout: 20_000 });
    const saved = page.waitForResponse(
      (r) => r.url().endsWith("/api/me/preferences") && r.request().method() === "PUT",
    );
    await select.click();
    await page.getByRole("option", { name: /オート/ }).click();
    expect((await saved).status()).toBe(200);

    // ホーム: エージェントのモードの承認の選択が、既定（オート）から始まる。
    await page.goto("/");
    await page.getByTestId("home-mode-agent").click();
    await expect(page.getByTestId("home-approval-auto")).toHaveAttribute("aria-checked", "true", {
      timeout: 20_000,
    });

    // 送ると、そのモードを最初の発話より先にスレッドへ設定する。
    const modePut = page.waitForRequest(
      (r) => r.method() === "PUT" && /\/api\/threads\/[^/]+\/autonomous-mode$/.test(r.url()),
    );
    await page.getByLabel("メッセージを入力").fill("承認モードの確認");
    await page.getByRole("button", { name: "送信" }).click();
    expect(JSON.parse((await modePut).postData() ?? "{}")).toEqual({ mode: "auto" });
    await page.waitForURL(/\/c\/[0-9a-f-]+/i, { timeout: 20_000 });
  });

  test("日時の表示を経過時間にすると保存される", async ({ page }) => {
    await page.goto("/settings/appearance");
    const relative = page.getByRole("tab", { name: "経過時間" });
    await expect(relative).toBeVisible({ timeout: 20_000 });
    const saved = page.waitForResponse(
      (r) => r.url().endsWith("/api/me/preferences") && r.request().method() === "PUT",
    );
    await relative.click();
    expect((await saved).status()).toBe(200);
    await expect(page.getByText("「2 時間前」のように表示します")).toBeVisible();
    await page.reload();
    await expect(page.getByRole("tab", { name: "経過時間" })).toHaveAttribute("aria-selected", "true", {
      timeout: 20_000,
    });
  });
});
