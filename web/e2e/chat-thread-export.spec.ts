import { readFile } from "node:fs/promises";

import { expect, test, type Page } from "@playwright/test";

import { loginViaKeycloak } from "./helpers";

/// 会話全体の書き出し（右上の ⋯ →「会話を書き出し…」）。前提: LLM=stub。
/// `long:2` は「## 第1章」から始まる回答、それ以外は「回答: <質問>」を返す。

async function twoTurns(page: Page) {
  await loginViaKeycloak(page);
  await page.goto("/");
  const input = page.getByLabel("メッセージを入力");
  await input.fill("long:2");
  await page.getByRole("button", { name: "送信" }).click();
  await page.waitForURL(/\/c\/[0-9a-f-]+/i, { timeout: 20_000 });
  await expect(page.getByText("段落2。").first()).toBeVisible({ timeout: 30_000 });
  await expect(page.getByTestId("message-export").last()).toBeVisible({ timeout: 20_000 });
  await input.fill("申請先はどこ");
  await page.getByRole("button", { name: "送信" }).click();
  await expect(page.getByText("回答: 申請先はどこ").first()).toBeVisible({ timeout: 30_000 });
  await expect(page.getByTestId("message-export")).toHaveCount(2, { timeout: 20_000 });
}

async function openExport(page: Page) {
  await expect(async () => {
    await page.keyboard.press("Escape");
    await page.getByTestId("chrome-menu").first().click();
    await page.getByTestId("thread-export").click({ timeout: 2_000 });
  }).toPass({ timeout: 20_000 });
  const dialog = page.getByTestId("thread-export-dialog");
  await expect(dialog).toBeVisible();
  return dialog;
}

async function downloadMd(page: Page): Promise<string> {
  const [download] = await Promise.all([page.waitForEvent("download"), page.getByTestId("thread-export-md").click()]);
  return readFile((await download.path())!, "utf8");
}

test.describe("会話全体の書き出し", () => {
  test("⋯ から開き、質問と回答を順に並べた Markdown をダウンロードできる（質問は外せる）", async ({ page }) => {
    await twoTurns(page);
    const dialog = await openExport(page);
    await expect(dialog.getByText("2 件のやり取り", { exact: false })).toBeVisible();
    // 「質問も含める」は既定でオン。
    await expect(dialog.getByTestId("thread-export-include-questions")).toHaveAttribute("data-state", "checked");

    const withQuestions = await downloadMd(page);
    expect(withQuestions).toContain("## 質問\n\nlong:2\n\n## 回答");
    expect(withQuestions).toContain("段落2。");
    expect(withQuestions).toContain("## 質問\n\n申請先はどこ\n\n## 回答\n\n回答: 申請先はどこ");
    // 質問 → 回答の順（1 件目の回答が 2 件目の質問より前）。
    expect(withQuestions.indexOf("段落2。")).toBeLessThan(withQuestions.indexOf("申請先はどこ"));

    const again = await openExport(page);
    await again.getByTestId("thread-export-include-questions").click();
    const answersOnly = await downloadMd(page);
    expect(answersOnly).not.toContain("## 質問");
    expect(answersOnly.match(/## 回答/g)).toHaveLength(2);
  });

  test("PDF は会話全体をその場で印刷する（やり取りの数だけ並ぶ）", async ({ page, context }) => {
    await context.addInitScript(() => {
      window.print = () => {
        const root = document.querySelector(".answer-print-root");
        (window as unknown as { __printed: unknown }).__printed = {
          turns: root?.querySelectorAll("[data-testid=answer-print-turn]").length ?? 0,
          questions: root?.querySelectorAll("[data-testid=answer-print-question]").length ?? 0,
          text: root?.textContent ?? "",
        };
      };
    });
    await twoTurns(page);
    await openExport(page);
    await page.getByTestId("thread-export-pdf").click();
    await expect
      .poll(() => page.evaluate(() => (window as unknown as { __printed?: unknown }).__printed ?? null), {
        timeout: 10_000,
      })
      .not.toBeNull();
    const printed = await page.evaluate(
      () => (window as unknown as { __printed: { turns: number; questions: number; text: string } }).__printed,
    );
    expect(printed.turns).toBe(2);
    expect(printed.questions).toBe(2);
    expect(printed.text).toContain("Shiki との会話");
    expect(printed.text).toContain("回答: 申請先はどこ");
    await expect(page.getByTestId("answer-print")).toHaveCount(0);
  });

  test("ノートとして開くと、会話全体が 1 つの下書きノートに入る", async ({ page }) => {
    await twoTurns(page);
    await openExport(page);
    await page.getByTestId("thread-export-note").click();
    await page.waitForURL(/\/notes\/draft\?/, { timeout: 20_000 });
    const editor = page.locator(".ProseMirror");
    await expect(editor.getByText("段落2。")).toBeVisible({ timeout: 20_000 });
    await expect(editor.getByText("回答: 申請先はどこ")).toBeVisible();
  });
});
