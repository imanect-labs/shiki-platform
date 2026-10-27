import { expect, test, type Page } from "@playwright/test";

import { loginViaKeycloak, uniqueName } from "./helpers";

/// issue #282「会話→ドキュメント」の受け入れ条件を検証する（前提: LLM=stub）:
/// - チャットで save_note（下書き）→ 下書きノート画面へ遷移し本文が seed される
/// - 「ドライブに保存」でノート実体化し、その会話が「ノート由来」になる（サイドバー履歴）
/// - 同じ会話で複数の下書き（別名）をタブで切り替えられる
/// - ノート分割ビューで会話を切替え・新しい会話（リセット）を作れる
/// - AI が図（```ui）をノート本文へ自動挿入できる（document.embed・確認カード無し）
///
/// stub 駆動: `savenote:<name>` → save_note（下書き）/ `docembed:<node_id>` → document.embed。

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

async function createFolderViaApi(page: Page, name: string, parentId?: string): Promise<string> {
  const csrf = (await page.context().cookies()).find((c) => c.name === "shiki_csrf")?.value ?? "";
  const res = await page.request.post("/api/folders", {
    headers: { "X-CSRF-Token": csrf },
    data: { name, parent_id: parentId ?? null },
  });
  expect(res.status(), await res.text()).toBeLessThan(300);
  return ((await res.json()) as { id: string }).id;
}

/// ホームから会話を開始して text を送信する。
async function sendFromHome(page: Page, text: string) {
  await page.goto("/");
  const input = page.getByLabel("メッセージを入力");
  await input.fill(text);
  await page.getByRole("button", { name: "送信" }).click();
}

async function openNote(page: Page, nodeId: string) {
  await page.goto(`/notes/${nodeId}`);
  await expect(page.getByTestId("note-sync-status")).toHaveText("同期済み", { timeout: 20_000 });
}

test("下書き: チャット→下書き画面→ドライブに保存で会話がノート由来になる", async ({ page }) => {
  await loginViaKeycloak(page); // alice
  const noteName = uniqueName("下書き");
  await sendFromHome(page, `savenote:${noteName}`);

  // 下書きカードが出て、下書きノート画面へ遷移する（自動）。
  await page.waitForURL(/\/notes\/draft/, { timeout: 25_000 });
  await expect(page.getByTestId("draft-badge")).toBeVisible();
  const editor = page.getByTestId("draft-note-editor");
  await expect(editor).toBeVisible({ timeout: 20_000 });
  // stub が入れた本文（見出し = 下書き名）が seed されている。
  await expect(editor.getByRole("heading", { name: noteName })).toBeVisible({ timeout: 15_000 });

  // 「ドライブに保存」→ ダイアログ（既定=ルート）→ 保存。
  await page.getByTestId("draft-save-button").click();
  await expect(page.getByTestId("save-draft-name")).toHaveValue(noteName);
  await page.getByTestId("save-draft-confirm").click();

  // 実体化したノートへ遷移し、本文が残る。
  await page.waitForURL(/\/notes\/[0-9a-f-]{36}/i, { timeout: 25_000 });
  await expect(page.getByTestId("note-sync-status")).toHaveText("同期済み", { timeout: 20_000 });
  await expect(page.getByTestId("note-editor").getByRole("heading", { name: noteName })).toBeVisible(
    { timeout: 15_000 },
  );

  // この会話が「ノート由来」としてサイドバー履歴に出る（ノートへのリンク）。
  // 没入エディタではサイドバーがレールに畳まれているため、まず開いてから確認する。
  const noteId = page.url().match(/\/notes\/([0-9a-f-]{36})/i)?.[1] ?? "";
  await page.getByRole("button", { name: "サイドバーを開く" }).click();
  await expect(
    page.locator(`a[href^="/notes/${noteId}"]`).filter({ has: page.locator("svg") }).first(),
  ).toBeVisible({ timeout: 15_000 });

  // 保存直後の遷移（?thread=）で余分な会話を作らない（1:N 初期化レースの回帰防止）。
  // ?thread 指定でアシスタントは開いた状態。会話数バッジ（2 以上のとき表示）が出ないこと。
  await expect(page.getByTestId("note-conversation-switcher")).toBeVisible({ timeout: 15_000 });
  await page.getByTestId("note-conversation-switcher").getByRole("button").first().click();
  await expect(page.getByRole("menuitem", { name: /会話 1/ })).toBeVisible();
  await expect(page.getByRole("menuitem", { name: /会話 2/ })).toHaveCount(0);
});

test("下書き: 保存先にサブフォルダ（ルート以外）を選んで保存できる", async ({ page }) => {
  await loginViaKeycloak(page); // alice
  const parentName = uniqueName("保存先親");
  const childName = uniqueName("保存先子");
  const parentId = await createFolderViaApi(page, parentName);
  const childId = await createFolderViaApi(page, childName, parentId);
  const noteName = uniqueName("下書き保存先");
  await sendFromHome(page, `savenote:${noteName}`);
  await page.waitForURL(/\/notes\/draft/, { timeout: 25_000 });
  await expect(
    page.getByTestId("draft-note-editor").getByRole("heading", { name: noteName }),
  ).toBeVisible({ timeout: 20_000 });

  // 保存ダイアログ → 保存先ピッカーで「親 → 子」へ降りて子を選ぶ。
  await page.getByTestId("draft-save-button").click();
  await page.getByTestId("save-draft-location").click();
  const picker = page.getByRole("dialog", { name: "フォルダを選択" });
  await picker.getByRole("button", { name: parentName, exact: true }).click();
  await picker.getByRole("button", { name: childName, exact: true }).click();
  await picker.getByRole("button", { name: `「${childName}」を選択` }).click();
  await expect(picker).toBeHidden();
  await expect(page.getByTestId("save-draft-location")).toContainText(childName);

  const created = page.waitForResponse(
    (r) => r.url().endsWith("/api/notes") && r.request().method() === "POST",
  );
  await page.getByTestId("save-draft-confirm").click();
  const res = await created;
  expect(res.status(), await res.text()).toBe(200);
  expect(((await res.json()) as { parent_id: string | null }).parent_id).toBe(childId);
  await page.waitForURL(/\/notes\/[0-9a-f-]{36}/i, { timeout: 25_000 });
});

