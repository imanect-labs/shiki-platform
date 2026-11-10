import { test, expect, type Page } from "@playwright/test";

import { loginViaKeycloak } from "./helpers";

/// 会話の分岐（#1166）: 過去の発話を編集して送り直すと枝ができ、発話の下の「< 1/2 >」で
/// 切り替えられる。回答の「…」から「新しいチャットに分岐」でその回答までを新しい会話にできる。
///
/// 前提: LLM=stub（発話をそのまま含めた回答を返す）。
const SHOTS = process.env.E2E_SHOTS_DIR;

async function shot(page: Page, name: string) {
  if (SHOTS) await page.screenshot({ path: `${SHOTS}/${name}.png`, fullPage: false });
}

async function sendAndWait(page: Page, text: string) {
  const input = page.getByLabel("メッセージを入力");
  await input.click();
  await input.fill(text);
  await page.getByRole("button", { name: "送信" }).click();
  await page.waitForURL(/\/c\/[0-9a-f-]+/i, { timeout: 20_000 });
  await expect(page.getByRole("button", { name: "生成を停止" })).toHaveCount(0, { timeout: 60_000 });
  // 確定（読み直し）まで待つ: 回答の「…」が出る＝保存済みの回答になった。
  await expect(page.getByTestId("message-more").last()).toBeAttached({ timeout: 20_000 });
}

test.describe("会話の分岐", () => {
  test("発話を編集して枝を切り替え、回答から新しいチャットに分岐できる", async ({ page }) => {
    await loginViaKeycloak(page);
    await page.goto("/");
    const tag = Date.now().toString(36);
    const q1 = `最初の質問 ${tag}`;
    const q2 = `二つ目の質問 ${tag}`;
    const q2b = `二つ目を直した質問 ${tag}`;

    await sendAndWait(page, q1);
    await sendAndWait(page, q2);
    const thread = page.url();

    // 二つ目の発話を編集する（ホバーで出る編集ボタン）。
    const bubble = page.getByText(q2, { exact: true }).first();
    await bubble.hover();
    await shot(page, "01-hover-user-actions");
    await page.getByTestId("edit-message").nth(1).click();
    const editor = page.getByTestId("message-editor");
    await expect(editor).toBeVisible();
    const area = editor.getByRole("textbox", { name: "発話を編集" });
    await expect(area).toHaveValue(q2);
    // 変えていないうちは送れない。
    await expect(page.getByTestId("message-editor-submit")).toBeDisabled();
    await area.fill(q2b);
    await shot(page, "02-editing");
    await page.getByTestId("message-editor-submit").click();

    // 新しい枝が表示され、元の発話は見えない。切り替えは「2 / 2」。
    await expect(page.getByRole("button", { name: "生成を停止" })).toHaveCount(0, { timeout: 60_000 });
    await expect(page.getByTestId("branch-position")).toHaveText("2 / 2", { timeout: 20_000 });
    await expect(page.getByText(q2b, { exact: true }).first()).toBeVisible();
    await expect(page.getByText(q2, { exact: true })).toHaveCount(0);
    expect(page.url()).toBe(thread);
    await shot(page, "03-new-branch");

    // 前の版へ切り替えると元の発話（と回答）へ戻る。
    await page.getByRole("button", { name: "前の版" }).click();
    await expect(page.getByTestId("branch-position")).toHaveText("1 / 2", { timeout: 20_000 });
    await expect(page.getByText(q2, { exact: true }).first()).toBeVisible();
    await expect(page.getByText(q2b, { exact: true })).toHaveCount(0);
    await expect(page.getByRole("button", { name: "前の版" })).toBeDisabled();
    await shot(page, "04-previous-branch");

    // リロードしても表示中の枝は保たれる（サーバの状態）。
    await page.reload();
    await expect(page.getByTestId("branch-position")).toHaveText("1 / 2", { timeout: 20_000 });

    // 最初の回答から新しいチャットに分岐する（その回答までが複製される）。
    await page.getByTestId("message-more").first().click();
    await shot(page, "05-more-menu");
    await page.getByTestId("fork-thread").click();
    await page.waitForURL((u) => u.href !== thread && /\/c\/[0-9a-f-]+/i.test(u.pathname), {
      timeout: 20_000,
    });
    // 新しい会話の読み込みを待つ（元の会話の二つ目の発話が消え、最初の往復だけが出る）。
    await expect(page.getByText(q2, { exact: true })).toHaveCount(0, { timeout: 20_000 });
    await expect(page.getByText(`回答: ${q1}`).first()).toBeVisible({ timeout: 20_000 });
    await expect(page.getByText(q1, { exact: true }).first()).toBeVisible();
    await expect(page.getByText(q2b, { exact: true })).toHaveCount(0);
    await expect(page.getByTestId("branch-switcher")).toHaveCount(0);
    await shot(page, "06-forked");
  });
});
