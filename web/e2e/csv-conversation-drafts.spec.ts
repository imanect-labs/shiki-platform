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
  await sendFromHome(page, `savecsv:${csvName}`);

  // 下書き CSV 画面へ遷移する（自動）。下書きバッジとローカルグリッドが出る。
  await page.waitForURL(/\/csv\/draft/, { timeout: 25_000 });
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
  // キーはユーザー単位（`shiki.csv-drafts.v1@<tenant>|<user>`・#817）なので接頭辞で消す。
  await page.evaluate(() => {
    const keys = Object.keys(window.localStorage).filter((k) => k.startsWith("shiki.csv-drafts.v1"));
    for (const k of keys) window.localStorage.removeItem(k);
  });
  await page.reload();
  await expect(page.getByTestId("csv-draft-grid")).toBeVisible({ timeout: 25_000 });
  await expect(page.getByTestId("draft-badge")).toBeVisible();
});

type StoredDraft = { name: string; content: string; source: string; rev: number };

/// ユーザー単位のキー（`shiki.csv-drafts.v1@<tenant>|<user>`）に保存された下書き本文を読む。
async function storedDraftContent(page: Page, name: string): Promise<string | undefined> {
  return page.evaluate((n) => {
    const key = Object.keys(window.localStorage).find((k) => k.startsWith("shiki.csv-drafts.v1@"));
    const store = JSON.parse((key && window.localStorage.getItem(key)) ?? "{}");
    return (Object.values(store) as StoredDraft[]).find((e) => e.name === n)?.content;
  }, name);
}

test("下書き: /me より先に履歴を復元しても保存済みの手編集を上書きしない", async ({ page }) => {
  await loginViaKeycloak(page);
  const csvName = uniqueName("手編集");
  await sendFromHome(page, `savecsv:${csvName}`);
  await page.waitForURL(/\/csv\/draft/, { timeout: 25_000 });
  await expect(page.getByTestId("csv-draft-grid")).toBeVisible({ timeout: 20_000 });

  // 保存済みの下書きを手編集した状態にする（source=user）。
  const edited = "手編集,済み\n1,2";
  await page.evaluate(
    ({ name, csv }) => {
      const key = Object.keys(window.localStorage).find((k) =>
        k.startsWith("shiki.csv-drafts.v1@"),
      );
      if (!key) throw new Error("scoped draft key not found");
      const store = JSON.parse(window.localStorage.getItem(key) ?? "{}");
      for (const entry of Object.values(store) as StoredDraft[]) {
        if (entry.name !== name) continue;
        entry.content = csv;
        entry.source = "user";
        entry.rev += 1;
      }
      window.localStorage.setItem(key, JSON.stringify(store));
    },
    { name: csvName, csv: edited },
  );

  // /me を遅らせ、ユーザー未確定のうちに会話履歴からの復元を走らせる。
  await page.route("**/api/me", async (route) => {
    await new Promise((r) => setTimeout(r, 3_000));
    await route.continue();
  });
  const meResponded = page.waitForResponse("**/api/me");
  await page.reload();
  await meResponded;
  const grid = page.getByTestId("csv-draft-grid");
  await expect(grid).toBeVisible({ timeout: 25_000 });
  await expect.poll(() => storedDraftContent(page, csvName)).toBe(edited);
  // 画面（エディタ）も保存済みの手編集で開く。AI 出力のまま開くと、次の編集や「ドライブに保存」で
  // 手編集が上書きされる。グリッドは canvas 描画なので、保存要求に載る CSV で確かめる。
  let savedCsv: string | undefined;
  await page.route("**/api/tabular/save", async (route) => {
    savedCsv = (route.request().postDataJSON() as { csv: string }).csv;
    await route.abort();
  });
  await page.getByTestId("draft-save-button").click();
  await page.getByTestId("save-draft-confirm").click();
  await expect.poll(() => savedCsv).toBeDefined();
  expect(savedCsv?.trim()).toBe(edited);
});