test("下書き: 同じ会話で複数の下書きをタブで切り替える", async ({ page }) => {
  await loginViaKeycloak(page);
  const nameA = uniqueName("予算");
  const nameB = uniqueName("議事録");
  await sendFromHome(page, `savenote:${nameA}`);
  await page.waitForURL(/\/notes\/draft/, { timeout: 25_000 });
  await expect(page.getByTestId("draft-note-editor").getByRole("heading", { name: nameA })).toBeVisible(
    { timeout: 15_000 },
  );

  // 同じ会話でもう 1 本（別名）を作る → タブが 2 つになり、新しい方がアクティブ。
  const input = page.getByLabel("メッセージを入力");
  await input.fill(`savenote:${nameB}`);
  await page.getByRole("button", { name: "送信" }).click();
  const tabs = page.getByTestId("draft-tabs");
  await expect(tabs).toBeVisible({ timeout: 20_000 });
  await expect(tabs.getByText(nameA)).toBeVisible();
  await expect(tabs.getByText(nameB)).toBeVisible();
  await expect(page.getByTestId("draft-note-editor").getByRole("heading", { name: nameB })).toBeVisible(
    { timeout: 15_000 },
  );

  // タブで最初の下書きへ戻れる。
  await tabs.getByText(nameA).click();
  await expect(page.getByTestId("draft-note-editor").getByRole("heading", { name: nameA })).toBeVisible(
    { timeout: 15_000 },
  );
});

test("分割ビュー: 会話の切替と「新しい会話」（1:N・旧会話は残る）", async ({ page }) => {
  await loginViaKeycloak(page);
  const nodeId = await createNoteViaApi(page, uniqueName("split-1n"));
  await openNote(page, nodeId);

  // アシスタントを開くだけでは会話を作らない（スイッチャは出ない）。最初の発話で作られる。
  await page.getByTestId("note-ask-ai").click();
  const input = page.getByTestId("note-chat-panel").getByLabel("メッセージを入力");
  await expect(input).toBeVisible({ timeout: 20_000 });
  await expect(page.getByTestId("note-conversation-switcher")).toHaveCount(0);
  await input.fill("最初の会話");
  await input.press("Enter");
  await expect(page.getByTestId("note-conversation-switcher")).toBeVisible({ timeout: 20_000 });

  // 「新しい会話」→ 入力欄だけ出る（押しただけでは作らない＝スイッチャは 1 件のまま）。
  await page.getByTestId("note-new-conversation").click();
  await expect(page.getByTestId("note-conversation-switcher")).toContainText("新しい会話");
  await expect(page.getByTestId("note-new-conversation")).toHaveCount(0);
  await page.getByTestId("note-conversation-switcher").getByRole("button").first().click();
  await expect(page.getByRole("menuitem", { name: /会話 2/ })).toHaveCount(0);
  await page.keyboard.press("Escape");

  // 送ると新スレッドになる（旧会話は履歴に残る＝スイッチャに 2 件）。
  await input.fill("二つ目の会話");
  await input.press("Enter");
  await expect(page.getByTestId("note-conversation-switcher")).toContainText("会話 2", {
    timeout: 20_000,
  });
  // スイッチャのバッジ（会話数）が 2 になる。
  await page.getByTestId("note-conversation-switcher").getByRole("button").first().click();
  await expect(page.getByRole("menuitem", { name: /会話 1/ })).toBeVisible();
  await expect(page.getByRole("menuitem", { name: /会話 2/ })).toBeVisible();
});

test("図の挿入: AI が ```ui の図をノート本文へ自動挿入する（document.embed）", async ({ page }) => {
  await loginViaKeycloak(page);
  const nodeId = await createNoteViaApi(page, uniqueName("embed-ai"));
  await openNote(page, nodeId);
  await page.getByTestId("note-ask-ai").click();
  const composer = page.getByTestId("note-chat-panel").getByLabel("メッセージを入力");
  await expect(composer).toBeVisible({ timeout: 20_000 });

  // アシスタントに「グラフを入れて」= stub の docembed で document.embed を駆動する。
  await composer.fill(`docembed:${nodeId}`);
  await page.getByTestId("note-chat-panel").getByRole("button", { name: "送信" }).click();

  // 本文に ```ui の図（bar）が現れる（確認カード無し・自動）。Yjs ブロードキャストで反映。
  const figure = page.getByTestId("note-editor").getByTestId("ui-figure");
  await expect(figure).toBeVisible({ timeout: 30_000 });
  await expect(figure).toHaveAttribute("data-ui-kind", "bar");
  await expect(figure).toContainText("月別の売上");
});
