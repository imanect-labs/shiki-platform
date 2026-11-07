import { expect, test, type Locator, type Page } from "@playwright/test";

import { loginViaKeycloak, uniqueName } from "./helpers";

/// AI によるドライブの保存先指定・移動（#942）の E2E（前提: LLM=stub）。
///
/// - drive.find でフォルダを探し、drive.move（承認カード → 承認）でファイルを移動できる。
/// - 下書き（save_note）に AI が指定した保存先が、保存ダイアログの初期値になる。
/// - 由来ノートのある会話の下書きは、そのノートと同じフォルダが保存先の初期値になる。
///
/// stub 駆動: `calltool:<ツール名> <入力 JSON>` でツールを決定的に呼ぶ / `savenote:<name>` → save_note。

async function csrf(page: Page): Promise<string> {
  return (await page.context().cookies()).find((c) => c.name === "shiki_csrf")?.value ?? "";
}

async function createFolder(page: Page, name: string, parentId?: string): Promise<string> {
  const res = await page.request.post("/api/folders", {
    headers: { "X-CSRF-Token": await csrf(page) },
    data: { name, parent_id: parentId ?? null },
  });
  expect(res.status(), await res.text()).toBeLessThan(300);
  return ((await res.json()) as { id: string }).id;
}

async function createNote(page: Page, name: string, parentId?: string): Promise<string> {
  const res = await page.request.post("/api/notes", {
    headers: { "X-CSRF-Token": await csrf(page) },
    data: { name, parent_id: parentId ?? null, markdown: `# ${name}\n` },
  });
  expect(res.status(), await res.text()).toBe(200);
  return ((await res.json()) as { id: string }).id;
}

async function parentOf(page: Page, fileId: string): Promise<string | null> {
  const res = await page.request.get(`/api/files/${fileId}`);
  expect(res.status()).toBe(200);
  return ((await res.json()) as { parent_id: string | null }).parent_id;
}

/// ノート画面のアシスタントで下書きができたら下書き画面を開く。通常は自動で下書き画面へ遷移する。
/// 遷移しなかったときだけカードから開く。遷移とカードの描画は競合するので、どちらかが起きるまで待ってから判断する
/// （遷移済みのカードを押しに行くと、DOM から外れた要素を待ち続ける）。
async function openDraftFromPanel(page: Page, panel: Locator) {
  const onDraft = () => /\/notes\/draft/.test(page.url());
  const link = panel.getByTestId("note-draft-card").last().getByRole("link", { name: "下書きを開く" });
  await expect
    .poll(async () => onDraft() || (await link.isVisible().catch(() => false)), { timeout: 30_000 })
    .toBe(true);
  if (!onDraft()) await link.click({ timeout: 5_000 }).catch(() => {});
  await page.waitForURL(/\/notes\/draft/, { timeout: 30_000 });
}

async function sendFromHome(page: Page, text: string) {
  await page.goto("/");
  const input = page.getByLabel("メッセージを入力");
  await input.fill(text);
  await page.getByRole("button", { name: "送信" }).click();
}

test("drive.find でフォルダを探し、drive.move は承認してから移動する", async ({ page }) => {
  await loginViaKeycloak(page);
  const folderName = uniqueName("移動先");
  const folderId = await createFolder(page, folderName);
  const noteId = await createNote(page, uniqueName("移動するメモ"));

  // 探す（読み取りのみ・承認なし）。思考プロセスの行に検索した語が出る。
  await sendFromHome(page, `calltool:drive.find {"query":"${folderName}","kind":"folder"}`);
  await page.waitForURL(/\/c\/[0-9a-f-]+/i, { timeout: 20_000 });
  await expect(page.getByText(`ドライブで「${folderName}」を検索しました`).first()).toBeVisible({
    timeout: 30_000,
  });
  // 行の文言だけでは「見つからなかった」も成功に見えるので、モデルへ返した結果に id があることを確かめる。
  const threadId = page.url().match(/\/c\/([0-9a-f-]+)/i)?.[1] ?? "";
  await expect
    .poll(async () => {
      const res = await page.request.get(`/api/threads/${threadId}/messages`);
      return JSON.stringify(await res.json());
    })
    .toContain(`(id: ${folderId})`);

  // 移す（承認ゲート対象）。承認するまでは移動しない。
  const input = page.getByLabel("メッセージを入力");
  await input.fill(
    `calltool:drive.move {"node_id":"${noteId}","destination_folder_id":"${folderId}"}`,
  );
  await page.getByRole("button", { name: "送信" }).click();
  const approve = page.getByRole("button", { name: "承認して続行" });
  await expect(approve).toBeVisible({ timeout: 30_000 });
  expect(await parentOf(page, noteId)).toBeNull();
  await approve.click();
  await expect.poll(() => parentOf(page, noteId), { timeout: 30_000 }).toBe(folderId);
});

