import { test, expect } from "@playwright/test";

import { loginViaKeycloak } from "./helpers";

/// 生成中に送った発話が「順番待ち」として受理され、自動で送られること。
///
/// 前提: LLM=stub。`slow:<秒> <本文>` は本文を出したあと指定秒だけ生成を続けるので、
/// その間に 2 通目を送る余裕が決定的に作れる。
///
/// ここで守っているのは「返答を待たないと次を書けない」体験を作らないこと。キューは
/// **サーバが持つ**（投入時点でメッセージは保存済み・run は先行 run の後ろに直列化される）ので、
/// ページを離れても消えない。
test.describe("生成中の発話（順番待ち）", () => {
  test("生成中でも送れて、順番待ちのまま再訪でき、順に応答が返る", async ({ page }) => {
    await loginViaKeycloak(page);
    await page.goto("/");

    const tag = Date.now().toString(36);
    const first = `slow:8 いちばん目 ${tag}`;
    const second = `にばん目 ${tag}`;

    const input = page.getByLabel("メッセージを入力");
    await input.click();
    await input.fill(first);
    await page.getByRole("button", { name: "送信" }).click();
    await page.waitForURL(/\/c\/[0-9a-f-]+/i, { timeout: 20_000 });

    // 1 通目の生成が始まる（本文が出て、停止ボタンが出ている＝生成中）。
    await expect(page.getByText(`いちばん目 ${tag}`).first()).toBeVisible({ timeout: 30_000 });
    const stop = page.getByRole("button", { name: "生成を停止" });
    await expect(stop).toBeVisible({ timeout: 20_000 });

    // 生成中に 2 通目を送る（送信ボタンは「順番待ちに追加」として出ている）。
    await input.fill(second);
    await page.getByRole("button", { name: "順番待ちに追加" }).click();

    // 受理されて順番待ちになる（吹き出しは送信済みと同じ・状態だけ小さく添える）。
    await expect(page.getByTestId("queued-message-note")).toBeVisible({ timeout: 10_000 });
    await expect(page.getByTestId("queued-message-note")).toContainText("順番待ち");

    // **ページを離れても消えない**（サーバが持っているため）。
    await page.reload();
    await expect(page.getByText(second, { exact: false })).toBeVisible({ timeout: 20_000 });

    // 1 通目が終わると自動で 2 通目の生成が始まり、両方に応答が付く。
    await expect(page.getByTestId("queued-message-note")).toHaveCount(0, { timeout: 60_000 });
    await expect(page.getByText(/回答/).first()).toBeVisible({ timeout: 60_000 });
    await expect(page.getByRole("button", { name: "生成を停止" })).toHaveCount(0, {
      timeout: 60_000,
    });

    // 発話順が保たれている（2 通目が 1 通目の応答より後ろにある）。
    const body = await page.locator("main").innerText();
    expect(body.indexOf(`いちばん目 ${tag}`)).toBeLessThan(body.indexOf(second));
    expect(body).toContain(second);
  });

  test("順番待ちは取り消せる", async ({ page }) => {
    await loginViaKeycloak(page);
    await page.goto("/");

    const tag = Date.now().toString(36);
    const input = page.getByLabel("メッセージを入力");
    await input.click();
    await input.fill(`slow:8 ながい ${tag}`);
    await page.getByRole("button", { name: "送信" }).click();
    await page.waitForURL(/\/c\/[0-9a-f-]+/i, { timeout: 20_000 });
    await expect(page.getByRole("button", { name: "生成を停止" })).toBeVisible({ timeout: 30_000 });

    await input.fill(`とりけす ${tag}`);
    await page.getByRole("button", { name: "順番待ちに追加" }).click();
    await expect(page.getByTestId("queued-message-note")).toBeVisible({ timeout: 10_000 });

    await page.getByRole("button", { name: "取り消す" }).click();
    await expect(page.getByTestId("queued-message-note")).toHaveCount(0);

    // 取り消した発話は生成されない（サーバ側の run もキャンセルされる）。
    await expect(page.getByRole("button", { name: "生成を停止" })).toHaveCount(0, {
      timeout: 60_000,
    });
    await page.reload();
    await expect(page.getByText(`回答: とりけす ${tag}`, { exact: false })).toHaveCount(0);
  });
});
