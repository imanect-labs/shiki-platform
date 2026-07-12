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
  // CI ランナー負荷下では TipTap+Yjs の反映が既定 5s を超える場合がある（#271 flake）。
  await expect(editor.locator("h1", { hasText: "アジェンダ" })).toBeVisible({
    timeout: 15_000,
  });

  // 本文とチェックリスト。
  await page.keyboard.press("Enter");
  await page.keyboard.type("/チェック");
  await expect(page.getByTestId("slash-menu")).toBeVisible();
  await page.getByRole("menuitem", { name: "チェックリスト" }).click();
  await page.keyboard.type("資料を用意する");
  await expect(
    editor.locator('ul[data-type="taskList"] li', { hasText: "資料を用意する" }),
  ).toBeVisible({ timeout: 15_000 });

  // リロードしても内容が残る（Yjs update log からの復元）。
  await page.reload();
  await expect(page.getByTestId("note-sync-status")).toHaveText("同期済み", {
    timeout: 20_000,
  });
  await expect(editorLocator(page).locator("h1", { hasText: "アジェンダ" })).toBeVisible({
    timeout: 15_000,
  });
  await expect(page.getByTestId("note-title-input")).toHaveValue("週次ミーティング", {
    timeout: 15_000,
  });
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

/// AI サジェスト（document.edit suggest モード・Task 11P.4）の承認/棄却 UI を検証する。
/// バックエンドの LLM はスタブで document.edit を確定的に呼べないため、エディタ拡張
/// （AiSuggestionMark）に提案マークを直接入れ、承認/棄却バーの挙動を検証する。
test("AI サジェスト: 提案の承認と棄却", async ({ page }) => {
  await loginViaKeycloak(page); // alice
  await page.goto("/drive");
  const nodeId = await createNoteViaApi(page, uniqueName("suggest-note"));
  await openNote(page, nodeId);

  const insertSuggestion = (text: string) =>
    page.evaluate((t) => {
      const editor = (window as unknown as { __shikiNoteEditor?: { chain: () => any } })
        .__shikiNoteEditor;
      if (!editor) throw new Error("editor 未公開");
      editor
        .chain()
        .focus()
        .insertContent({ type: "text", text: t, marks: [{ type: "aiSuggestion" }] })
        .run();
    }, text);

  const editor = editorLocator(page);
  await editor.click();
  await insertSuggestion("AI が提案した文章。");

  // 提案バーが出て、提案テキストがマーク付きで表示される。
  await expect(page.getByTestId("note-suggestion-bar")).toBeVisible();
  await expect(
    editor.locator(".note-ai-suggestion", { hasText: "AI が提案した文章。" }),
  ).toBeVisible();

  // 承認 → マークが外れて本文化（バーが消える・テキストは残る）。
  await page.getByTestId("note-accept-suggestions").click();
  await expect(page.getByTestId("note-suggestion-bar")).toHaveCount(0);
  await expect(editor.getByText("AI が提案した文章。")).toBeVisible();
  await expect(editor.locator(".note-ai-suggestion")).toHaveCount(0);

  // もう一度提案 → 棄却でテキストごと消える。
  await insertSuggestion("棄却される提案。");
  await expect(page.getByTestId("note-suggestion-bar")).toBeVisible();
  await page.getByTestId("note-reject-suggestions").click();
  await expect(page.getByTestId("note-suggestion-bar")).toHaveCount(0);
  await expect(editor.getByText("棄却される提案。")).toHaveCount(0);
});

