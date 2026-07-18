import { expect, test, type Page } from "@playwright/test";

import { loginViaKeycloak, uniqueName } from "./helpers";

/// Task 11.3「パワポを作成して」→下書き→確定の受け入れ条件を検証する（前提: LLM=stub）:
/// - チャットで save_slide（下書き）→ 下書きスライド画面へ遷移しスライドが seed される
/// - 「ドライブに保存」でスライド実体化 → /slides/{id} へ遷移しフィルムストリップが見える
/// - 同じ会話で複数の下書き（別名）をタブで切り替えられる
///
/// stub 駆動: `saveslide:<name>` → save_slide（固定 3 枚・theme_id=plain）。

/// ホームから会話を開始して text を送信する。
async function sendFromHome(page: Page, text: string) {
  await page.goto("/");
  const input = page.getByLabel("メッセージを入力");
  await input.fill(text);
  await page.getByRole("button", { name: "送信" }).click();
}

test("下書き: チャット→下書きスライド画面→ドライブに保存で /slides/{id} が開く", async ({
  page,
}) => {
  await loginViaKeycloak(page); // alice
  const slideName = uniqueName("デモ");
  await sendFromHome(page, `saveslide:${slideName}`);

  // 下書きスライド画面へ遷移する（自動）。下書きバッジとワークスペースが出る。
  await page.waitForURL(/\/slides\/draft/, { timeout: 25_000 });
  await expect(page.getByTestId("draft-badge")).toBeVisible();
  const filmstrip = page.getByTestId("slide-filmstrip");
  await expect(filmstrip).toBeVisible({ timeout: 20_000 });
  // stub が入れた 3 枚がフィルムストリップに並ぶ。
  await expect(filmstrip.getByRole("button", { name: /スライド 3/ })).toBeVisible({
    timeout: 15_000,
  });

  // 会話にも下書きカードが残る（履歴からの再入口）。
  await expect(page.getByTestId("slide-draft-card").first()).toBeVisible();

  // 「ドライブに保存」→ ダイアログ（既定=ルート・名前は下書き名）→ 保存。
  await page.getByTestId("draft-save-button").click();
  await expect(page.getByTestId("save-draft-name")).toHaveValue(slideName);
  await page.getByTestId("save-draft-confirm").click();

  // 実体化したスライドへ遷移し、フィルムストリップ（3 枚）が見える。
  await page.waitForURL(/\/slides\/[0-9a-f-]{36}/i, { timeout: 25_000 });
  await expect(page.getByTestId("slide-filmstrip")).toBeVisible({ timeout: 20_000 });
  await expect(
    page.getByTestId("slide-filmstrip").getByRole("button", { name: /スライド 3/ }),
  ).toBeVisible({ timeout: 15_000 });
});

test("下書き: 同じ会話で複数の下書きスライドをタブで切り替える", async ({ page }) => {
  await loginViaKeycloak(page);
  const nameA = uniqueName("提案A");
  const nameB = uniqueName("提案B");
  await sendFromHome(page, `saveslide:${nameA}`);
  await page.waitForURL(/\/slides\/draft/, { timeout: 25_000 });
  await expect(page.getByTestId("slide-filmstrip")).toBeVisible({ timeout: 20_000 });

  // 同じ会話でもう 1 本（別名）を作る → タブが 2 つになり、新しい方がアクティブ。
  const input = page.getByLabel("メッセージを入力");
  await input.fill(`saveslide:${nameB}`);
  await page.getByRole("button", { name: "送信" }).click();
  const tabs = page.getByTestId("draft-tabs");
  await expect(tabs).toBeVisible({ timeout: 20_000 });
  await expect(tabs.getByText(nameA)).toBeVisible();
  await expect(tabs.getByText(nameB)).toBeVisible();

  // タブで最初の下書きへ戻れる（ワークスペースは表示されたまま）。
  await tabs.getByText(nameA).click();
  await expect(page.getByTestId("slide-filmstrip")).toBeVisible({ timeout: 15_000 });
});
