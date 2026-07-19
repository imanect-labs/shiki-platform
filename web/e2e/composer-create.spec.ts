import { expect, test, type Page } from "@playwright/test";

import { loginViaKeycloak } from "./helpers";

/// issue #333「チャット入力の + → 作成」の受け入れ条件を検証する:
/// - 「+」→「作成」→ ノート / ドキュメント(Word) / スライド / スプレッドシート(CSV) が選べる
/// - 選択すると当該が作成され、対応エディタへ遷移する
/// - 添付（ローカル/ドライブ）は従来どおり同じ「+」内に共存する（区切り表示）
///
/// ドキュメントは POST /documents（markdown 無し）なので ingestion-worker 非稼働の既定 CI
/// でも作成できる。Collabora 起動の確認は office.spec 側（OFFICE_E2E ゲート）に委ねる。

async function openCreateMenu(page: Page) {
  await page.goto("/");
  await page.getByRole("button", { name: "追加メニューを開く" }).click();
  // 添付項目が従来どおり共存していること。
  await expect(page.getByRole("menuitem", { name: "ローカルからアップロード" })).toBeVisible();
  await expect(page.getByRole("menuitem", { name: "ドライブから選択" })).toBeVisible();
  await page.getByTestId("composer-create-menu").click();
}

test("+→作成→ノート: 作成されエディタが開く", async ({ page }) => {
  await loginViaKeycloak(page);
  await openCreateMenu(page);
  await page.getByTestId("composer-create-note").click();
  await page.waitForURL(/\/notes\/[0-9a-f-]{36}/i, { timeout: 25_000 });
  await expect(page.getByTestId("note-sync-status")).toHaveText("同期済み", { timeout: 20_000 });
});

test("+→作成→ドキュメント: .docx が作成され /office へ遷移する", async ({ page }) => {
  await loginViaKeycloak(page);
  await openCreateMenu(page);
  await page.getByTestId("composer-create-document").click();
  await page.waitForURL(/\/office\/[0-9a-f-]{36}/i, { timeout: 25_000 });
  const nodeId = page.url().match(/\/office\/([0-9a-f-]{36})/i)?.[1] ?? "";
  // ノードが .docx として作成されている（Collabora 起動は OFFICE_E2E ゲートの office.spec 側）。
  const node = await page.evaluate(async (id) => {
    const res = await fetch(`/api/files/${id}`, { credentials: "include" });
    if (!res.ok) throw new Error(`get_file: ${res.status}`);
    return (await res.json()) as { name: string; content_type: string | null };
  }, nodeId);
  expect(node.name).toMatch(/^無題のドキュメント( \(\d+\))?\.docx$/);
  expect(node.content_type ?? "").toContain("wordprocessingml.document");
});

test("+→作成→スライド: 作成されエディタが開く", async ({ page }) => {
  await loginViaKeycloak(page);
  await openCreateMenu(page);
  await page.getByTestId("composer-create-slide").click();
  await page.waitForURL(/\/slides\/[0-9a-f-]{36}/i, { timeout: 25_000 });
});

test("+→作成→スプレッドシート: 作成されエディタが開く", async ({ page }) => {
  await loginViaKeycloak(page);
  await openCreateMenu(page);
  await page.getByTestId("composer-create-csv").click();
  await page.waitForURL(/\/csv\/[0-9a-f-]{36}/i, { timeout: 25_000 });
});

test("ドライブの「新規作成 > ドキュメント」もサーバ経路で作成される（回帰）", async ({ page }) => {
  await loginViaKeycloak(page);
  await page.goto("/drive");
  await page.getByRole("button", { name: "新規作成" }).click();
  await page.getByTestId("new-document").click();
  await page.waitForURL(/\/office\/[0-9a-f-]{36}/i, { timeout: 25_000 });
});
