import { expect, test } from "@playwright/test";

import { loginViaKeycloak, uniqueName } from "./helpers";

/// issue #381「Office 文書の新規作成を Collabora へ一本化」の受け入れ条件（前提: LLM=stub）:
/// - チャットで「Word で作って」→ **承認カード** → 承認 → `.docx` が実体化され Collabora Writer が開く
/// - Excel も同様に `.xlsx` → Collabora Calc が開く
/// - 会話には成果物への導線カード（document_ref）が残る
///
/// 実体は「空テンプレを作成 → Collabora へ HTML/セルを paste」なので **Collabora が要る**。
/// 既定 CI は office プロファイル無効のため OFFICE_E2E=1（フル compose）でのみ動かす。
/// 「承認なしにドライブへファイルを作らない」不変条件自体は Rust 側のユニットテスト
/// （`office_create_tool::tests::creation_tools_require_confirmation`）で常時固定している。
///
/// stub 駆動: `savedoc:<name>` → save_document / `savesheet:<name>` → save_sheet。
test.skip(process.env.OFFICE_E2E !== "1", "OFFICE_E2E=1（Collabora 稼働）のみ");

/// ホームから会話を開始して text を送信する。
async function sendFromHome(page: import("@playwright/test").Page, text: string) {
  await page.goto("/");
  const input = page.getByLabel("メッセージを入力");
  await input.fill(text);
  await page.getByRole("button", { name: "送信" }).click();
}

/// 承認カードを待って承認する（作成系は必ずゲートを通る）。
async function approve(page: import("@playwright/test").Page) {
  const button = page.getByRole("button", { name: "承認して続行" }).first();
  await expect(button).toBeVisible({ timeout: 30_000 });
  await button.click();
}

/// Collabora の編集面が実際に立ち上がったことを確認する（office.spec.ts と同じ判定）。
async function expectCollaboraOpen(page: import("@playwright/test").Page) {
  await page.waitForURL(/\/office\/[0-9a-f-]{36}/i, { timeout: 60_000 });
  await expect(page.getByTestId("office-frame")).toBeVisible({ timeout: 30_000 });
  const frame = page.frameLocator('[data-testid="office-frame"]');
  await expect(frame.locator("#main-document-content, #document-container").first()).toBeVisible({
    timeout: 60_000,
  });
}

test("Word: 承認 → .docx が実体化され Collabora Writer が開く", async ({ page }) => {
  await loginViaKeycloak(page); // alice
  const docName = uniqueName("提案書");
  await sendFromHome(page, `savedoc:${docName}`);

  // 承認するまでファイルは作られない（md 下書き画面へは遷移しない）。
  await approve(page);
  await expectCollaboraOpen(page);

  // 会話に戻ると成果物カードが残っている（#358 の実害の解消）。
  await page.goBack();
  const card = page.getByTestId("document-ref-card").first();
  await expect(card).toBeVisible({ timeout: 20_000 });
  await expect(card).toContainText(`${docName}.docx`);
  await expect(card).toContainText("作成しました");
});

test("Excel: 承認 → .xlsx が実体化され Collabora Calc が開く", async ({ page }) => {
  await loginViaKeycloak(page);
  const bookName = uniqueName("売上");
  await sendFromHome(page, `savesheet:${bookName}`);

  await approve(page);
  await expectCollaboraOpen(page);

  await page.goBack();
  const card = page.getByTestId("document-ref-card").first();
  await expect(card).toBeVisible({ timeout: 20_000 });
  await expect(card).toContainText(`${bookName}.xlsx`);
});

test("ドライブに .docx / .xlsx として並び、版履歴が効く", async ({ page }) => {
  await loginViaKeycloak(page);
  const bookName = uniqueName("台帳");
  await sendFromHome(page, `savesheet:${bookName}`);
  await approve(page);
  await expectCollaboraOpen(page);

  await page.goto("/drive");
  const fileName = `${bookName}.xlsx`;
  await expect(page.getByText(fileName).first()).toBeVisible({ timeout: 20_000 });
  await page.getByRole("button", { name: `「${fileName}」の操作` }).click();
  await page.getByRole("menuitem", { name: "版履歴" }).click();
  await expect(page.getByRole("dialog").getByTestId("version-row").first()).toBeVisible({
    timeout: 15_000,
  });
});
