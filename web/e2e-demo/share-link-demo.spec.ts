import { expect, test, type BrowserContext, type Page } from "@playwright/test";

import { loginAs, loginViaKeycloak, uniqueName } from "../e2e/helpers";

/// #338 共有リンク／一般アクセスの網羅デモ（録画用・単一テストで状態をローカル共有）。
/// alice(a-corp) がオーナーとして 制限付き→組織内→全員+期限+パスワード を設定しリンクを発行、
/// bob(a-corp) が受信者としてリンクで開く（組織内で開ける / 制限付きは拒否 / パスワード解錠）。
///
/// 録画は各コンテキストごとに保存され、テスト後にパスを出力する。

const VIDEO_DIR = "demo-videos";

async function createNote(page: Page, name: string): Promise<string> {
  return page.evaluate(async (noteName) => {
    const csrf = document.cookie.match(/(?:^|;\s*)shiki_csrf=([^;]+)/)?.[1] ?? "";
    const res = await fetch("/api/notes", {
      method: "POST",
      credentials: "include",
      headers: { "Content-Type": "application/json", "X-CSRF-Token": csrf },
      body: JSON.stringify({ name: noteName, parent_id: null, markdown: "# デモ\n\n共有リンクの検証用ノートです。" }),
    });
    if (!res.ok) throw new Error(`ノート作成に失敗: ${res.status}`);
    return ((await res.json()) as { id: string }).id;
  }, name);
}

async function openSynced(page: Page, id: string) {
  await page.goto(`/notes/${id}`);
  await expect(page.getByTestId("note-sync-status")).toHaveText("同期済み", { timeout: 25_000 });
}

const pause = (p: Page, ms = 900) => p.waitForTimeout(ms);

/// 一般アクセスを保存し、PUT 完了と成功トーストを待つ（堅牢化）。
async function saveGeneralAccess(page: Page, dialog: ReturnType<Page["getByRole"]>) {
  const put = page.waitForResponse(
    (r) => r.url().includes("/general-access") && r.request().method() === "PUT" && r.ok(),
    { timeout: 15_000 },
  );
  await dialog.getByTestId("ga-save").click();
  await put;
  await expect(page.getByText("リンク設定を更新しました。").first()).toBeVisible({ timeout: 10_000 });
}

test("共有リンク／一般アクセスの網羅デモ", async ({ browser }) => {
  test.setTimeout(180_000);
  const org = uniqueName("組織内共有");
  const pwName = uniqueName("パスワード保護");
  const restricted = uniqueName("制限付き");
  const ids: Record<string, string> = {};

  // ============ オーナー（alice） ============
  const aliceCtx: BrowserContext = await browser.newContext({
    permissions: ["clipboard-read", "clipboard-write"],
    recordVideo: { dir: `${VIDEO_DIR}/owner` },
  });
  const alice = await aliceCtx.newPage();
  await loginViaKeycloak(alice);
  ids.org = await createNote(alice, org);
  ids.pw = await createNote(alice, pwName);
  ids.restricted = await createNote(alice, restricted);

  // ① 組織内アクセス + リンクコピー
  await openSynced(alice, ids.org);
  await pause(alice);
  await alice.getByTestId("note-share").click();
  const d1 = alice.getByRole("dialog");
  await pause(alice); // メイン（相手を追加）を映す
  await d1.getByTestId("link-settings-open").click(); // 歯車 → リンク設定へ遷移
  await expect(d1).toContainText("リンクの設定");
  await pause(alice);
  await d1.getByTestId("ga-level-organization").click();
  await pause(alice);
  await saveGeneralAccess(alice, d1);
  await pause(alice);
  await d1.getByTestId("copy-link").click();
  await expect(d1.getByTestId("copy-link")).toContainText("コピーしました");
  await pause(alice, 1300);
  await alice.keyboard.press("Escape");

  // ② 全員アクセス + 有効期限 + パスワード
  await openSynced(alice, ids.pw);
  await pause(alice);
  await alice.getByTestId("note-share").click();
  const d2 = alice.getByRole("dialog");
  await d2.getByTestId("link-settings-open").click();
  await pause(alice);
  await d2.getByTestId("ga-level-anyone").click();
  await pause(alice);
  await d2.getByTestId("ga-expiry").fill("2026-12-31");
  await pause(alice);
  await d2.getByTestId("ga-password-toggle").click();
  await d2.getByTestId("ga-password").fill("shiki-demo-pw");
  await pause(alice);
  await saveGeneralAccess(alice, d2);
  await pause(alice);
  await d2.getByTestId("copy-link").click();
  await expect(d2.getByTestId("copy-link")).toContainText("コピーしました");
  await pause(alice, 1600);
  await aliceCtx.close();

  // ============ 受信者（bob） ============
  const bobCtx: BrowserContext = await browser.newContext({
    recordVideo: { dir: `${VIDEO_DIR}/recipient` },
  });
  const bob = await bobCtx.newPage();
  await loginAs(bob, "bob");

  // ① 組織内アクセスのノートはリンクで開ける
  await openSynced(bob, ids.org);
  await pause(bob, 1400);

  // ② 制限付き（未共有）は開けない（存在秘匿の「見つかりません」＋解錠フォーム）
  await bob.goto(`/notes/${ids.restricted}`);
  await expect(bob.getByText("ノートが見つかりません")).toBeVisible({ timeout: 15_000 });
  await expect(bob.getByTestId("ga-unlock-password")).toBeVisible();
  await pause(bob, 1500);

  // ③ パスワード付きは解錠して開く（誤入力→エラー→正入力→開く）
  await bob.goto(`/notes/${ids.pw}?unlock=1`);
  await expect(bob.getByTestId("ga-unlock-password")).toBeVisible({ timeout: 15_000 });
  await pause(bob);
  await bob.getByTestId("ga-unlock-password").fill("wrong-password");
  await bob.getByTestId("ga-unlock-submit").click();
  await expect(bob.getByRole("alert")).toBeVisible({ timeout: 10_000 });
  await pause(bob, 1300);
  await bob.getByTestId("ga-unlock-password").fill("shiki-demo-pw");
  await bob.getByTestId("ga-unlock-submit").click();
  await expect(bob.getByTestId("note-sync-status")).toHaveText("同期済み", { timeout: 25_000 });
  await pause(bob, 1800);
  await bobCtx.close();
});
