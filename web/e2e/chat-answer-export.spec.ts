import { readFile } from "node:fs/promises";

import { expect, test, type Page } from "@playwright/test";

import { loginViaKeycloak } from "./helpers";

/// 回答の書き出し（#1048）。フッターの「書き出し」1 ボタンから、ノート・md・PDF・Word を選ぶ。
/// 前提: LLM=stub。`long:2` は「## 第1章」から始まる見出し付きの回答を返す（ファイル名の素になる）。
/// Word（.docx）は変換に ingestion-worker が要るため、ここではダイアログが開くところまでを見る。

async function askLong(page: Page) {
  await loginViaKeycloak(page);
  await page.goto("/");
  const input = page.getByLabel("メッセージを入力");
  await input.fill("long:2");
  await page.getByRole("button", { name: "送信" }).click();
  await page.waitForURL(/\/c\/[0-9a-f-]+/i, { timeout: 20_000 });
  // 生成が終わり、保存済みの回答としてフッターが出るまで待つ（PDF は保存済みの ID が要る）。
  await expect(page.getByText("段落2。").first()).toBeVisible({ timeout: 30_000 });
  const exportButton = page.getByTestId("message-export").last();
  await expect(exportButton).toBeVisible({ timeout: 20_000 });
  return exportButton;
}

test.describe("チャットの回答の書き出し（#1048）", () => {
  test("フッターに共有は無く、書き出しメニューに 5 つの行き先が並ぶ", async ({ page }) => {
    const exportButton = await askLong(page);
    const footer = exportButton.locator("xpath=..");
    await expect(footer.getByRole("button", { name: "コピー" })).toBeVisible();
    await expect(footer.getByRole("button", { name: "共有" })).toHaveCount(0);

    await exportButton.click();
    const menu = page.getByRole("menu");
    await expect(menu.getByText("エクスポート", { exact: true })).toBeVisible();
    await expect(menu.getByText("ダウンロード", { exact: true })).toBeVisible();
    for (const id of ["note", "docx-drive", "md", "docx", "pdf"]) {
      await expect(page.getByTestId(`message-export-${id}`)).toBeVisible();
    }

    // Word としてドライブに保存 → 名前（回答の見出し）と保存先を選ぶダイアログ。
    await page.getByTestId("message-export-docx-drive").click();
    const dialog = page.getByRole("dialog");
    await expect(dialog.getByTestId("save-draft-name")).toHaveValue("第1章");
    await expect(dialog.getByTestId("save-draft-location")).toBeVisible();
    // 出典の無い回答には共有の注意を出さない。
    await expect(dialog.getByTestId("save-draft-notice")).toHaveCount(0);
    await dialog.getByRole("button", { name: "キャンセル" }).click();
    await expect(dialog).toBeHidden();
  });

  test("Markdown をダウンロードできる（名前は回答の見出し）", async ({ page }) => {
    const exportButton = await askLong(page);
    await exportButton.click();
    const [download] = await Promise.all([
      page.waitForEvent("download"),
      page.getByTestId("message-export-md").click(),
    ]);
    expect(download.suggestedFilename()).toBe("第1章.md");
    const body = await readFile((await download.path())!, "utf8");
    expect(body).toContain("## 第1章");
    expect(body).toContain("段落2。");
  });

  test("ノートとして開くと下書きノートに本文が入る", async ({ page }) => {
    const exportButton = await askLong(page);
    const threadUrl = page.url();
    await exportButton.click();
    await page.getByTestId("message-export-note").click();
    await page.waitForURL(/\/notes\/draft\?/, { timeout: 20_000 });
    expect(new URL(page.url()).searchParams.get("name")).toBe("第1章");
    expect(new URL(page.url()).searchParams.get("thread")).toBe(new URL(threadUrl).pathname.split("/").pop());
    await expect(page.locator(".ProseMirror").getByText("段落2。")).toBeVisible({ timeout: 20_000 });
  });

  test("PDF は回答 1 件の印刷ビューを開き、印刷ダイアログを出す", async ({ page, context }) => {
    // 印刷ダイアログの代わりに呼ばれたことだけを記録する。
    await context.addInitScript(() => {
      window.print = () => {
        (window as unknown as { __printed: boolean }).__printed = true;
      };
    });
    const exportButton = await askLong(page);
    await exportButton.click();
    const [popup] = await Promise.all([
      context.waitForEvent("page"),
      page.getByTestId("message-export-pdf").click(),
    ]);
    await popup.waitForLoadState();
    expect(popup.url()).toMatch(/\/c\/[0-9a-f-]+\/print\?m=/i);
    const view = popup.getByTestId("answer-print");
    await expect(view.getByRole("heading", { name: "第1章" })).toBeVisible({ timeout: 20_000 });
    await expect(view.getByText("段落2。")).toBeVisible();
    await expect
      .poll(() => popup.evaluate(() => (window as unknown as { __printed?: boolean }).__printed === true), {
        timeout: 10_000,
      })
      .toBe(true);
  });
});
