import { test, expect } from "@playwright/test";

import { loginViaKeycloak } from "./helpers";

/// チャット（Phase 3）の E2E。
/// 前提: compose で chat 有効（SHIKI__CHAT__ENABLED=true）＋ LLM=stub（決定的）で shiki-server 起動。
/// stub プロバイダは「回答: <質問>」を語単位でストリーミングするため、実 LLM 無しで検証できる。
test.describe("chat (permission-aware RAG チャット)", () => {
  test("送信→回答がストリーミングで返り、再訪しても確定結果が残る", async ({ page }) => {
    await loginViaKeycloak(page);
    await page.goto("/");

    const q = `経費規程について教えて ${Date.now().toString(36)}`;
    const input = page.getByLabel("メッセージを入力");
    await input.click();
    await input.fill(q);
    await page.getByRole("button", { name: "送信" }).click();

    // 会話画面 /c/:id へ遷移する。
    await page.waitForURL(/\/c\/[0-9a-f-]+/i, { timeout: 20_000 });

    // 送信したユーザー発話が表示される。
    await expect(page.getByText(q, { exact: false })).toBeVisible({ timeout: 10_000 });

    // アシスタント回答がストリーミングで現れる（stub は「回答:」を返す・接続非依存ジョブ）。
    await expect(page.getByText(/回答/).first()).toBeVisible({ timeout: 30_000 });

    // エージェントモードのトグルが UI に存在する（切り替え可能・Task 3.10）。
    await expect(page.getByRole("switch", { name: "エージェントモード" })).toBeVisible();

    // ページ再訪しても確定回答が残る（generation_event の projection・Task 3.11）。
    await page.reload();
    await expect(page.getByText(/回答/).first()).toBeVisible({ timeout: 20_000 });
  });

  test("共有ダイアログを開ける（Task 3.7）", async ({ page }) => {
    await loginViaKeycloak(page);
    await page.goto("/");
    const input = page.getByLabel("メッセージを入力");
    await input.fill("こんにちは");
    await page.getByRole("button", { name: "送信" }).click();
    await page.waitForURL(/\/c\//i, { timeout: 20_000 });

    // 応答が速いとメッセージフッタの共有ボタンも現れ strict 違反になるため、ヘッダ側（先頭）に限定する。
    await page.getByRole("button", { name: "共有" }).first().click();
    await expect(page.getByRole("dialog")).toContainText("会話を共有");
  });
});