test("下書き: AI が指定した保存先が保存ダイアログの初期値になる", async ({ page }) => {
  await loginViaKeycloak(page);
  const folderName = uniqueName("AI指定先");
  const folderId = await createFolder(page, folderName);
  const noteName = uniqueName("指定先つき下書き");
  await sendFromHome(
    page,
    `calltool:save_note {"name":"${noteName}","markdown":"# ${noteName}","parent_id":"${folderId}"}`,
  );
  await page.waitForURL(/\/notes\/draft/, { timeout: 30_000 });
  await expect(
    page.getByTestId("draft-note-editor").getByRole("heading", { name: noteName }),
  ).toBeVisible({ timeout: 20_000 });

  await page.getByTestId("draft-save-button").click();
  await expect(page.getByTestId("save-draft-location")).toContainText(folderName, {
    timeout: 15_000,
  });
  const created = page.waitForResponse(
    (r) => r.url().endsWith("/api/notes") && r.request().method() === "POST",
  );
  await page.getByTestId("save-draft-confirm").click();
  const res = await created;
  expect(((await res.json()) as { parent_id: string | null }).parent_id).toBe(folderId);
});

test("下書き: 由来ノートのある会話はそのノートのフォルダが保存先の初期値になる", async ({
  page,
}) => {
  await loginViaKeycloak(page);
  const folderName = uniqueName("由来フォルダ");
  const folderId = await createFolder(page, folderName);
  const noteId = await createNote(page, uniqueName("由来ノート"), folderId);
  await page.goto(`/notes/${noteId}`);
  await expect(page.getByTestId("note-sync-status")).toHaveText("同期済み", { timeout: 20_000 });
  await page.getByTestId("note-ask-ai").click();
  const panel = page.getByTestId("note-chat-panel");
  const composer = panel.getByLabel("メッセージを入力");
  await expect(composer).toBeVisible({ timeout: 20_000 });
  const draftName = uniqueName("同じ場所に");
  await composer.fill(`savenote:${draftName}`);
  await panel.getByRole("button", { name: "送信" }).click();
  await openDraftFromPanel(page, panel);
  await page.waitForURL(/\/notes\/draft/, { timeout: 30_000 });
  await page.getByTestId("draft-save-button").click();
  await expect(page.getByTestId("save-draft-location")).toContainText(folderName, {
    timeout: 15_000,
  });
  const created = page.waitForResponse(
    (r) => r.url().endsWith("/api/notes") && r.request().method() === "POST",
  );
  await page.getByTestId("save-draft-confirm").click();
  const res = await created;
  expect(((await res.json()) as { parent_id: string | null }).parent_id).toBe(folderId);
});

test("下書き: 保存先の解決中は保存できず、AI の \"root\" 指定は由来ノートのフォルダより優先する", async ({
  page,
}) => {
  await loginViaKeycloak(page);
  const folderName = uniqueName("由来フォルダ2");
  const folderId = await createFolder(page, folderName);
  const noteId = await createNote(page, uniqueName("由来ノート2"), folderId);
  await page.goto(`/notes/${noteId}`);
  await expect(page.getByTestId("note-sync-status")).toHaveText("同期済み", { timeout: 20_000 });
  await page.getByTestId("note-ask-ai").click();
  const panel = page.getByTestId("note-chat-panel");
  const composer = panel.getByLabel("メッセージを入力");
  await expect(composer).toBeVisible({ timeout: 20_000 });
  const draftName = uniqueName("ルートへ");
  await composer.fill(`savenote:${draftName}`);
  await panel.getByRole("button", { name: "送信" }).click();
  await openDraftFromPanel(page, panel);
  await page.waitForURL(/\/notes\/draft/, { timeout: 30_000 });

  // 解決（パンくず）を遅らせても、解決前に保存は押せない（ルートへ誤って作らない）。
  let release: () => void = () => {};
  const gate = new Promise<void>((r) => (release = r));
  await page.route("**/api/nodes/*/breadcrumb", async (route) => {
    await gate;
    await route.continue();
  });
  await page.getByTestId("draft-save-button").click();
  await expect(page.getByTestId("save-draft-location")).toContainText("保存先を確認中");
  await expect(page.getByTestId("save-draft-confirm")).toBeDisabled();
  release();
  await expect(page.getByTestId("save-draft-location")).toContainText(folderName, {
    timeout: 15_000,
  });
  await expect(page.getByTestId("save-draft-confirm")).toBeEnabled();
  await page.unroute("**/api/nodes/*/breadcrumb");
  await page.keyboard.press("Escape");

  // AI が同じ下書きを "root" 指定で流し直すと、由来ノートのフォルダではなくルートが初期値になる。
  const draftComposer = page.getByLabel("メッセージを入力");
  await draftComposer.fill(
    `calltool:save_note {"name":"${draftName}","markdown":"# ${draftName}","parent_id":"root"}`,
  );
  await page.getByRole("button", { name: "送信" }).click();
  await expect(page.getByText(`ノート「${draftName}」の下書きを作成`).nth(1)).toBeVisible({
    timeout: 30_000,
  });
  await page.getByTestId("draft-save-button").click();
  await expect(page.getByTestId("save-draft-location")).toContainText("マイドライブ（ルート）", {
    timeout: 15_000,
  });
});
