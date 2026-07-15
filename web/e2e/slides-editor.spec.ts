import { expect, test } from "@playwright/test";

import { loginViaKeycloak, uniqueName } from "./helpers";

/// スライドエディタ（Task 11.2）: GrapesJS 砂箱の起動・編集の Yjs 反映・2 コンテキスト収束。
/// エディタバンドル（app-gateway /builtin）が未配備の環境ではスキップする
/// （compose への同梱は Task 11.5 系のデプロイ PR で行う）。

const BUILTIN_URL = process.env.E2E_B1_ORIGIN
  ? `${process.env.E2E_B1_ORIGIN}/builtin/slide-editor`
  : "http://localhost:8091/builtin/slide-editor";

test.beforeAll(async () => {
  const res = await fetch(BUILTIN_URL).catch(() => null);
  test.skip(!res?.ok, `エディタバンドル未配備のためスキップ（${BUILTIN_URL}）`);
});

test("編集がもう一方のユーザーの画面に収束する（共同編集）", async ({ browser }) => {
  // ユーザー A がスライドを作成して編集する。
  const ctxA = await browser.newContext();
  const pageA = await ctxA.newPage();
  await loginViaKeycloak(pageA);
  await pageA.goto("/drive");
  await pageA.getByRole("button", { name: "新規作成" }).click();
  await pageA.getByTestId("new-slide").click();
  await pageA.waitForURL(/\/slides\//, { timeout: 20_000 });
  const slideUrl = pageA.url();

  // エディタ iframe: 別オリジン隔離（allow-scripts allow-same-origin のみ・それ以上を許可しない）。
  const frame = pageA.getByTestId("slide-editor-frame");
  await expect(frame).toBeVisible({ timeout: 20_000 });
  expect(await frame.getAttribute("sandbox")).toBe("allow-scripts allow-same-origin");

  // キャンバスで見出しを編集する（RTE）。
  const canvas = pageA
    .frameLocator('[data-testid="slide-editor-frame"]')
    .frameLocator("iframe.gjs-frame");
  const h1 = canvas.locator("h1").first();
  await h1.dblclick();
  await pageA.keyboard.press("ControlOrMeta+a");
  const typed = uniqueName("共同編集テスト");
  await pageA.keyboard.type(typed);
  // キャンバスの余白クリックで RTE を確定させる（実ユーザーの操作と同じ経路）。
  await canvas.locator("body").click({ position: { x: 10, y: 10 } });

  // 同一ユーザーの別セッション（別タブ相当）で同じスライドを開くと、編集が反映されている。
  const ctxB = await browser.newContext();
  const pageB = await ctxB.newPage();
  await loginViaKeycloak(pageB);
  await pageB.goto(slideUrl);
  const filmstripB = pageB.getByTestId("slide-filmstrip");
  await expect(filmstripB).toBeVisible({ timeout: 20_000 });
  // フィルムストリップのサムネイル（sandbox iframe）に編集後のテキストが出る。
  await expect(
    pageB.frameLocator('[data-testid="slide-frame"] iframe').first().getByText(typed),
  ).toBeVisible({ timeout: 20_000 });

  await ctxA.close();
  await ctxB.close();
});

test("スライドの追加が反映され、切替時に未確定編集が失われない", async ({ page }) => {
  await loginViaKeycloak(page);
  await page.goto("/drive");
  await page.getByRole("button", { name: "新規作成" }).click();
  await page.getByTestId("new-slide").click();
  await page.waitForURL(/\/slides\//, { timeout: 20_000 });
  await expect(page.getByTestId("slide-editor-frame")).toBeVisible({ timeout: 20_000 });

  // RTE で編集したまま（確定操作なしで）スライドを追加 → 切替前の編集が確定・保全される。
  const canvas = page
    .frameLocator('[data-testid="slide-editor-frame"]')
    .frameLocator("iframe.gjs-frame");
  await canvas.locator("h1").first().dblclick();
  await page.keyboard.press("ControlOrMeta+a");
  const typed = uniqueName("切替前の編集");
  await page.keyboard.type(typed);
  await page.getByTestId("slide-add").click();

  // フィルムストリップが 2 枚になり、1 枚目のサムネイルに編集内容が残る。
  const thumbs = page.getByTestId("slide-filmstrip").getByTestId("slide-frame");
  await expect(thumbs).toHaveCount(2, { timeout: 20_000 });
  await expect(
    page.frameLocator('[data-testid="slide-frame"] iframe').first().getByText(typed),
  ).toBeVisible({ timeout: 20_000 });
});
