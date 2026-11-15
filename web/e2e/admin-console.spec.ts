import { test, expect } from "@playwright/test";

import { loginAs, loginViaKeycloak } from "./helpers";

/// 管理コンソール（AI）とチャットのモデル選択（#553）の E2E。
/// 前提: dev seed で alice が組織管理者（organization#admin）、bob は一般メンバー。LLM=stub。
test.describe("管理コンソール", () => {
  test("管理者はアカウントメニューから入り、AI の画面を開ける", async ({
    page,
  }) => {
    await loginViaKeycloak(page);
    await page
      .getByRole("button", { name: "アカウントメニューを開く" })
      .click();
    await page.getByRole("menuitem", { name: "管理コンソール" }).click();
    await page.waitForURL(/\/admin$/);
    await expect(page.getByRole("heading", { name: "ホーム" })).toBeVisible();

    // レールから AI へ（利用量が入口）。
    await page.getByRole("link", { name: "AI" }).click();
    await page.waitForURL(/\/admin\/ai\/usage$/);
    await expect(page.getByRole("heading", { name: "利用量" })).toBeVisible();
    await expect(page.getByRole("region", { name: "指標" })).toContainText(
      "呼び出し",
    );

    // モデル: カタログ表と Auto 行が出る。
    await page.getByRole("link", { name: "モデル" }).click();
    // 完全一致で引く。部分一致だと、データ読込後に出る「モデルカタログ」パネルの見出しにも当たり、
    // 読込が先に終わると strict mode violation で落ちる（タイミング依存のフレーキー）。
    await expect(
      page.getByRole("heading", { name: "モデル", exact: true }),
    ).toBeVisible();
    await expect(
      page.getByRole("radio", { name: "Auto を既定にする" }),
    ).toBeChecked();

    // 円換算レートを入れて保存できる（保存後は読み直した値が入っている）。
    // 再実行でも差分が出るよう、いまの値と違う値を入れる。
    const rate = page.getByLabel("円換算レート");
    const next = (await rate.inputValue()) === "150" ? "151" : "150";
    await rate.fill(next);
    await page.getByRole("button", { name: "変更を保存" }).click();
    await expect(
      page.getByText("AI の設定を保存しました", { exact: true }),
    ).toBeVisible();
    await expect(rate).toHaveValue(next);

    // 範囲外は保存ボタンが無効で理由が出る。
    await rate.fill("5000");
    await expect(
      page.getByText("円換算レートは 1〜1000 円で入力してください"),
    ).toBeVisible();
    await expect(
      page.getByRole("button", { name: "変更を保存" }),
    ).toBeDisabled();
    await page.getByRole("button", { name: "取り消す" }).click();

    // プロバイダ: 接続先が 1 つ以上出る（秘密は出さない）。
    await page.getByRole("link", { name: "プロバイダ" }).click();
    await expect(
      page.getByRole("heading", { name: "プロバイダ" }),
    ).toBeVisible();
    await expect(page.getByText("API キー").first()).toBeVisible();
  });

  test("一般メンバーには管理コンソールが開かない", async ({ page }) => {
    await loginAs(page, "bob");
    await page
      .getByRole("button", { name: "アカウントメニューを開く" })
      .click();
    await expect(
      page.getByRole("menuitem", { name: "管理コンソール" }),
    ).toHaveCount(0);
    await page.keyboard.press("Escape");
    await page.goto("/admin/ai/models");
    await expect(page.getByText("組織の管理者だけが使えます")).toBeVisible();
  });
});

test.describe("チャットのモデル選択", () => {
  test("送信の左に常に出て、選ぶと「モデル名・思考の深さ」になり、Auto に戻せる", async ({
    page,
  }) => {
    await loginViaKeycloak(page);
    await page.goto("/");
    const button = page.getByTestId("model-button");
    await expect(button).toHaveText(/Auto/);

    // 一覧（#583）の先頭のモデルを選ぶ。
    await button.click();
    await expect(page.getByTestId("model-picker")).toBeVisible();
    const first = page
      .locator(
        '[data-testid^="model-option-"]:not([data-testid="model-option-default"])',
      )
      .first();
    const label = (await first.locator("[data-label]").innerText()).trim();
    await first.click();
    await page.keyboard.press("Escape");
    await expect(page.getByTestId("model-picker")).toHaveCount(0);
    await expect(button).toContainText(label);

    // 送信すると新しいスレッドへ選択が引き継がれる。
    await page.getByLabel("メッセージを入力").fill("こんにちは");
    await page.getByRole("button", { name: "送信" }).click();
    await page.waitForURL(/\/c\/[0-9a-f-]+/i, { timeout: 20_000 });
    await expect(page.getByTestId("model-button")).toContainText(label);

    // 一覧から Auto に戻せる。
    await page.getByTestId("model-button").click();
    await page.getByRole("option", { name: /Auto（おまかせ）/ }).click();
    await expect(page.getByTestId("model-button")).toHaveText(/Auto/);
  });
});

