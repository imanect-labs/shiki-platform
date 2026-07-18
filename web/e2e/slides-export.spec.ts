import { expect, test } from "@playwright/test";
import JSZip from "jszip";

import { loginViaKeycloak } from "./helpers";

/// pptx エクスポート（Task 11.4）: 生成された .pptx が本物の OOXML で、
/// スライドのテキストが**編集可能なネイティブ要素**として含まれることを検証する（PIT-42）。
/// エディタバンドル（/builtin）未配備の環境ではスキップ。

const BUILTIN_URL = process.env.E2E_B1_ORIGIN
  ? `${process.env.E2E_B1_ORIGIN}/builtin/slide-editor`
  : "http://localhost:8091/builtin/slide-editor";

test.beforeAll(async () => {
  const res = await fetch(BUILTIN_URL).catch(() => null);
  test.skip(!res?.ok, `エディタバンドル未配備のためスキップ（${BUILTIN_URL}）`);
});

test("エクスポートした pptx にネイティブテキストが入る", async ({ page }) => {
  await loginViaKeycloak(page);
  await page.goto("/drive");
  await page.getByRole("button", { name: "新規作成" }).click();
  await page.getByTestId("new-slide").click();
  await page.waitForURL(/\/slides\//, { timeout: 20_000 });
  await expect(page.getByTestId("slide-editor-frame")).toBeVisible({ timeout: 20_000 });

  // エクスポート実行 → レポート表示 → ダウンロード。
  await page.getByTestId("slide-export").click();
  await expect(page.getByTestId("slide-export-report")).toBeVisible({ timeout: 60_000 });
  // 既定スライド（タイトル 1 枚）は全要素ネイティブ変換＝ラスタライズ 0。
  await expect(page.getByTestId("slide-export-report")).toContainText(
    "すべての要素が編集可能な形式で変換されました",
  );

  const downloadPromise = page.waitForEvent("download");
  await page.getByTestId("slide-export-download").click();
  const download = await downloadPromise;
  expect(download.suggestedFilename()).toMatch(/\.pptx$/);

  // 中身を検証: OOXML の必須エントリと、スライド XML にテキストが乗っていること。
  const stream = await download.createReadStream();
  const chunks: Buffer[] = [];
  for await (const chunk of stream) chunks.push(chunk as Buffer);
  const zip = await JSZip.loadAsync(Buffer.concat(chunks));
  expect(zip.file("ppt/presentation.xml")).toBeTruthy();
  const slide1 = zip.file("ppt/slides/slide1.xml");
  expect(slide1).toBeTruthy();
  const xml = await slide1!.async("string");
  // 新規スライドの初期タイトル「無題のスライド」が <a:t> ラン（編集可能テキスト）として存在。
  expect(xml).toContain("無題のスライド");
  expect(xml).toContain("<a:t>");
});
