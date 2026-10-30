import { expect, test } from "@playwright/test";

import { loginViaKeycloak } from "./helpers";

/// 入力欄の Markdown の WYSIWYG（既定の書き方・設定 → チャット → 入力の形式）。
/// 前提: chat.spec と同じ（LLM=stub は「回答: <質問>」を返す）。
/// 打った記法はその場で見た目に変わり、送ると本文は Markdown として届き、吹き出しも同じ見た目で出る。

test.describe("入力欄の Markdown", () => {
  test("記法がその場で見た目に変わり、Shift+Enter で改行、Enter で Markdown として送る", async ({
    page,
  }) => {
    test.setTimeout(90_000);
    await loginViaKeycloak(page);
    await page.goto("/");
    const input = page.getByLabel("メッセージを入力");
    await input.click();

    // インライン: `**…**` は太字、`` `…` `` はコードになる（記号は残らない）。
    await input.pressSequentially("**要点**を `doc_search` で調べて");
    await expect(input.locator("strong")).toHaveText("要点");
    await expect(input.locator("code")).toHaveText("doc_search");
    await expect(input).not.toContainText("**");

    // Shift+Enter は送らずに改行。行頭の `- ` は箇条書きになり、Shift+Enter で次の項目へ進む。
    await input.press("Shift+Enter");
    await input.pressSequentially("- 規程");
    await input.press("Shift+Enter");
    await input.pressSequentially("議事録");
    await expect(input.locator("li")).toHaveText(["規程", "議事録"]);
    await expect(page).toHaveURL(/\/$/);

    // Enter で送る。送る本文は Markdown（記法に化けない `_` はエスケープしない）。
    const sent = page.waitForRequest(
      // スレッドの作成（題名）ではなく、発話の送信を待つ。
      (r) => r.method() === "POST" && (r.postData() ?? "").includes("- 規程\\n"),
    );
    await input.press("Enter");
    const body = (await sent).postData() ?? "";
    expect(body).toContain("**要点**を `doc_search` で調べて");
    expect(body).toContain("- 規程\\n- 議事録");
    await page.waitForURL(/\/c\/[0-9a-f-]+/i, { timeout: 20_000 });
    // 吹き出しも太字・箇条書きで出る。
    const turn = page.locator("[data-user-turn]").first();
    await expect(turn.locator("strong")).toHaveText("要点");
    await expect(turn.locator("li")).toHaveText(["規程", "議事録"]);
  });

  test("コードブロックの中の Enter は改行で、Ctrl+Enter で送る", async ({ page }) => {
    test.setTimeout(90_000);
    await loginViaKeycloak(page);
    await page.goto("/");
    const input = page.getByLabel("メッセージを入力");
    await input.click();

    // `` ``` `` だけの行の Enter はコードブロックにする（送らない）。
    await input.pressSequentially("```");
    await input.press("Enter");
    await input.pressSequentially("SELECT 1;");
    await input.press("Enter");
    await input.pressSequentially("SELECT 2;");
    await expect(input.locator("pre")).toHaveText("SELECT 1;\nSELECT 2;");
    await expect(page).toHaveURL(/\/$/);

    await input.press("Control+Enter");
    await page.waitForURL(/\/c\/[0-9a-f-]+/i, { timeout: 20_000 });
    await expect(page.locator("[data-user-turn] pre").first()).toContainText("SELECT 2;");
  });
});