test.describe("チャットのツール", () => {
  test("選んだツールは本文の先頭に入り、押すか先頭で Backspace を押すと外れる", async ({
    page,
  }) => {
    await loginViaKeycloak(page);
    await page.goto("/");
    const input = page.getByLabel("メッセージを入力");

    await page.getByRole("button", { name: "追加メニューを開く" }).click();
    await page.getByTestId("composer-tool-web_search").click();
    await page.getByTestId("composer-tool-code_interpreter").click();
    await page.keyboard.press("Escape");
    const tokens = page.getByTestId("composer-tool-chip");
    await expect(tokens).toHaveCount(2);

    // 続けて打った文字は語の右から始まる（1 行目だけ字下げ）。入力欄は Markdown の WYSIWYG
    // （contenteditable）なので、fill ではなく打鍵で入れる（カーソルの同期を実際の操作に揃える）。
    await input.click();
    await input.pressSequentially("集計して");
    // 入力欄は Markdown の WYSIWYG（contenteditable）なので、字下げは先頭の段落に付く。
    const indent = await input
      .locator(":scope > :first-child")
      .evaluate((el) => parseFloat(getComputedStyle(el).textIndent));
    expect(indent).toBeGreaterThan(40);

    // 本文の先頭で Backspace → 最後の語（コード実行）が外れる。本文は消えない。
    await input.press("Home");
    await input.press("Backspace");
    await expect(tokens).toHaveCount(1);
    await expect(input).toHaveText("集計して");

    // 語を押しても外れる。
    await page.getByRole("button", { name: "Web 検索を外す" }).click();
    await expect(tokens).toHaveCount(0);
    expect(
      await input
        .locator(":scope > :first-child")
        .evaluate((el) => getComputedStyle(el).textIndent),
    ).toBe("0px");
  });
});

// 実API・Keycloak・OpenFGAを通して保存と権限境界を確認する。
test.describe("AD連携とロール管理", () => {
  test("ロールを作成・保存し、再読込後も編集できる", async ({ page }) => {
    await loginViaKeycloak(page);
    await page.goto("/admin/roles");
    const name = `E2Eロール-${Date.now()}`;
    await page
      .getByRole("button", { name: "ロールを作成", exact: true })
      .click();
    await page.getByLabel("ロール名", { exact: true }).fill(name);
    await page
      .getByRole("button", { name: "編集内容に追加", exact: true })
      .click();
    await page
      .getByRole("button", { name: "ロールを作成", exact: true })
      .click();
    await page.getByLabel("ロール名", { exact: true }).fill(`${name}のチーム`);
    await page.getByRole("button", { name: "親を選ぶ", exact: true }).click();
    await page
      .getByRole("radio", { name: `${name}を親にする`, exact: true })
      .click();
    await page
      .getByRole("button", { name: "編集内容に追加", exact: true })
      .click();
    await page.getByRole("button", { name: "変更を確認", exact: true }).click();
    await expect(page.getByRole("dialog")).toContainText(name);
    await page.getByRole("button", { name: "反映する", exact: true }).click();
    await expect(
      page.getByText("反映が完了しました。", { exact: false }),
    ).toBeVisible({ timeout: 60_000 });
    await page.reload();
    await page
      .getByRole("searchbox", { name: "ロールを検索", exact: true })
      .fill(name);
    await page
      .getByRole("button", { name: `${name}を編集`, exact: true })
      .click();
    const renamed = `${name}-変更`;
    await page.getByLabel("ロール名", { exact: true }).fill(renamed);
    await page
      .getByRole("button", { name: "編集内容に追加", exact: true })
      .click();
    await page.getByRole("button", { name: "変更を確認", exact: true }).click();
    const dialog = page.getByRole("dialog");
    await expect(
      dialog.getByText("既存ロールの変更", { exact: true }),
    ).toBeVisible();
    await expect(dialog).toContainText(renamed);
    await page.getByRole("button", { name: "反映する", exact: true }).click();
    await expect(
      page.getByText("反映が完了しました。", { exact: false }),
    ).toBeVisible({ timeout: 60_000 });
  });
  test("AD接続設定で秘密を再表示せず、未許可の接続先を案内する", async ({
    page,
  }) => {
    await loginViaKeycloak(page);
    await page.goto("/admin/directory");
    await page.getByRole("button", { name: "接続設定", exact: true }).click();
    await expect(page.getByRole("dialog")).toContainText("AD接続設定");
    await expect(
      page.getByLabel("バインド用パスワード", { exact: true }),
    ).toHaveValue("");
    await expect(
      page.getByRole("button", { name: "テストして保存", exact: true }),
    ).toBeDisabled();
  });
  test("一般メンバーは管理APIと画面を利用できない", async ({ page }) => {
    await loginAs(page, "bob");
    await page.goto("/admin/directory");
    await expect(page.getByText("組織の管理者だけが使えます")).toBeVisible();
    const status = await page.evaluate(
      async () => (await fetch("/api/org-admin/directory/roles")).status,
    );
    expect(status).toBe(403);
  });
});
