import { expect, test, type Page } from "@playwright/test";

import { loginViaKeycloak, uniqueName } from "./helpers";

/// ノートの Markdown ネイティブ化（issue #297）の受け入れ条件を検証する:
/// - live preview: フォーカス行に記法（見出し #・強調 **）が見え、外すと消える
/// - コピー(A): 全選択コピーの text/plain が正規化 Markdown（## / ** / - ）
/// - ペースト(B): 素の Markdown を貼るとブロック（見出し/リスト/引用）へ変換される
/// - 往復: コピーした Markdown をそのまま貼り戻して構造が保たれる
/// - 安全性: ```shiki-embed フェンスの素ペーストは埋め込み化しない（confused-deputy 回避）

// 動画で挙動を確認できるよう録画する（品質確認・レビュー用）。
test.use({ video: "on" });

async function createNoteViaApi(page: Page, name: string): Promise<string> {
  return page.evaluate(async (noteName) => {
    const csrf = document.cookie.match(/(?:^|;\s*)shiki_csrf=([^;]+)/)?.[1] ?? "";
    const res = await fetch("/api/notes", {
      method: "POST",
      credentials: "include",
      headers: { "Content-Type": "application/json", "X-CSRF-Token": csrf },
      body: JSON.stringify({ name: noteName, parent_id: null, markdown: null }),
    });
    if (!res.ok) throw new Error(`ノート作成に失敗: ${res.status}`);
    return ((await res.json()) as { id: string }).id;
  }, name);
}

async function openNote(page: Page, nodeId: string) {
  await page.goto(`/notes/${nodeId}`);
  await expect(page.getByTestId("note-sync-status")).toHaveText("同期済み", {
    timeout: 20_000,
  });
}

const editorLocator = (page: Page) => page.getByTestId("note-editor");

/// text/plain のみのクリップボードで貼り付けイベントを発火する（OS クリップボード非依存）。
/// リッチ経路（text/html）を持たないため handlePaste の Markdown 変換経路を通る。
async function pastePlainMarkdown(page: Page, markdown: string) {
  await page.evaluate((text) => {
    const el = document.querySelector<HTMLElement>('[data-testid="note-editor"]');
    if (!el) throw new Error("エディタ DOM が見つからない");
    el.focus();
    const dt = new DataTransfer();
    dt.setData("text/plain", text);
    el.dispatchEvent(
      new ClipboardEvent("paste", { clipboardData: dt, bubbles: true, cancelable: true }),
    );
  }, markdown);
}

test("live preview: フォーカス行の記法可視化とフォーカス連動", async ({ page }) => {
  await loginViaKeycloak(page);
  await page.goto("/drive");
  const nodeId = await createNoteViaApi(page, uniqueName("md-livepreview"));
  await openNote(page, nodeId);

  const editor = editorLocator(page);
  await editor.click();

  // 素の Markdown を入力: 見出し（## + 空白で入力規則が発火）。
  await editor.pressSequentially("## 設計メモ");
  await expect(editor.locator("h2", { hasText: "設計メモ" })).toBeVisible({ timeout: 15_000 });

  // カーソルは見出し内 → `## ` マーカーが可視化される（Obsidian 風）。
  const marker = editor.locator(".note-md-marker");
  await expect(marker.filter({ hasText: "##" })).toBeVisible();
  await page.screenshot({ path: "test-results/md-livepreview-heading.png" });

  // 強調（**太字**）を別行に入力 → 太字化し、カーソル行で ** マーカーが見える。
  await editor.press("Enter");
  await editor.pressSequentially("これは **重要** な点");
  await expect(editor.locator("strong", { hasText: "重要" })).toBeVisible({ timeout: 15_000 });
  await expect(marker.filter({ hasText: "**" }).first()).toBeVisible();

  // フォーカスを外す（タイトル入力へ）→ 記法マーカーは消える（整形表示に戻る）。
  await page.getByTestId("note-title-input").click();
  await expect(editor.locator(".note-md-marker")).toHaveCount(0);
  await page.screenshot({ path: "test-results/md-livepreview-blurred.png" });
});