/// ノート×チャット分割ビュー（Task 11P.5）: パネル開閉とスレッド紐付けを検証する。
test("ノート分割ビュー: チャットパネルの開閉とスレッド紐付け", async ({ page }) => {
  await loginViaKeycloak(page); // alice
  await page.goto("/drive");
  const nodeId = await createNoteViaApi(page, uniqueName("split-note"));
  await openNote(page, nodeId);

  // アシスタントパネルを開く → スレッドが自動作成され Conversation が出る。
  await page.getByTestId("note-chat-toggle").click();
  await expect(page.getByTestId("note-chat-panel")).toBeVisible();
  // コンポーザ（メッセージ入力）が見える＝Conversation が載っている。
  await expect(page.getByTestId("note-chat-panel").getByLabel("メッセージを入力")).toBeVisible({
    timeout: 15_000,
  });

  // 閉じる → パネルが消える。エディタは残る。
  await page.getByRole("button", { name: "チャットを閉じる" }).click();
  await expect(page.getByTestId("note-chat-panel")).toHaveCount(0);
  await expect(editorLocator(page)).toBeVisible();
});

/// ドライブ「新規作成 > ノート」からノートを作成してエディタへ遷移できる（Task 11P.5）。
test("ドライブ: 新規作成ノート → エディタ遷移", async ({ page }) => {
  await loginViaKeycloak(page); // alice
  await page.goto("/drive");
  await page.getByRole("button", { name: "新規作成" }).click();
  await page.getByTestId("new-note").click();
  // ノートページへ遷移し、エディタが同期完了する。
  await page.waitForURL(/\/notes\//, { timeout: 15_000 });
  await expect(page.getByTestId("note-sync-status")).toHaveText("同期済み", {
    timeout: 20_000,
  });
  await expect(editorLocator(page)).toBeVisible();
});

/// fail-closed（Task 11P.5）: スレッド閲覧権限のないノート共同編集者にスレッド内容が漏れない。
test("分割ビュー fail-closed: スレッド非共有では会話が見えない", async ({ page, browser }) => {
  await loginViaKeycloak(page); // alice
  await page.goto("/drive");
  const nodeId = await createNoteViaApi(page, uniqueName("failclosed-note"));
  await openNote(page, nodeId);

  // alice がチャットパネルを開いてスレッドを作成（thread_id が meta に入る）。
  await page.getByTestId("note-chat-toggle").click();
  await expect(page.getByTestId("note-chat-panel").getByLabel("メッセージを入力")).toBeVisible({
    timeout: 15_000,
  });

  // bob をノートの editor に共有する（スレッドは共有しない＝別 ReBAC）。
  await shareViaApi(page, nodeId, "editor");

  // bob はノートを編集できるが、チャットパネルを開いてもスレッド内容は見えない（fail-closed）。
  const bobCtx = await browser.newContext();
  const bobPage = await bobCtx.newPage();
  await loginAs(bobPage, "bob");
  await openNote(bobPage, nodeId);
  await bobPage.getByTestId("note-chat-toggle").click();
  await expect(bobPage.getByTestId("note-chat-panel")).toBeVisible();
  // Conversation は 403/404 を fail-closed で「見つかりません」に落とす。
  await expect(bobPage.getByText("この会話は見つかりませんでした。")).toBeVisible({
    timeout: 15_000,
  });
  await bobCtx.close();
});

/// 埋め込みブロック（Task 11P.6）: 3 種の描画・往復・生 HTML 非実行を検証する。
test("埋め込み: 3 種の描画・md 往復・生 HTML 非実行", async ({ page }) => {
  await loginViaKeycloak(page); // alice
  await page.goto("/drive");
  const nodeId = await createNoteViaApi(page, uniqueName("embed-note"));
  await openNote(page, nodeId);
  const editor = editorLocator(page);
  await editor.click();

  // エディタ API で 3 種の埋め込みを挿入する（スラッシュの prompt を介さず確定的に）。
  const insertEmbed = (payload: unknown) =>
    page.evaluate((p) => {
      const editor = (window as unknown as { __shikiNoteEditor?: { chain: () => any } })
        .__shikiNoteEditor;
      if (!editor) throw new Error("editor 未公開");
      editor.chain().focus().insertShikiEmbed(p).run();
    }, payload);

  await insertEmbed({
    kind: "genui",
    spec: { version: 1, actions: [], root: { component: "text", text: "GenUI 埋め込み" } },
  });
  await insertEmbed({ kind: "iframe", src: "https://example.com/app" });
  // drive は存在しない node → アクセス不可プレースホルダ（作成者の権限を借用しない）。
  await insertEmbed({ kind: "drive", node_id: "00000000-0000-0000-0000-000000000abc" });

  // genui はシキコンポーネントとして描画される（HTML 実行なし）。
  await expect(editor.getByTestId("embed-genui")).toBeVisible();
  await expect(editor.getByText("GenUI 埋め込み")).toBeVisible();
  // iframe は sandbox 属性付き・same-origin なし（別オリジン分離）。
  const iframe = editor.getByTestId("embed-iframe").locator("iframe");
  await expect(iframe).toHaveAttribute("sandbox", /allow-scripts/);
  await expect(iframe).not.toHaveAttribute("sandbox", /allow-same-origin/);
  await expect(iframe).toHaveAttribute("src", "https://example.com/app");
  // drive は閲覧者本人の権限で解決 → 権限なしはプレースホルダに落ちる（漏洩しない）。
  await expect(editor.getByText("表示できない埋め込みです（アクセス権がありません）。")).toBeVisible({
    timeout: 15_000,
  });

  // リロード後も埋め込みが残る（Yjs→md→Yjs 往復で壊れない）。
  await page.reload();
  await expect(page.getByTestId("note-sync-status")).toHaveText("同期済み", { timeout: 20_000 });
  await expect(editorLocator(page).getByTestId("embed-genui")).toBeVisible();
  await expect(editorLocator(page).getByTestId("embed-iframe").locator("iframe")).toBeVisible();
});

/// XSS negative（Task 11P.6）: どの流入経路でも生 HTML/script が実行されない。
test("埋め込み XSS: 生 HTML は全流入経路で実行されない", async ({ page }) => {
  await loginViaKeycloak(page); // alice
  await page.goto("/drive");

  // 流入経路①: note_ref 保存相当（POST /notes で md に script を含めて作成）。
  // 流入経路②: ファイル側 md 直接書込→インポート（同じ POST /notes 経路で検証）。
  const malicious =
    "# 見出し\n\n<script>window.__xss_fired = true;</script>\n\n通常の <b onclick=alert(1)>本文</b>。\n";
  const nodeId = await page.evaluate(async (md) => {
    const csrf = document.cookie.match(/(?:^|;\s*)shiki_csrf=([^;]+)/)?.[1] ?? "";
    const res = await fetch("/api/notes", {
      method: "POST",
      credentials: "include",
      headers: { "Content-Type": "application/json", "X-CSRF-Token": csrf },
      body: JSON.stringify({ name: `xss-${Date.now()}`, parent_id: null, markdown: md }),
    });
    return (await res.json()).id as string;
  }, malicious);

  await openNote(page, nodeId);
  // script は実行されない（グローバルフラグが立たない）。
  const fired = await page.evaluate(() => (window as unknown as { __xss_fired?: boolean }).__xss_fired ?? false);
  expect(fired).toBe(false);
  // 生 HTML はコードブロック等へ縮退し、<script> タグはレンダリングされない。
  await expect(page.locator("script:has-text('__xss_fired')")).toHaveCount(0);
  // 見出しは正しく残る（本文は壊さない）。
  await expect(editorLocator(page).locator("h1", { hasText: "見出し" })).toBeVisible();

  // 流入経路③: エディタ貼り付け相当（script を含む HTML をエディタ内容として挿入）。
  await editorLocator(page).click();
  await page.evaluate(() => {
    const editor = (window as unknown as { __shikiNoteEditor?: { chain: () => any } })
      .__shikiNoteEditor;
    editor?.chain().focus().insertContent("<script>window.__xss_fired = true;</script>").run();
  });
  await page.waitForTimeout(500);
  const fired2 = await page.evaluate(() => (window as unknown as { __xss_fired?: boolean }).__xss_fired ?? false);
  expect(fired2).toBe(false);
});
