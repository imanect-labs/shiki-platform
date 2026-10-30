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
    await input.pressSequentially("**要点**を `doc_search` で調べて README.md も見て");
    await expect(input.locator("strong")).toHaveText("要点");
    await expect(input.locator("code")).toHaveText("doc_search");
    await expect(input).not.toContainText("**");
    // ファイル名のような語は自動でリンクにしない。
    await expect(input.locator("a")).toHaveCount(0);

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
    expect(body).toContain("**要点**を `doc_search` で調べて README.md も見て");
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

  test("送った発話のリンクは行き先のホストを添え、図にはしない", async ({ page }) => {
    test.setTimeout(90_000);
    await loginViaKeycloak(page);
    await page.goto("/");
    const input = page.getByLabel("メッセージを入力");
    await input.click();

    // Markdown の素テキストを貼ると構造になる（リンクも入る）。
    await page.evaluate(() => {
      const el = document.querySelector('[aria-label="メッセージを入力"]');
      const data = new DataTransfer();
      data.setData(
        "text/plain",
        "- [就業規程](https://example.com/rule)\n- 確認する\n\n```ui\nbar\n```\n",
      );
      el?.dispatchEvent(new ClipboardEvent("paste", { clipboardData: data, bubbles: true, cancelable: true }));
    });
    await expect(input.locator("a")).toHaveText("就業規程");
    await input.press("Control+Enter");
    await page.waitForURL(/\/c\/[0-9a-f-]+/i, { timeout: 20_000 });

    // 表示の文字と行き先が違うリンクは、行き先のホストを添える。```ui は図にせずコードのまま。
    const turn = page.locator("[data-user-turn]").first();
    await expect(turn.locator("a")).toContainText("就業規程");
    await expect(turn.locator("a")).toContainText("(example.com)");
    await expect(turn.locator("pre")).toContainText("bar");
  });

  test("間の空行は残り、吹き出しにも空行として出る", async ({ page }) => {
    test.setTimeout(90_000);
    await loginViaKeycloak(page);
    await page.goto("/");
    const input = page.getByLabel("メッセージを入力");
    await input.click();
    await input.pressSequentially("前の段落");
    await input.press("Shift+Enter");
    await input.press("Shift+Enter");
    await input.pressSequentially("後の段落");
    await expect(input.locator("p")).toHaveText(["前の段落", "", "後の段落"]);

    // Markdown は空の段落を持てないので、空行はノーブレークスペースだけの段落として送る。
    const sent = page.waitForRequest(
      // スレッドの作成（題名）ではなく、発話の送信（`text` を持つ）を待つ。
      (r) => r.method() === "POST" && (r.postData() ?? "").includes('"text":'),
    );
    await input.press("Enter");
    const body = JSON.parse((await sent).postData() ?? "{}") as { text?: string };
    expect(body.text).toBe("前の段落\n\n\u00a0\n\n後の段落");
    await page.waitForURL(/\/c\/[0-9a-f-]+/i, { timeout: 20_000 });
    const turn = page.locator("[data-user-turn]").first();
    await expect(turn.locator("p")).toHaveCount(3);
  });
});

