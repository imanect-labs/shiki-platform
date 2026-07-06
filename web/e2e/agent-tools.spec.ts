import { test, expect, type Page } from "@playwright/test";

import { loginViaKeycloak } from "./helpers";

/// エージェントモードのツール（web_search / code_interpreter）E2E（Phase 4）。
/// 前提: compose で chat 有効・LLM=stub（決定的）・websearch=stub。
/// stub LLM は本文プレフィックスで対応ツールを 1 回呼ぶ（`websearch:`→web_search /
/// `python:`→code_interpreter）。web_search はホスト側 StubSearchProvider が決定的ヒットを返すため
/// サンドボックス無しの CI でも成立する。code_interpreter は実サンドボックス（重い V8 ビルド）が
/// 要るため `E2E_SANDBOX=1` のときのみ実行する。
test.describe("agent tools (web_search / code_interpreter)", () => {
  /// 初回送信でスレッドを作り、会話画面でエージェントモードを ON にして返すヘルパ。
  async function openThreadInAgentMode(page: Page) {
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

  /// Chain of Thought のツール動作ラベルを確認する（ストリーミング中は自動展開・
  /// 完了後は折りたたまれるため、必要なら「思考プロセス」トグルを開いてから確認する）。
  async function expectToolVerb(page: Page, verbPattern: RegExp) {
    const verb = page.getByText(verbPattern).first();
    const cotToggle = page.getByRole("button", { name: /思考プロセス/ }).first();
    await expect(verb.or(cotToggle)).toBeVisible({ timeout: 30_000 });
    if (!(await verb.isVisible())) {
      await cotToggle.click();
    }
    await expect(verb).toBeVisible({ timeout: 10_000 });
  }

  test("web_search: ツール活動が可視化され回答が返る", async ({ page }) => {
    const input = await openThreadInAgentMode(page);

    // stub LLM は `websearch:` で web_search を選ぶ（提示ツール順に依存しない）。
    await input.fill("websearch: rust 最新情報");
    await page.getByRole("button", { name: "送信" }).click();

    // Chain of Thought に「web を検索」動作が出る（tool_call の可視化・Task 4.11）。
    await expectToolVerb(page, /web を検索(していま|しました)/);

    // 再訪してもツール履歴が残る（generation_event の projection）。
    await page.reload();
    await expectToolVerb(page, /web を検索/);
  });

  test("code_interpreter: コード実行が可視化される", async ({ page }) => {
    // 実サンドボックス（V8/Pyodide）が要る。CI（sandbox 無し）ではスキップ。
    test.skip(process.env.E2E_SANDBOX !== "1", "実サンドボックス（E2E_SANDBOX=1）が必要");
    const input = await openThreadInAgentMode(page);

    // stub LLM は `python:` で code_interpreter を選ぶ。
    await input.fill("python: print(6 * 7)");
    await page.getByRole("button", { name: "送信" }).click();

    // 「コードを実行」動作が出る。
    await expectToolVerb(page, /コードを実行(していま|しました)/);
    // 実行結果（42）を含む観測が返る。
    await expect(page.getByText(/42/)).toBeVisible({ timeout: 30_000 });
  });
});
