import { test, expect } from "@playwright/test";

import { loginViaKeycloak } from "./helpers";

/// ホームのモード切替（#964）・ワークスペース選択（Phase 6 UX）の E2E。
/// 会話の図を「アプリとして保存」する経路は mini-apps.spec.ts（```ui app・#604）で確かめる。
/// 前提: LLM=stub・通常チャットはモデル裁量ループ（issue #102）。
test.describe("ホームのモード切替", () => {
  test("モードは入力欄の上で選び、エージェントにすると作業フォルダと承認モードを選べる", async ({
    page,
  }) => {
    await loginViaKeycloak(page);
    await page.goto("/");

    // ホームの入力欄にはエージェントモードのトグルを置かない（モードは入力欄の上の切替）。
    await expect(page.getByRole("tablist", { name: "モード" })).toBeVisible();
    await expect(page.getByRole("switch", { name: "エージェントモード" })).toHaveCount(0);
    await expect(page.getByTestId("home-mode-chat")).toHaveAttribute("aria-selected", "true");

    // チャットでは作業フォルダは出ない。エージェントにすると出る。
    const folder = page.getByRole("button", { name: /作業フォルダ/ });
    await expect(folder).toBeHidden();
    await page.getByTestId("home-mode-agent").click();
    await expect(folder).toBeVisible();
    await expect(folder).toContainText("マイドライブ");
    await expect(page.getByTestId("home-approval-require_approval")).toHaveAttribute(
      "aria-checked",
      "true",
    );

    // ←→ でモードを移れる（tablist の作法）。
    await page.getByTestId("home-mode-agent").press("ArrowRight");
    await expect(page.getByTestId("home-mode-create")).toBeFocused();
    await expect(page.getByTestId("home-mode-create")).toHaveAttribute("aria-selected", "true");
    await expect(page.getByRole("radiogroup", { name: "作るもの" })).toBeVisible();
    await page.getByTestId("home-mode-create").press("ArrowLeft");
    await expect(page.getByTestId("home-mode-agent")).toHaveAttribute("aria-selected", "true");

    // 作業フォルダのチップからフォルダ選択ダイアログが開く（「配下に作成」「このフォルダを使う」の 2 アクション）。
    await folder.click();
    await expect(page.getByRole("dialog")).toContainText("ワークスペースの場所");
    await page.keyboard.press("Escape");

    // 承認モードを既定から緩めると、何が起きるかが文字で出る（ホバーの無い端末でも分かる）。
    await page.getByTestId("home-approval-auto").click();
    await expect(page.getByTestId("home-approval-auto")).toHaveAttribute("aria-checked", "true");
    await expect(page.locator("#home-approval-description")).toBeVisible();

    // エージェントのモードで送ると、選んだ承認モードを最初の発話より先にスレッドへ設定し、
    // エージェントモードの会話として始まる。
    const modePut = page.waitForRequest(
      (r) => r.method() === "PUT" && /\/api\/threads\/[^/]+\/autonomous-mode$/.test(r.url()),
    );
    const posted = page.waitForRequest(
      (r) => r.method() === "POST" && /\/api\/threads\/[^/]+\/messages$/.test(r.url()),
    );
    const input = page.getByLabel("メッセージを入力");
    await input.fill(`エージェントで始める ${Date.now().toString(36)}`);
    await page.getByRole("button", { name: "送信" }).click();
    expect(JSON.parse((await modePut).postData() ?? "{}")).toEqual({ mode: "auto" });
    const body = JSON.parse((await posted).postData() ?? "{}") as { autonomous?: boolean };
    expect(body.autonomous).toBe(true);
    await page.waitForURL(/\/c\/[0-9a-f-]+/i, { timeout: 20_000 });
    await expect(page.getByRole("switch", { name: "エージェントモード" })).toHaveAttribute(
      "aria-checked",
      "true",
      { timeout: 20_000 },
    );
  });

  test("作成のモードで送ると、作るものの種類が発話の先頭に入る", async ({ page }) => {
    await loginViaKeycloak(page);
    await page.goto("/");

    await page.getByTestId("home-mode-create").click();
    await page.getByTestId("home-create-sheet").click();
    await expect(page.getByTestId("home-create-sheet")).toHaveAttribute("aria-checked", "true");
    const input = page.getByLabel("メッセージを入力");
    await expect(input).toHaveAttribute("placeholder", /Excel/);

    const what = `月次の経費集計表 ${Date.now().toString(36)}`;
    const posted = page.waitForRequest(
      (r) => r.method() === "POST" && /\/api\/threads\/[^/]+\/messages$/.test(r.url()),
    );
    await input.fill(what);
    await page.getByRole("button", { name: "送信" }).click();
    const body = JSON.parse((await posted).postData() ?? "{}") as { text?: string };
    expect(body.text).toBe(`Excel で作成：${what}`);
    await page.waitForURL(/\/c\/[0-9a-f-]+/i, { timeout: 20_000 });
  });

  test("社内検索のモードで送ると、最初の発話で社内文書検索を必ず使う", async ({ page }) => {
    await loginViaKeycloak(page);
    await page.goto("/");

    await page.getByTestId("home-mode-search").click();
    // 既定のチャットに比べて、承認モードの設定は出さない（承認の PUT も飛ばない）。
    await expect(page.getByRole("radiogroup", { name: "承認", exact: true })).toBeHidden();
    let modePut = false;
    page.on("request", (r) => {
      if (r.method() === "PUT" && /\/autonomous-mode$/.test(r.url())) modePut = true;
    });

    const posted = page.waitForRequest(
      (r) => r.method() === "POST" && /\/api\/threads\/[^/]+\/messages$/.test(r.url()),
    );
    await page.getByLabel("メッセージを入力").fill(`経費精算の締め日 ${Date.now().toString(36)}`);
    await page.getByRole("button", { name: "送信" }).click();
    const body = JSON.parse((await posted).postData() ?? "{}") as {
      tools?: string[];
      autonomous?: boolean;
    };
    expect(body.tools).toContain("doc_search");
    expect(body.autonomous ?? false).toBe(false);
    expect(modePut).toBe(false);
  });
});
