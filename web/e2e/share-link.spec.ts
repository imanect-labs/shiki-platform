import { expect, test, type Page } from "@playwright/test";

import { loginAs, loginViaKeycloak, uniqueName } from "./helpers";

/// issue #338「共有リンク発行＋一般アクセス」の受け入れ条件を検証する（LLM=stub）:
/// - 共有ダイアログに「一般アクセス」（制限付き/組織内/全員）＋「リンクをコピー」がある
/// - 組織内アクセスにすると、同組織の別ユーザーがそのリンク（ディープリンク）で開ける
/// - 制限付き（既定）のままなら未共有ユーザーは開けない
/// - パスワード付き一般アクセスは、未解錠では開けず、正しいパスワードで解錠すると開ける
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

test("一般アクセス（組織内）＋リンクコピー: 同組織の別ユーザーが開ける", async ({
  page,
  context,
  browser,
}) => {
  await context.grantPermissions(["clipboard-read", "clipboard-write"]);
  await loginViaKeycloak(page); // alice (a-corp)
  const nodeId = await createNoteViaApi(page, uniqueName("ga-org"));
  await openNoteSynced(page, nodeId);

  // 共有ダイアログを開く。
  await page.getByTestId("note-share").click();
  const dialog = page.getByRole("dialog");
  await expect(dialog).toContainText("アクセスできる範囲");

  // 一般アクセスを「組織内」にして保存する。
  await dialog.getByTestId("ga-level-organization").click();
  await dialog.getByTestId("ga-save").click();
  // toast は aria-live 領域にも複製されるため first() で strict 違反を避ける。
  await expect(page.getByText("共有設定を更新しました。").first()).toBeVisible({
    timeout: 10_000,
  });

  // 「リンクをコピー」でクリップボードへ入る（成功でボタンが「コピーしました」になる）。
  await dialog.getByTestId("copy-link").click();
  await expect(dialog.getByTestId("copy-link")).toContainText("コピーしました");

  // bob（同組織）がそのディープリンクで開ける。
  const bobCtx = await browser.newContext();
  const bobPage = await bobCtx.newPage();
  await loginAs(bobPage, "bob");
  await openNoteSynced(bobPage, nodeId);
  await bobCtx.close();
});

test("制限付き（既定）: 未共有ユーザーは開けない", async ({ page, browser }) => {
  await loginViaKeycloak(page); // alice
  const nodeId = await createNoteViaApi(page, uniqueName("ga-restricted"));
  await openNoteSynced(page, nodeId); // 作成者は開ける

  // 共有せず、bob（同組織だが未共有）がリンクで開こうとすると見つからない。
  const bobCtx = await browser.newContext();
  const bobPage = await bobCtx.newPage();
  await loginAs(bobPage, "bob");
  await bobPage.goto(`/notes/${nodeId}`);
  await expect(bobPage.getByText("ノートが見つかりません")).toBeVisible({ timeout: 15_000 });
  await expect(bobPage.getByTestId("note-sync-status")).toHaveCount(0);
  await bobCtx.close();
});

test("パスワード付き一般アクセス: 未解錠は不可・解錠後に開ける", async ({ page, browser }) => {
  await loginViaKeycloak(page); // alice
  const nodeId = await createNoteViaApi(page, uniqueName("ga-pw"));
  await openNoteSynced(page, nodeId);

  // 一般アクセス=全員 + パスワードを設定して保存する。
  await page.getByTestId("note-share").click();
  const dialog = page.getByRole("dialog");
  await dialog.getByTestId("ga-level-anyone").click();
  await dialog.getByTestId("ga-password-toggle").click();
  await dialog.getByTestId("ga-password").fill("s3cret-pass");
  await dialog.getByTestId("ga-save").click();
  // toast は aria-live 領域にも複製されるため first() で strict 違反を避ける。
  await expect(page.getByText("共有設定を更新しました。").first()).toBeVisible({
    timeout: 10_000,
  });

  // bob が解錠 UI でパスワードを入れて開く。
  const bobCtx = await browser.newContext();
  const bobPage = await bobCtx.newPage();
  await loginAs(bobPage, "bob");
  await bobPage.goto(`/notes/${nodeId}?unlock=1`);
  // 未解錠では開けない（解錠フォームが出る）。
  await expect(bobPage.getByTestId("ga-unlock-password")).toBeVisible({ timeout: 15_000 });

  // 誤パスワードはエラー（オラクルにしない一律メッセージ）。
  await bobPage.getByTestId("ga-unlock-password").fill("wrong");
  await bobPage.getByTestId("ga-unlock-submit").click();
  await expect(bobPage.getByRole("alert")).toBeVisible({ timeout: 10_000 });

  // 正しいパスワードで解錠 → ノートが開く。
  await bobPage.getByTestId("ga-unlock-password").fill("s3cret-pass");
  await bobPage.getByTestId("ga-unlock-submit").click();
  await expect(bobPage.getByTestId("note-sync-status")).toHaveText("同期済み", {
    timeout: 20_000,
  });
  await bobCtx.close();
});
