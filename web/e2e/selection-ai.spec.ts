import { expect, test } from "@playwright/test";

import { loginViaKeycloak, uniqueName } from "./helpers";

/// 選択→AI 指示（Task 11.10）: ノートで選択→「AI に依頼」→チップ→送信で
/// ユーザーメッセージに選択コンテキストが付くことを一気通貫で検証する。

test("ノートの選択が AI への依頼チップになり送信される", async ({ page }) => {
  await loginViaKeycloak(page);
  await page.goto("/drive");
  await page.getByRole("button", { name: "新規作成" }).click();
  await page.getByTestId("new-note").click();
  await page.waitForURL(/\/notes\//, { timeout: 20_000 });

  // 本文を入力して全選択 → バブルメニューの「AI に依頼」。
  const editor = page.locator(".tiptap").first();
  await editor.click();
  await page.keyboard.type("第一四半期の売上は好調に推移した。");
  await page.keyboard.press("ControlOrMeta+a");
  await page.getByTestId("note-ask-ai").click();

  // アシスタントパネルが開き、Composer に選択チップが出る。
  await expect(page.getByTestId("selection-chip")).toBeVisible({ timeout: 10_000 });
  await expect(page.getByTestId("selection-chip")).toContainText("ノートの選択範囲");

  // 指示を送信 → ユーザーメッセージに選択チップが表示される（サーバ受理の裏取り）。
  const input = page.getByPlaceholder(/尋ねて|指示|メッセージ/).first();
  await input.fill("この部分を要約して");
  await input.press("Enter");
  await expect(page.getByTestId("message-selection-chip")).toBeVisible({ timeout: 20_000 });
  // 送信後はチップが消費されている。
  await expect(page.getByTestId("selection-chip")).toHaveCount(0);
});

/// CSV はアシスタントパネルを持たず、選択範囲をホームのチャットへ引き継ぐ。ホームで会話を
/// 作って最初の発話を送るときに、選択（node_id 付き）が落ちないことを確かめる。
test("CSV の選択がホームのチャットへ引き継がれ、最初の発話に付く", async ({ page }) => {
  await loginViaKeycloak(page);
  await page.goto("/drive");
  const nodeId = await page.evaluate(async (name) => {
    const csrf = document.cookie.match(/(?:^|;\s*)shiki_csrf=([^;]+)/)?.[1] ?? "";
    const res = await fetch("/api/tabular/save", {
      method: "POST",
      credentials: "include",
      headers: { "Content-Type": "application/json", "X-CSRF-Token": csrf },
      body: JSON.stringify({ parent_id: null, name, csv: "id,name\n1,alice\n2,bob\n" }),
    });
    if (!res.ok) throw new Error(`CSV 作成に失敗: ${res.status}`);
    return (await res.json()).node_id as string;
  }, uniqueName("sel") + ".csv");

  await page.goto(`/csv/${nodeId}`);
  const grid = page.getByTestId("csv-grid");
  await expect(grid).toBeVisible({ timeout: 20_000 });
  await expect(page.getByText(/2 行 × 2 列/)).toBeVisible({ timeout: 15_000 });

  // Glide は canvas 描画なので、1 行目のセル（行番号列とヘッダの内側）を座標でクリックする。
  const box = (await grid.locator("canvas").first().boundingBox())!;
  await page.mouse.click(box.x + 120, box.y + 36 + 17);
  await page.getByTestId("csv-ask-ai").click();

  // ホームの入力欄に選択チップが出る。
  await page.waitForURL((u) => u.pathname === "/", { timeout: 10_000 });
  await expect(page.getByTestId("selection-chip")).toContainText("CSV の選択範囲");

  await page.getByLabel("メッセージを入力").fill("この行を説明して");
  await page.getByRole("button", { name: "送信" }).click();
  await page.waitForURL(/\/c\//, { timeout: 20_000 });
  await expect(page.getByTestId("message-selection-chip")).toBeVisible({ timeout: 20_000 });

  // 楽観表示ではなくサーバに保存された発話にも付いていること（再読み込みで確かめる）。
  await page.reload();
  await expect(page.getByTestId("message-selection-chip")).toBeVisible({ timeout: 20_000 });
  await expect(page.getByTestId("message-selection-chip")).toContainText("CSV の選択範囲");
});
