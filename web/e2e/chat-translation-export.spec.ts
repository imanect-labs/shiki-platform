import { readFile } from "node:fs/promises";

import { expect, test, type Page } from "@playwright/test";

import { loginViaKeycloak } from "./helpers";

/// 書き出しに Web ページの訳文（translation ブロック・#1161）が入る。前提: LLM=stub。
///
/// web_translate は外部ページの取得が要るので、ここでは会話の読み込み（GET /threads/:id/messages）の
/// 応答で、最後の回答に訳文のブロックを足す（書き出しは画面が持つメッセージから組み立てる）。
/// 訳文にはページ由来の記法（```ui・リンク・見出し）を入れ、書き出した先で page モードと同じ扱いに
/// なることを見る。

const PAGE_MD = [
  "## Overview",
  "",
  "これは訳した本文です。[社内ポータル](https://evil.example/login) を見てください。[節へ](#sec)",
  "",
  "```ui",
  "bar 1 2 3",
  "```",
  "",
  "```go",
  "fmt.Println(1)",
  "```",
].join("\n");

const TRANSLATION = {
  source_url: "https://example.com/post",
  title: "Original title",
  translated_title: "訳したタイトル",
  target_lang: "ja",
  markdown: PAGE_MD,
  failed_chunks: [],
  chunks: 1,
  part: 1,
  parts: 2,
  source_images: [],
  source_truncated: false,
  fetched_at: "2026-11-10T00:00:00Z",
};

async function askWithTranslation(page: Page) {
  await loginViaKeycloak(page);
  await page.goto("/");
  const input = page.getByLabel("メッセージを入力");
  await input.fill("申請先はどこ");
  await page.getByRole("button", { name: "送信" }).click();
  await page.waitForURL(/\/c\/[0-9a-f-]+/i, { timeout: 20_000 });
  await expect(page.getByText("回答: 申請先はどこ").first()).toBeVisible({ timeout: 30_000 });
  await expect(page.getByTestId("message-export").last()).toBeVisible({ timeout: 20_000 });
  // 読み直しで、最後の回答に訳文を足す。
  await page.route("**/api/threads/*/messages", async (route) => {
    if (route.request().method() !== "GET") return route.continue();
    const res = await route.fetch();
    const data = (await res.json()) as { messages: { role: string; content: unknown[] }[] };
    const last = [...data.messages].reverse().find((m) => m.role === "assistant");
    last?.content.push({ type: "translation", translation: TRANSLATION });
    await route.fulfill({ response: res, json: data });
  });
  await page.reload();
  await expect(page.getByTestId("translation-block")).toBeVisible({ timeout: 20_000 });
  const exportButton = page.getByTestId("message-export").last();
  await expect(exportButton).toBeVisible({ timeout: 20_000 });
  return exportButton;
}

async function download(page: Page, testId: string): Promise<string> {
  const [file] = await Promise.all([page.waitForEvent("download"), page.getByTestId(testId).click()]);
  return readFile((await file.path())!, "utf8");
}

/// 訳文の節が page モードの規則で書き出されている（`depth` は題名の見出しの深さ）。
function expectPageRules(md: string, depth: number) {
  const h = (n: number) => "#".repeat(n);
  expect(md).toContain(`${h(depth)} 訳したタイトル（1/2）`);
  expect(md).toContain("出典: [example.com](https://example.com/post) の機械翻訳（日本語）");
  // ページの一番上の見出しは題名のすぐ下の段になる（「回答」「出典」の節を装わせない）。
  expect(md).toContain(`${h(depth + 1)} Overview`);
  expect(md).not.toMatch(/^## Overview/m);
  // リンクには行き先のホストを添え、断片だけのリンクは文字にする。
  expect(md).toContain("[社内ポータル](https://evil.example/login)（evil.example）");
  expect(md).not.toContain("](#sec)");
  // ```ui は図にしない（言語名を外す）。ほかの言語名はそのまま。
  expect(md).not.toContain("```ui");
  expect(md).toContain("```\nbar 1 2 3\n```");
  expect(md).toContain("```go\nfmt.Println(1)\n```");
}

test.describe("書き出しに Web ページの訳文が入る（#1161）", () => {
  test("回答 1 件の Markdown に、本文・訳文の順で入る", async ({ page }) => {
    const exportButton = await askWithTranslation(page);
    await exportButton.click();
    const md = await download(page, "message-export-md");
    expect(md.indexOf("回答: 申請先はどこ")).toBeLessThan(md.indexOf("訳したタイトル"));
    expect(md).toContain("これは訳した本文です。");
    expectPageRules(md, 2);
  });

  test("会話全体の Markdown では、訳文が「回答」の節の中に入る", async ({ page }) => {
    await askWithTranslation(page);
    await expect(async () => {
      await page.keyboard.press("Escape");
      await page.getByTestId("chrome-menu").first().click();
      await page.getByTestId("thread-export").click({ timeout: 2_000 });
    }).toPass({ timeout: 20_000 });
    await expect(page.getByTestId("thread-export-dialog")).toBeVisible();
    const md = await download(page, "thread-export-md");
    expect(md).toContain("## 回答\n\n回答: 申請先はどこ\n\n### 訳したタイトル（1/2）");
    expectPageRules(md, 3);
  });

  test("ノートとして開くと、訳文が入り ```ui は図にならない", async ({ page }) => {
    const exportButton = await askWithTranslation(page);
    await exportButton.click();
    await page.getByTestId("message-export-note").click();
    await page.waitForURL(/\/notes\/draft\?/, { timeout: 20_000 });
    expect(new URL(page.url()).searchParams.get("name")).toBe("訳したタイトル");
    const editor = page.locator(".ProseMirror");
    await expect(editor.getByText("これは訳した本文です。", { exact: false })).toBeVisible({ timeout: 20_000 });
    await expect(editor.getByText("bar 1 2 3")).toBeVisible();
    // ページの見出しは、H3 までのノートでも見出しのまま（題名 h2 のすぐ下）。
    await expect(editor.locator("h3", { hasText: "Overview" })).toBeVisible();
    await expect(editor.locator("svg")).toHaveCount(0);
  });

  test("PDF は会話と同じ描き方で訳文を印刷する（コピー・続きの案内は出さない）", async ({ page, context }) => {
    await context.addInitScript(() => {
      window.print = () => {
        const root = document.querySelector(".answer-print-root");
        (window as unknown as { __printed: unknown }).__printed = {
          text: root?.textContent ?? "",
          blocks: root?.querySelectorAll("[data-testid=translation-block]").length ?? 0,
          copy: root?.querySelectorAll("[aria-label=訳文をコピー]").length ?? 0,
          more: root?.querySelectorAll("[data-testid=translation-more]").length ?? 0,
        };
      };
    });
    const exportButton = await askWithTranslation(page);
    await exportButton.click();
    await page.getByTestId("message-export-pdf").click();
    await expect
      .poll(() => page.evaluate(() => (window as unknown as { __printed?: unknown }).__printed ?? null), {
        timeout: 10_000,
      })
      .not.toBeNull();
    const printed = await page.evaluate(
      () =>
        (window as unknown as { __printed: { text: string; blocks: number; copy: number; more: number } }).__printed,
    );
    expect(printed.blocks).toBe(1);
    expect(printed.text).toContain("回答: 申請先はどこ");
    expect(printed.text).toContain("これは訳した本文です。");
    expect(printed.text).toContain("example.com");
    expect(printed.copy).toBe(0);
    expect(printed.more).toBe(0);
    await expect(page.getByTestId("answer-print")).toHaveCount(0);
  });
});
