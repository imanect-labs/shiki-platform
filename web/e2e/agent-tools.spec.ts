import { test, expect } from "@playwright/test";

import { loginViaKeycloak } from "./helpers";

/// エージェントモードのツール（web_search / code_interpreter）E2E（Phase 4）。
/// 前提: compose で chat 有効・LLM=stub（決定的）・websearch=stub。
/// stub LLM は本文プレフィックスで対応ツールを 1 回呼ぶ（`websearch:`→web_search /
/// `python:`→code_interpreter）。web_search はホスト側 StubSearchProvider が決定的ヒットを返すため
/// サンドボックス無しの CI でも成立する。code_interpreter は実サンドボックス（重い V8 ビルド）が
/// 要るため `E2E_SANDBOX=1` のときのみ実行する。
test.describe("agent tools (web_search / code_interpreter)", () => {
  /// 初回送信でスレッドを作り、会話画面でエージェントモードを ON にして返すヘルパ。
  async function openThreadInAgentMode(page: import("@playwright/test").Page) {
    await loginViaKeycloak(page);
    await page.goto("/");
    const input = page.getByLabel("メッセージを入力");
    await input.fill("こんにちは");
    await page.getByRole("button", { name: "送信" }).click();
    await page.waitForURL(/\/c\/[0-9a-f-]+/i, { timeout: 20_000 });
    // 初回応答が確定するまで待つ（stub は「回答:」を返す）。
    await expect(page.getByText(/回答/).first()).toBeVisible({ timeout: 30_000 });
    // エージェントモードを ON にする（会話画面のトグル）。
    await page.getByRole("switch", { name: "エージェントモード" }).click();
    return input;
  }

  test("web_search: ツール活動チップが表示され回答が返る", async ({ page }) => {
    const input = await openThreadInAgentMode(page);

    // stub LLM は `websearch:` で web_search を選ぶ（提示ツール順に依存しない）。
    await input.fill("websearch: rust 最新情報");
    await page.getByRole("button", { name: "送信" }).click();

    // Chain of Thought に「web を検索」チップが出る（tool_call の可視化・Task 4.11）。
    await expect(page.getByText("web を検索")).toBeVisible({ timeout: 30_000 });

    // ツール実行後、アシスタントの最終回答が返る（2 ターン目・接続非依存ジョブ）。
    await expect(page.getByText(/回答/).first()).toBeVisible({ timeout: 30_000 });

    // 再訪してもツール履歴（チップ）が残る（generation_event の projection）。
    await page.reload();
    await expect(page.getByText("web を検索")).toBeVisible({ timeout: 20_000 });
  });

  test("code_interpreter: コード実行チップが表示される", async ({ page }) => {
    // 実サンドボックス（V8/Pyodide）が要る。CI（sandbox 無し）ではスキップ。
    test.skip(process.env.E2E_SANDBOX !== "1", "実サンドボックス（E2E_SANDBOX=1）が必要");
    const input = await openThreadInAgentMode(page);

    // stub LLM は `python:` で code_interpreter を選ぶ。
    await input.fill("python: print(6 * 7)");
    await page.getByRole("button", { name: "送信" }).click();

    // 「コードを実行」チップが出る。
    await expect(page.getByText("コードを実行")).toBeVisible({ timeout: 30_000 });
    // 実行結果（42）を含む回答/観測が返る。
    await expect(page.getByText(/42/)).toBeVisible({ timeout: 30_000 });
  });
});
