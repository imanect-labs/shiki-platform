import { expect, test, type Page } from "@playwright/test";

import { loginAs, loginViaKeycloak, uniqueName } from "./helpers";

/// ノート（md 共同編集・Task 11P.3）の受け入れ条件を検証する:
/// - スラッシュコマンドで見出し等を挿入できる
/// - メタデータパネル（タイトル・タグ）を編集できる
/// - 編集がリロード後も永続する（Yjs update log）
/// - 2 クライアントの並行編集が収束し、参加者プレゼンスが見える
/// - viewer は読めるが編集できない（読取専用 UI）

/// UI が未提供のノート作成は API で行う（「新規作成 > ノート」は 11P.5）。
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
    const body = (await res.json()) as { id: string };
    return body.id;
  }, name);
}

/// bob（同テナントの別ユーザー）へ指定ロールで共有する。
async function shareViaApi(page: Page, nodeId: string, role: "viewer" | "editor") {
  await page.evaluate(
    async ({ id, shareRole }) => {
      const csrf = document.cookie.match(/(?:^|;\s*)shiki_csrf=([^;]+)/)?.[1] ?? "";
      const res = await fetch(`/api/nodes/${id}/shares`, {
        method: "PUT",
        credentials: "include",
        headers: { "Content-Type": "application/json", "X-CSRF-Token": csrf },
        body: JSON.stringify({
          target: { type: "user", id: "00000000-0000-0000-0000-000000000002" },
          role: shareRole,
        }),
      });
      if (!res.ok) throw new Error(`共有に失敗: ${res.status}`);
    },
    { id: nodeId, shareRole: role },
  );
}

async function openNote(page: Page, nodeId: string) {
  await page.goto(`/notes/${nodeId}`);
  await expect(page.getByTestId("note-sync-status")).toHaveText("同期済み", {
    timeout: 20_000,
  });
}

const editorLocator = (page: Page) => page.getByTestId("note-editor");

test("ノート編集: スラッシュコマンド・メタデータ・リロード永続", async ({ page }) => {
  await loginViaKeycloak(page); // alice
  await page.goto("/drive");
  const noteName = uniqueName("meeting-note");
  const nodeId = await createNoteViaApi(page, noteName);
  await openNote(page, nodeId);

  // タイトル（メタデータパネル → frontmatter 反映は 11P.2 の保存経路）。
  await page.getByTestId("note-title-input").fill("週次ミーティング");

  // タグ追加。
  await page.getByLabel("タグを追加").fill("議事録");
  await page.getByLabel("タグを追加").press("Enter");
  await expect(page.getByText("議事録", { exact: true })).toBeVisible();

  // スラッシュコマンドで見出しを挿入。
  const editor = editorLocator(page);
  await editor.click();
  await page.keyboard.type("/");
  await expect(page.getByTestId("slash-menu")).toBeVisible();
  await page.getByRole("menuitem", { name: /見出し 1/ }).click();
  await page.keyboard.type("アジェンダ");
  await expect(editor.locator("h1", { hasText: "アジェンダ" })).toBeVisible();

  // 本文とチェックリスト。
  await page.keyboard.press("Enter");
  await page.keyboard.type("/チェック");
  await expect(page.getByTestId("slash-menu")).toBeVisible();
  await page.getByRole("menuitem", { name: "チェックリスト" }).click();
  await page.keyboard.type("資料を用意する");
  await expect(
    editor.locator('ul[data-type="taskList"] li', { hasText: "資料を用意する" }),
  ).toBeVisible();

  // リロードしても内容が残る（Yjs update log からの復元）。
  await page.reload();
  await expect(page.getByTestId("note-sync-status")).toHaveText("同期済み", {
    timeout: 20_000,
  });
  await expect(editorLocator(page).locator("h1", { hasText: "アジェンダ" })).toBeVisible();
  await expect(page.getByTestId("note-title-input")).toHaveValue("週次ミーティング");
});

test("共同編集: 2 クライアントの収束とプレゼンス表示", async ({ page, browser }) => {
  await loginViaKeycloak(page); // alice（タブ1）
  await page.goto("/drive");
  const nodeId = await createNoteViaApi(page, uniqueName("collab-note"));
  await openNote(page, nodeId);

  // 同一ユーザーの 2 枚目のタブ（別コンテキストでの再ログインを避け、同 context で開く）。
  const page2 = await page.context().newPage();
  await openNote(page2, nodeId);

  // タブ1 で入力 → タブ2 に反映される（WS ブロードキャスト経由の収束）。
  await editorLocator(page).click();
  await page.keyboard.type("タブ1からの編集です。");
  await expect(editorLocator(page2).getByText("タブ1からの編集です。")).toBeVisible({
    timeout: 15_000,
  });

  // タブ2 で追記 → タブ1 に反映される。
  await editorLocator(page2).click();
  await page2.keyboard.press("End");
  await page2.keyboard.type("タブ2の追記。");
  await expect(editorLocator(page).getByText("タブ2の追記。")).toBeVisible({
    timeout: 15_000,
  });

  // プレゼンス（awareness）に参加者が表示される。
  await expect(page.getByTestId("note-presence")).toBeVisible();
  await page2.close();
  void browser;
});

test("viewer は読めるが編集できない（読取専用 UI・fail-closed）", async ({
  page,
  browser,
}) => {
  await loginViaKeycloak(page); // alice
  await page.goto("/drive");
  const nodeId = await createNoteViaApi(page, uniqueName("readonly-note"));
  await openNote(page, nodeId);
  await editorLocator(page).click();
  await page.keyboard.type("閲覧者に見せる本文");
  await shareViaApi(page, nodeId, "viewer");

  // bob（viewer）は内容を読めるが編集 UI は無効。
  const bobCtx = await browser.newContext();
  const bobPage = await bobCtx.newPage();
  await loginAs(bobPage, "bob");
  await openNote(bobPage, nodeId);
  await expect(bobPage.getByText("閲覧者に見せる本文")).toBeVisible({ timeout: 15_000 });
  await expect(bobPage.getByTestId("note-readonly-badge")).toBeVisible();
  await expect(bobPage.getByTestId("note-editor")).toHaveAttribute(
    "contenteditable",
    "false",
  );
  await bobCtx.close();
});
