import { test, expect, type Page } from "@playwright/test";

import { loginViaKeycloak, uniqueName } from "../e2e/helpers";

/// #342 共有リンク（2タブ）スクショ用。alice でノートを作り、共有ダイアログの権限タブ／
/// リンクタブ／発行後一覧を 2x・ライト/ダークで撮る。前提: shiki-server(:8080)+keycloak(:8081)+web(:3000)。

const SHOT_DIR = "e2e-demo/shots";

async function createNote(page: Page, name: string): Promise<string> {
  return page.evaluate(async (noteName) => {
    const csrf = document.cookie.match(/(?:^|;\s*)shiki_csrf=([^;]+)/)?.[1] ?? "";
    const res = await fetch("/api/notes", {
      method: "POST",
      credentials: "include",
      headers: { "Content-Type": "application/json", "X-CSRF-Token": csrf },
      body: JSON.stringify({ name: noteName, parent_id: null, markdown: "# 共有リンク デモ\n\n本文です。" }),
    });
    if (!res.ok) throw new Error(`ノート作成に失敗: ${res.status}`);
    return ((await res.json()) as { id: string }).id;
  }, name);
}

for (const theme of ["light", "dark"] as const) {
  test(`share-dialog shots (${theme})`, async ({ browser }) => {
    const context = await browser.newContext({
      deviceScaleFactor: 2,
      viewport: { width: 1280, height: 900 },
      colorScheme: theme,
      permissions: ["clipboard-read", "clipboard-write"],
    });
    // next-themes を明示テーマに固定（class strategy）。
    await context.addInitScript((t) => {
      try {
        window.localStorage.setItem("theme", t as string);
      } catch {
        /* noop */
      }
    }, theme);
    const page = await context.newPage();
    await loginViaKeycloak(page); // alice

    const nodeId = await createNote(page, uniqueName("共有デモ"));
    await page.goto(`/notes/${nodeId}`);
    await expect(page.getByTestId("note-sync-status")).toHaveText("同期済み", { timeout: 25_000 });

    // 共有ダイアログを開く（既定=権限タブ）。
    await page.getByTestId("note-share").click();
    const dialog = page.getByRole("dialog");
    await expect(dialog.getByTestId("share-tab-links")).toBeVisible();
    await page.waitForTimeout(400);
    await dialog.screenshot({ path: `${SHOT_DIR}/permissions-${theme}.png` });

    // リンクタブ（発行フォーム）。
    await dialog.getByTestId("share-tab-links").click();
    await expect(dialog.getByTestId("link-create")).toBeVisible();
    await page.waitForTimeout(400);
    await dialog.screenshot({ path: `${SHOT_DIR}/links-form-${theme}.png` });

    // 組織内リンクを発行 → 一覧に出る。
    await dialog.getByTestId("link-audience-organization").click();
    await dialog.getByTestId("link-create").click();
    await expect(dialog.getByTestId("link-item")).toHaveCount(1, { timeout: 10_000 });
    // もう1本（社内全員＋パスワード）発行して一覧を賑やかに。
    await dialog.getByTestId("link-audience-anyone").click();
    await dialog.getByTestId("link-password-toggle").click();
    await dialog.getByTestId("link-password").fill("demo-pass");
    await dialog.getByTestId("link-create").click();
    await expect(dialog.getByTestId("link-item")).toHaveCount(2, { timeout: 10_000 });
    await page.waitForTimeout(400);
    await dialog.screenshot({ path: `${SHOT_DIR}/links-list-${theme}.png` });

    await context.close();
  });
}
