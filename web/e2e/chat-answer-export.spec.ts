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

  test("Markdown をダウンロードできる（本文と見出しがそのまま入る）", async ({ page }) => {
    const exportButton = await askLong(page);
    await exportButton.click();
    const [download] = await Promise.all([
      page.waitForEvent("download"),
      page.getByTestId("message-export-md").click(),
    ]);
    // 名前（回答の見出し）はノートの下書き名・ダイアログの既定名と同じ組み立て。headless の
    // Chromium は日本語の blob ダウンロード名を "download" にすることがあるので、名前は本文で見る。
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

  test("PDF はその場で回答 1 件だけを印刷し、終わると画面を戻す", async ({ page, context }) => {
    // 印刷ダイアログの代わりに、呼ばれた時点の紙面（印刷層の文字・画面を隠しているか）を記録する。
    await context.addInitScript(() => {
      window.print = () => {
        const root = document.querySelector(".answer-print-root");
        (window as unknown as { __printed: unknown }).__printed = {
          text: root?.textContent ?? "",
          heading: root?.querySelector("h2")?.textContent ?? "",
          hiding: document.body.classList.contains("printing-answer"),
        };
      };
    });
    const exportButton = await askLong(page);
    const threadUrl = page.url();
    await exportButton.click();
    await page.getByTestId("message-export-pdf").click();
    const printed = await expect
      .poll(() => page.evaluate(() => (window as unknown as { __printed?: unknown }).__printed ?? null), {
        timeout: 10_000,
      })
      .not.toBeNull()
      .then(() => page.evaluate(() => (window as unknown as { __printed: Record<string, unknown> }).__printed));
    expect(printed.heading).toBe("第1章");
    expect(printed.text).toContain("段落2。");
    expect(printed.hiding).toBe(true);
    // 新しいタブも遷移も起きず、印刷後は印刷層を外して画面を戻す。
    expect(context.pages()).toHaveLength(1);
    expect(page.url()).toBe(threadUrl);
    await expect(page.getByTestId("answer-print")).toHaveCount(0);
    expect(await page.evaluate(() => document.body.classList.contains("printing-answer"))).toBe(false);
  });
});
