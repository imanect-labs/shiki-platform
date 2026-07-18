import { expect, test } from "@playwright/test";

import { loginViaKeycloak, uniqueName } from "./helpers";

/// スライド（Task 11.1）: 新規作成→閲覧→プレゼン、および XSS negative（PIT-40）。

test("新規作成→スライドが開きプレゼンできる", async ({ page }) => {
  await loginViaKeycloak(page);
  await page.goto("/drive");

  // 新規作成 > スライド → ビューアへ遷移。
  await page.getByRole("button", { name: "新規作成" }).click();
  await page.getByTestId("new-slide").click();
  await page.waitForURL(/\/slides\//, { timeout: 20_000 });

  // 初期スライド（タイトル 1 枚）がフィルムストリップとメインに描画される。
  await expect(page.getByTestId("slide-filmstrip")).toBeVisible({ timeout: 20_000 });
  await expect(page.getByTestId("slide-frame").first()).toBeVisible();

  // sandbox iframe（全能力拒否）で描画されている（PIT-40 第3層の固定化）。
  const sandbox = await page
    .getByTestId("slide-frame")
    .first()
    .locator("iframe")
    .getAttribute("sandbox");
  expect(sandbox).toBe("");

  // プレゼンモードに入って終了できる。
  await page.getByTestId("slide-present").click();
  await expect(page.getByTestId("present-mode")).toBeVisible();
  await page.keyboard.press("Escape");
  await expect(page.getByTestId("present-mode")).toBeHidden();
});

test("script 入り .slide の直接アップロードがどの経路でも実行されない", async ({ page }) => {
  await loginViaKeycloak(page);

  // alert/dialog が一度でも出たら失敗にする（stored XSS の検出）。
  let dialogFired = false;
  page.on("dialog", (d) => {
    dialogFired = true;
    void d.dismiss();
  });

  await page.goto("/drive");
  const fileName = `${uniqueName("xss")}.slide`;
  const malicious = JSON.stringify({
    version: 1,
    meta: { title: "xss" },
    slides: [
      {
        id: "s1",
        html: '<script>alert(1)</script><img src="x" onerror="alert(2)"><p onclick="alert(3)">本文テキスト</p>',
        notes: "",
      },
    ],
  });

  // ドライブへ直接アップロード（エディタ・API 正規化を通らない流入経路）。
  await page.locator('input[type="file"][multiple]').setInputFiles({
    name: fileName,
    mimeType: "application/vnd.shiki.slide+json",
    buffer: Buffer.from(malicious),
  });
  await expect(page.getByText(fileName, { exact: true })).toBeVisible({ timeout: 20_000 });

  // 一覧はアップロード直後の再取得で揺れるため、検索で 1 件に絞ってから開く。
  await page.getByPlaceholder("ドライブを検索").fill(fileName);
  await expect(page.getByText(fileName, { exact: true }).first()).toBeVisible({
    timeout: 10_000,
  });
  // 検索絞り込み後も content 検索の遅延反映で行が差し替わり、クリックが再マウント前の
  // 要素に当たって遷移しないことがある（CI で実測）。遷移しなければ再クリックする。
  for (let i = 0; i < 3 && !/\/slides\//.test(page.url()); i++) {
    await page.getByText(fileName, { exact: true }).first().click();
    await page.waitForURL(/\/slides\//, { timeout: 10_000 }).catch(() => {});
  }
  expect(page.url()).toMatch(/\/slides\//);
  const frame = page.getByTestId("slide-frame").first();
  await expect(frame).toBeVisible({ timeout: 20_000 });

  // iframe 内にテキストが描画されている（内容自体は表示される）。
  const inner = page.frameLocator('[data-testid="slide-frame"] iframe').first();
  await expect(inner.getByText("本文テキスト")).toBeVisible();

  // スクリプト実行の痕跡がない（alert 等のダイアログが発火していない）。
  await page.waitForTimeout(500);
  expect(dialogFired).toBe(false);
});
