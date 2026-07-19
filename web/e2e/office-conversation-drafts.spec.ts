import { expect, test } from "@playwright/test";

import { loginViaKeycloak, uniqueName } from "./helpers";

/// issue #332「会話→Word 文書」の受け入れ条件を検証する（前提: LLM=stub）:
/// - チャットで save_document（下書き）→ 下書きカード → 下書き文書画面へ遷移し本文が seed される
/// - 同じ会話で複数の下書き（別名）をタブで切り替えられる
/// - リロードしてもスレッド履歴（document_draft ブロック）から下書きへ復元できる
/// - 「ドライブに保存」で .docx 実体化 → /office/{id} へ遷移する（Collabora の起動確認は
///   OFFICE_E2E=1 のみ。既定 CI は ingestion-worker 非稼働のため保存以降はゲート内で検証）
///
/// stub 駆動: `savedoc:<name>` → save_document（下書き）。

/// ホームから会話を開始して text を送信する。
async function sendFromHome(page: import("@playwright/test").Page, text: string) {
  await page.goto("/");
  const input = page.getByLabel("メッセージを入力");
  await input.fill(text);
  await page.getByRole("button", { name: "送信" }).click();
}

test("下書き: チャット→下書き文書画面へ遷移し本文が seed される", async ({ page }) => {
  await loginViaKeycloak(page); // alice
  const docName = uniqueName("提案書");
  await sendFromHome(page, `savedoc:${docName}`);

  // 下書き文書画面へ遷移する（自動）。
  await page.waitForURL(/\/office\/draft/, { timeout: 25_000 });
  await expect(page.getByTestId("draft-badge")).toBeVisible();
  const editor = page.getByTestId("draft-note-editor");
  await expect(editor).toBeVisible({ timeout: 20_000 });
  // stub が入れた本文（見出し = 下書き名）が seed されている。
  await expect(editor.getByRole("heading", { name: docName })).toBeVisible({ timeout: 15_000 });

  // 会話に下書きカードが残っている（アシスタントパネル内）。
  await expect(page.getByTestId("document-draft-card").first()).toBeVisible({ timeout: 15_000 });
});

test("下書き: 同じ会話で複数の下書きをタブで切り替える", async ({ page }) => {
  await loginViaKeycloak(page);
  const nameA = uniqueName("企画書");
  const nameB = uniqueName("報告書");
  await sendFromHome(page, `savedoc:${nameA}`);
  await page.waitForURL(/\/office\/draft/, { timeout: 25_000 });
  await expect(
    page.getByTestId("draft-note-editor").getByRole("heading", { name: nameA }),
  ).toBeVisible({ timeout: 15_000 });

  // 同じ会話でもう 1 本（別名）を作る → タブが 2 つになり、新しい方がアクティブ。
  const input = page.getByLabel("メッセージを入力");
  await input.fill(`savedoc:${nameB}`);
  await page.getByRole("button", { name: "送信" }).click();
  const tabs = page.getByTestId("draft-tabs");
  await expect(tabs).toBeVisible({ timeout: 20_000 });
  await expect(tabs.getByText(nameA)).toBeVisible();
  await expect(tabs.getByText(nameB)).toBeVisible();
  await expect(
    page.getByTestId("draft-note-editor").getByRole("heading", { name: nameB }),
  ).toBeVisible({ timeout: 15_000 });

  // タブで最初の下書きへ戻れる。
  await tabs.getByText(nameA).click();
  await expect(
    page.getByTestId("draft-note-editor").getByRole("heading", { name: nameA }),
  ).toBeVisible({ timeout: 15_000 });
});

test("復元: localStorage を消してもスレッド履歴から下書きへ復元できる", async ({ page }) => {
  await loginViaKeycloak(page);
  const docName = uniqueName("復元");
  await sendFromHome(page, `savedoc:${docName}`);
  await page.waitForURL(/\/office\/draft/, { timeout: 25_000 });
  await expect(
    page.getByTestId("draft-note-editor").getByRole("heading", { name: docName }),
  ).toBeVisible({ timeout: 15_000 });

  // 別端末相当: ローカル下書きストアを消してリロード → 履歴の document_draft から復元される。
  await page.evaluate(() => window.localStorage.removeItem("shiki.document-drafts.v1"));
  await page.reload();
  await expect(
    page.getByTestId("draft-note-editor").getByRole("heading", { name: docName }),
  ).toBeVisible({ timeout: 20_000 });
});

/// 確定保存（.docx 化）は ingestion-worker が必要（markdown 非空のため）。既定 CI では worker
/// 非稼働なのでフル compose 環境（OFFICE_E2E=1）のみで検証する。Collabora 起動まで確認する。
test("保存: ドライブに保存で .docx 実体化し /office/{id}（Collabora）が開く", async ({ page }) => {
  test.skip(
    process.env.OFFICE_E2E !== "1",
    "OFFICE_E2E=1（ingestion-worker + Collabora 稼働）のみ",
  );
  await loginViaKeycloak(page);
  const docName = uniqueName("確定");
  await sendFromHome(page, `savedoc:${docName}`);
  await page.waitForURL(/\/office\/draft/, { timeout: 25_000 });
  await expect(
    page.getByTestId("draft-note-editor").getByRole("heading", { name: docName }),
  ).toBeVisible({ timeout: 15_000 });

  // 「ドライブに保存」→ ダイアログ（既定=ルート）→ 保存。
  await page.getByTestId("draft-save-button").click();
  await expect(page.getByTestId("save-draft-name")).toHaveValue(docName);
  await page.getByTestId("save-draft-confirm").click();

  // 実体化した .docx の Collabora エディタへ遷移する（/office/draft ではない実 UUID）。
  await page.waitForURL(/\/office\/[0-9a-f-]{36}/i, { timeout: 30_000 });
  // Collabora iframe が起動する（office.spec.ts と同じ判定）。
  await expect(page.getByTestId("office-frame")).toBeVisible({ timeout: 30_000 });
  const frame = page.frameLocator('[data-testid="office-frame"]');
  await expect(frame.locator("#main-document-content, #document-container").first()).toBeVisible({
    timeout: 60_000,
  });
});
