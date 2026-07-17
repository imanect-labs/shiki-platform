import { expect, test, type Page } from "@playwright/test";

import { loginViaKeycloak, uniqueName } from "./helpers";

/// Task 11.11「表を作成して」→下書き→確定の受け入れ条件を検証する（前提: LLM=stub）:
/// - チャットで save_csv（下書き）→ 下書き CSV 画面へ遷移しグリッドが seed される
/// - 「ドライブに保存」で CSV 実体化 → /csv/{id} へ遷移しエディタのグリッドが見える
/// - 下書きがリロードでスレッド履歴から復元される
///
/// stub 駆動: `savecsv:<name>` → save_csv（固定 3 列×3 行）。

/// ホームから会話を開始して text を送信する。
async function sendFromHome(page: Page, text: string) {
  await page.goto("/");
  const input = page.getByLabel("メッセージを入力");
  await input.fill(text);
  await page.getByRole("button", { name: "送信" }).click();
}

test("下書き: チャット→下書き CSV 画面→ドライブに保存で /csv/{id} が開く", async ({ page }) => {
  await loginViaKeycloak(page); // alice
  const csvName = uniqueName("集計");
  page.on("console", (m) => { if (m.type() === "error") console.log("[console.error]", m.text().slice(0, 300)); });
  page.on("pageerror", (e) => console.log("[pageerror]", String(e).slice(0, 300)));
  await sendFromHome(page, `savecsv:${csvName}`);

  // 下書き CSV 画面へ遷移する（自動）。下書きバッジとローカルグリッドが出る。
  try {
    await page.waitForURL(/\/csv\/draft/, { timeout: 25_000 });
  } catch (e) {
    console.log("STUCK at", page.url());
    console.log("main text:", (await page.locator("main").innerText().catch(() => "")).slice(0, 600));
    throw e;
  }
  await expect(page.getByTestId("draft-badge")).toBeVisible();
  await expect(page.getByTestId("csv-draft-grid")).toBeVisible({ timeout: 20_000 });

  // 会話にも下書きカードが残る（履歴からの再入口）。
  await expect(page.getByTestId("csv-draft-card").first()).toBeVisible();

  // 「ドライブに保存」→ ダイアログ（既定=ルート・名前は下書き名）→ 保存。
  await page.getByTestId("draft-save-button").click();
  await expect(page.getByTestId("save-draft-name")).toHaveValue(csvName);
  await page.getByTestId("save-draft-confirm").click();

  // 実体化した CSV エディタへ遷移し、グリッドが見える。
  await page.waitForURL(/\/csv\/[0-9a-f-]{36}/i, { timeout: 25_000 });
  await expect(page.getByTestId("csv-grid")).toBeVisible({ timeout: 20_000 });
});

test("下書き: リロードしてもスレッド履歴から復元される", async ({ page }) => {
  await loginViaKeycloak(page);
  const csvName = uniqueName("復元");
  await sendFromHome(page, `savecsv:${csvName}`);
  await page.waitForURL(/\/csv\/draft/, { timeout: 25_000 });
  await expect(page.getByTestId("csv-draft-grid")).toBeVisible({ timeout: 20_000 });

  // localStorage を消してリロード → 会話履歴の csv_draft ブロックから復元される。
  await page.evaluate(() => window.localStorage.removeItem("shiki.csv-drafts.v1"));
  await page.reload();
  await expect(page.getByTestId("csv-draft-grid")).toBeVisible({ timeout: 25_000 });
  await expect(page.getByTestId("draft-badge")).toBeVisible();
});