test("コピー=Markdown・素 md ペースト=ブロック変換・往復", async ({ page }) => {
  await page.context().grantPermissions(["clipboard-read", "clipboard-write"]);
  await loginViaKeycloak(page);
  await page.goto("/drive");

  // --- ノート1: 内容を作ってコピー（A: text/plain が正規化 Markdown）---
  const note1 = await createNoteViaApi(page, uniqueName("md-copy"));
  await openNote(page, note1);
  const editor = editorLocator(page);
  await editor.click();
  await editor.pressSequentially("## 買い物リスト");
  await editor.press("Enter");
  await editor.pressSequentially("牛乳と **卵** を買う");
  await editor.press("Enter");
  await editor.pressSequentially("- りんご");
  await editor.press("Enter");
  await editor.pressSequentially("みかん");
  await expect(editor.locator("ul li", { hasText: "みかん" })).toBeVisible({ timeout: 15_000 });

  // 全選択してコピー → クリップボード text/plain を読む。
  await editor.press("ControlOrMeta+a");
  await editor.press("ControlOrMeta+c");
  const copied = await page.evaluate(() => navigator.clipboard.readText());
  expect(copied).toContain("## 買い物リスト");
  expect(copied).toContain("**卵**");
  expect(copied).toContain("- りんご");
  expect(copied).toContain("- みかん");

  // --- ノート2: 上でコピーした Markdown を素テキストで貼る（B＋往復）---
  const note2 = await createNoteViaApi(page, uniqueName("md-paste"));
  await openNote(page, note2);
  await editorLocator(page).click();
  await pastePlainMarkdown(page, copied);

  const editor2 = editorLocator(page);
  await expect(editor2.locator("h2", { hasText: "買い物リスト" })).toBeVisible({ timeout: 15_000 });
  await expect(editor2.locator("strong", { hasText: "卵" })).toBeVisible();
  await expect(editor2.locator("ul li", { hasText: "りんご" })).toBeVisible();
  await expect(editor2.locator("ul li", { hasText: "みかん" })).toBeVisible();
  await page.screenshot({ path: "test-results/md-paste-roundtrip.png" });

  // --- 外部由来の素 Markdown を貼る（見出し/リスト/引用/コード）---
  const note3 = await createNoteViaApi(page, uniqueName("md-paste-ext"));
  await openNote(page, note3);
  await editorLocator(page).click();
  await pastePlainMarkdown(
    page,
    "# タイトル\n\n> 引用のブロック\n\n1. 最初\n2. 次\n\n```js\nconst x = 1;\n```\n",
  );
  const editor3 = editorLocator(page);
  await expect(editor3.locator("h1", { hasText: "タイトル" })).toBeVisible({ timeout: 15_000 });
  await expect(editor3.locator("blockquote", { hasText: "引用のブロック" })).toBeVisible();
  await expect(editor3.locator("ol li", { hasText: "最初" })).toBeVisible();
  await expect(editor3.locator("pre", { hasText: "const x = 1;" })).toBeVisible();
});

test("ペースト安全性: shiki-embed フェンスは埋め込み化しない（confused-deputy 回避）", async ({
  page,
}) => {
  await loginViaKeycloak(page);
  await page.goto("/drive");
  const nodeId = await createNoteViaApi(page, uniqueName("md-embed-safe"));
  await openNote(page, nodeId);
  await editorLocator(page).click();

  // ```shiki-embed を素テキストで貼り付けても、埋め込みノードには昇格しない。
  await pastePlainMarkdown(
    page,
    '```shiki-embed\n{"kind":"iframe","src":"https://evil.example/x"}\n```\n',
  );

  const editor = editorLocator(page);
  // 埋め込みは 1 つも生成されない（既存の明示挿入経路のみが埋め込みを作る）。
  await expect(editor.getByTestId("note-embed")).toHaveCount(0);
  // 代わりに無害なコードブロックへ縮退し、ペイロードがテキストとして見える。
  await expect(editor.locator("pre", { hasText: "kind" })).toBeVisible({ timeout: 15_000 });
  // iframe 要素は描画されない。
  await expect(editor.locator("iframe")).toHaveCount(0);
});
