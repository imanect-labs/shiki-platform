import { expect, test } from "@playwright/test";

import { loginViaKeycloak, uniqueName } from "./helpers";

/// ノートの「選択→AI 依頼」で document.edit（要確認・破壊系）が human-in-the-loop の
/// 承認カードを出し、承認すると本物のパイプライン（CollabHub::apply_ai_edit・Yjs）で本文が
/// ライブに書き換わることを検証する（前提: LLM=stub）。
///
/// 回帰対象:
/// - 通常チャット（非自律）でも承認者を配線する（未配線だと要確認ツールが常に拒否され共同編集不能）。
/// - SSE を逆プロキシがバッファしない（開いたままの run の承認イベントが live で届く・no-transform）。
///
/// stub 駆動: note_selection の node_id ＋編集キーワードで document.edit（append）を呼ぶ。

async function createNoteViaApi(page: import("@playwright/test").Page, name: string): Promise<string> {
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

test("選択→AI: document.edit が承認ゲートを経て本文をライブ編集する", async ({ page }) => {
  await loginViaKeycloak(page);
  const nodeId = await createNoteViaApi(page, uniqueName("ai-edit"));
  await page.goto(`/notes/${nodeId}`);
  await expect(page.getByTestId("note-sync-status")).toHaveText("同期済み", { timeout: 20_000 });

  // 本文を入力する（TipTap ＋ Yjs）。
  const editor = page.locator(".tiptap").first();
  await editor.click();
  await page.keyboard.type("# 週次レポート\n\n売上は好調に推移。西日本エリアは横ばいで課題。\n");

  // アシスタントを開く。
  await page.getByTestId("note-ask-ai").click();

  // 本文の一部を選択 → 自動でチャットへ選択チップが挿入され node_id が渡る。
  await editor.getByText("西日本エリアは横ばい", { exact: false }).click({ clickCount: 3 });
  await expect(page.getByTestId("selection-chip")).toBeVisible({ timeout: 10_000 });

  // 依頼を送る（編集キーワードを含む）→ stub が document.edit を呼ぶ。
  const input = page.getByTestId("note-chat-panel").getByLabel("メッセージを入力");
  await input.fill("この内容を、要点を整理して見出し付きで追記して");
  await input.press("Enter");

  // 破壊系なので承認カードが出る（human-in-the-loop）。承認して実行させる。
  const approve = page.getByRole("button", { name: "承認して続行" });
  await expect(approve).toBeVisible({ timeout: 20_000 });
  await approve.click();

  // AI 編集が Yjs 経由で本文へ反映される（append した見出し「サマリー」が現れる）。
  await expect(editor.getByRole("heading", { name: "サマリー" })).toBeVisible({ timeout: 25_000 });
});
