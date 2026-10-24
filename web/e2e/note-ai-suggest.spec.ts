import { expect, test, type Page } from "@playwright/test";

import { loginViaKeycloak, uniqueName } from "./helpers";

/// ノートの AI 編集の提案モード（#886）: 消す側にも提案の印が付き、棄却すると元の本文に戻り、
/// 承認すると差し替え・節の削除が反映される（前提: LLM=stub の `notesuggest:<node_id>`）。

// 「参考」の節には文字を持たない区切り線も入れる（ブロックの属性に付く印も確かめる）。
const BODY = "# 週次レポート\n\n## 概要\n\n売上は12%増だった。\n\n## 参考\n\n前回のメモを参照。\n\n---\n";

async function createNote(page: Page, name: string): Promise<string> {
  return page.evaluate(
    async ([noteName, markdown]) => {
      const csrf = document.cookie.match(/(?:^|;\s*)shiki_csrf=([^;]+)/)?.[1] ?? "";
      const res = await fetch("/api/notes", {
        method: "POST",
        credentials: "include",
        headers: { "Content-Type": "application/json", "X-CSRF-Token": csrf },
        body: JSON.stringify({ name: noteName, parent_id: null, markdown }),
      });
      if (!res.ok) throw new Error(`ノート作成に失敗: ${res.status}`);
      return ((await res.json()) as { id: string }).id;
    },
    [name, BODY] as const,
  );
}

/// ノートを開き、スタブに提案モードの編集をさせて承認ゲートを通す。
async function suggestEdits(page: Page) {
  const nodeId = await createNote(page, uniqueName("ai-suggest"));
  await page.goto(`/notes/${nodeId}`);
  await expect(page.getByTestId("note-sync-status")).toHaveText("同期済み", { timeout: 20_000 });
  await page.getByTestId("note-ask-ai").click();
  const input = page.getByTestId("note-chat-panel").getByLabel("メッセージを入力");
  await input.fill(`notesuggest:${nodeId}`);
  await input.press("Enter");
  await page.getByRole("button", { name: "承認して続行" }).click({ timeout: 20_000 });
  await expect(page.getByTestId("note-suggestion-bar")).toBeVisible({ timeout: 20_000 });
  return page.locator(".tiptap").first();
}

test("棄却すると、消す提案も入れる提案も元の本文に戻る", async ({ page }) => {
  await loginViaKeycloak(page);
  const editor = await suggestEdits(page);
  // 消す側は印付きで残る（語の単位で「12」→「14」・節は見出しごと）。
  await expect(editor.locator("[data-ai-deletion]", { hasText: "12" })).toBeVisible();
  await expect(editor.locator("[data-ai-suggestion]", { hasText: "14" })).toBeVisible();
  await expect(editor.locator("h2 [data-ai-deletion]", { hasText: "参考" })).toBeVisible();
  await expect(editor.locator("hr[data-ai-deletion]")).toHaveCount(1);

  await page.getByTestId("note-reject-suggestions").click();
  await expect(page.getByTestId("note-suggestion-bar")).toBeHidden();
  await expect(editor.getByText("売上は12%増だった。", { exact: true })).toBeVisible();
  await expect(editor.getByRole("heading", { name: "参考" })).toBeVisible();
  await expect(editor.getByText("前回のメモを参照。")).toBeVisible();
  await expect(editor.locator("hr")).toHaveCount(1);
  await expect(editor.locator("[data-ai-deletion], [data-ai-suggestion]")).toHaveCount(0);
});

test("承認すると、差し替えと節の削除が本文になる", async ({ page }) => {
  await loginViaKeycloak(page);
  const editor = await suggestEdits(page);

  await page.getByTestId("note-accept-suggestions").click();
  await expect(page.getByTestId("note-suggestion-bar")).toBeHidden();
  await expect(editor.getByText("売上は14%増だった。", { exact: true })).toBeVisible();
  // 節は見出しごと消え、空の見出し・段落も残らない。
  await expect(editor.getByRole("heading", { name: "参考" })).toHaveCount(0);
  await expect(editor.getByText("前回のメモを参照。")).toHaveCount(0);
  await expect(editor.locator("h2")).toHaveCount(1);
  await expect(editor.locator("hr")).toHaveCount(0);
  await expect(editor.locator("[data-ai-deletion], [data-ai-suggestion]")).toHaveCount(0);
});
