import { expect, test, type Page } from "@playwright/test";

import { loginAs, loginViaKeycloak, uniqueName } from "./helpers";

/// issue #342「共有リンクの複数発行・個別失効/延長・2タブ化」の受け入れ条件を検証する（LLM=stub）:
/// - 共有ダイアログの「リンク」タブで範囲を選んでリンクを発行でき、発行済み一覧に出る。
/// - 発行した組織内リンクのディープリンクを、同組織の別ユーザーが開ける。
/// - リンクを失効すると、その別ユーザーは開けなくなる。
/// - リンク未発行なら未共有ユーザーは開けない。
/// - パスワード付きリンクは、token 付き URL でも未解錠では開けず、正しいパスワードで開ける。
///
/// alice / bob は同組織（a-corp）、charlie は別組織（b-corp）。

async function createNoteViaApi(page: Page, name: string): Promise<string> {
  return page.evaluate(async (noteName) => {
    const csrf = document.cookie.match(/(?:^|;\s*)shiki_csrf=([^;]+)/)?.[1] ?? "";
    const res = await fetch("/api/notes", {
      method: "POST",
      credentials: "include",
      headers: { "Content-Type": "application/json", "X-CSRF-Token": csrf },
      body: JSON.stringify({ name: noteName, parent_id: null, markdown: "# 見出し\n\n本文です。" }),
    });
    if (!res.ok) throw new Error(`ノート作成に失敗: ${res.status}`);
    return ((await res.json()) as { id: string }).id;
  }, name);
}

async function openNoteSynced(page: Page, nodeId: string) {
  await page.goto(`/notes/${nodeId}`);
  await expect(page.getByTestId("note-sync-status")).toHaveText("同期済み", { timeout: 20_000 });
}

test("組織内リンクを発行→別ユーザーが開ける→失効で開けなくなる", async ({
  page,
  context,
  browser,
}) => {
  await context.grantPermissions(["clipboard-read", "clipboard-write"]);
  await loginViaKeycloak(page); // alice (a-corp)
  const nodeId = await createNoteViaApi(page, uniqueName("sl-org"));
  await openNoteSynced(page, nodeId);

  // 共有ダイアログ → リンクタブ → 組織内で発行。
  await page.getByTestId("note-share").click();
  const dialog = page.getByRole("dialog");
  await dialog.getByTestId("share-tab-links").click();
  await dialog.getByTestId("link-audience-organization").click();
  await dialog.getByTestId("link-create").click();

  // 発行済み一覧に出て、コピーされた URL が当該ノートのディープリンク。
  await expect(dialog.getByTestId("link-item")).toHaveCount(1, { timeout: 10_000 });
  const copied = await page.evaluate(() => navigator.clipboard.readText());
  expect(copied).toContain(`/notes/${nodeId}`);

  // bob（同組織）はそのディープリンクで開ける。
  const bobCtx = await browser.newContext();
  const bobPage = await bobCtx.newPage();
  await loginAs(bobPage, "bob");
  await openNoteSynced(bobPage, nodeId);

  // alice がリンクを失効する。
  await dialog.getByTestId("link-revoke").click();
  await expect(dialog.getByTestId("link-item")).toHaveCount(0, { timeout: 10_000 });

  // 失効後は bob は開けない（存在秘匿の「見つかりません」）。
  await bobPage.goto(`/notes/${nodeId}`);
  await expect(bobPage.getByText("ノートが見つかりません")).toBeVisible({ timeout: 15_000 });
  await bobCtx.close();
});

test("リンク未発行: 未共有ユーザーは開けない", async ({ page, browser }) => {
  await loginViaKeycloak(page); // alice
  const nodeId = await createNoteViaApi(page, uniqueName("sl-none"));
  await openNoteSynced(page, nodeId); // 作成者は開ける

  const bobCtx = await browser.newContext();
  const bobPage = await bobCtx.newPage();
  await loginAs(bobPage, "bob");
  await bobPage.goto(`/notes/${nodeId}`);
  await expect(bobPage.getByText("ノートが見つかりません")).toBeVisible({ timeout: 15_000 });
  await expect(bobPage.getByTestId("note-sync-status")).toHaveCount(0);
  await bobCtx.close();
});

test("パスワード付きリンク: 未解錠は不可・token 解錠後に開ける", async ({ page, context, browser }) => {
  await context.grantPermissions(["clipboard-read", "clipboard-write"]);
  await loginViaKeycloak(page); // alice
  const nodeId = await createNoteViaApi(page, uniqueName("sl-pw"));
  await openNoteSynced(page, nodeId);

  // リンクタブ → 社内全員 + パスワードで発行。
  await page.getByTestId("note-share").click();
  const dialog = page.getByRole("dialog");
  await dialog.getByTestId("share-tab-links").click();
  await dialog.getByTestId("link-audience-anyone").click();
  await dialog.getByTestId("link-password-toggle").click();
  await dialog.getByTestId("link-password").fill("s3cret-pass");
  await dialog.getByTestId("link-create").click();
  await expect(dialog.getByTestId("link-item")).toHaveCount(1, { timeout: 10_000 });

  // コピー URL は解錠ヒント付き（?lt=<token>&unlock=1）。
  const url = await page.evaluate(() => navigator.clipboard.readText());
  expect(url).toContain(`/notes/${nodeId}`);
  expect(url).toContain("lt=");
  expect(url).toContain("unlock=1");
  const linkPath = url.slice(url.indexOf(`/notes/${nodeId}`));

  // bob が token 付き URL で開く。未解錠では開けない（解錠フォームが出る）。
  const bobCtx = await browser.newContext();
  const bobPage = await bobCtx.newPage();
  await loginAs(bobPage, "bob");
  await bobPage.goto(linkPath);
  await expect(bobPage.getByTestId("link-unlock-password")).toBeVisible({ timeout: 15_000 });

  // 誤パスワードはエラー（オラクルにしない一律メッセージ）。
  await bobPage.getByTestId("link-unlock-password").fill("wrong");
  await bobPage.getByTestId("link-unlock-submit").click();
  await expect(bobPage.getByRole("alert")).toBeVisible({ timeout: 10_000 });

  // 正しいパスワードで解錠 → ノートが開く。
  await bobPage.getByTestId("link-unlock-password").fill("s3cret-pass");
  await bobPage.getByTestId("link-unlock-submit").click();
  await expect(bobPage.getByTestId("note-sync-status")).toHaveText("同期済み", { timeout: 20_000 });
  await bobCtx.close();
});
