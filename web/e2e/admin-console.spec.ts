import { test, expect, type Page } from "@playwright/test";

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

test("CSVは最後に選択したファイルを使い、検証中の本文を保護する", async ({
  page,
}) => {
  await loginViaKeycloak(page);
  await page.goto("/admin/directory");
  await page.evaluate(() => {
    const original = File.prototype.text;
    File.prototype.text = async function () {
      const result = await original.call(this);
      await new Promise((resolve) =>
        setTimeout(resolve, this.name === "slow-a.csv" ? 300 : 10),
      );
      if (this.name === "slow-a.csv")
        (window as unknown as { csvReadAComplete: boolean }).csvReadAComplete =
          true;
      return result;
    };
  });
  await page
    .getByRole("button", { name: "CSVインポート", exact: true })
    .click();
  const file = page.getByLabel("CSVファイル", { exact: true });
  await file.setInputFiles({
    name: "slow-a.csv",
    mimeType: "text/csv",
    buffer: Buffer.from("group_id,role_id\ngroup-a,role-a\n"),
  });
  await file.setInputFiles({
    name: "fast-b.csv",
    mimeType: "text/csv",
    buffer: Buffer.from("group_id,role_id\ngroup-b,role-b\n"),
  });
  const csv = page.getByLabel("CSVの内容", { exact: true });
  await expect(csv).toHaveValue("group_id,role_id\ngroup-b,role-b\n");
  await page.waitForFunction(
    () =>
      (window as unknown as { csvReadAComplete?: boolean }).csvReadAComplete,
  );
  await expect(csv).toHaveValue("group_id,role_id\ngroup-b,role-b\n");
  let acceptRequest!: () => void;
  const gate = new Promise<void>((resolve) => {
    acceptRequest = resolve;
  });
  await page.route("**/api/org-admin/directory/import-plan", async (route) => {
    expect(route.request().postDataJSON().csv).toBe(
      "group_id,role_id\ngroup-b,role-b\n",
    );
    await gate;
    await route.fulfill({
      status: 400,
      contentType: "application/json",
      body: JSON.stringify({ message: "検証テストの応答" }),
    });
  });
  await page
    .getByRole("button", { name: "検証して確認へ", exact: true })
    .click();
  await expect(csv).toBeDisabled();
  await expect(file).toBeDisabled();
  acceptRequest();
  await expect(page.getByRole("alert")).toContainText("検証テストの応答");
  await expect(csv).toBeEnabled();
  await expect(csv).toHaveValue("group_id,role_id\ngroup-b,role-b\n");
});

test("中断した反映は確認してから再実行する", async ({ page }) => {
  await loginViaKeycloak(page);
  const id = "10000000-0000-4000-8000-000000000001";
  const stopped = {
    id,
    state: "failed",
    error_code: "execution_interrupted",
    attempts: 1,
    processed: 1,
    total: 3,
  };
  await page.route("**/api/org-admin/directory/operations", (route) =>
    route.fulfill({ json: [stopped] }),
  );
  let retries = 0;
  await page.route(
    `**/api/org-admin/directory/operations/${id}/retry`,
    (route) => {
      expect(route.request().postDataJSON()).toEqual({
        recovery_confirmed: true,
      });
      retries++;
      return route.fulfill({
        json: {
          ...stopped,
          state: "succeeded",
          error_code: null,
          processed: 3,
          attempts: 2,
        },
      });
    },
  );
  await page.goto("/admin/directory");
  await page
    .getByRole("button", { name: "続きから再実行", exact: true })
    .click();
  const dialog = page.getByRole("dialog", { name: "中断した反映の再実行" });
  await expect(dialog).toContainText(id);
  await expect(
    dialog.getByRole("button", { name: "確認して再実行" }),
  ).toBeDisabled();
  await dialog.getByRole("button", { name: "戻る", exact: true }).click();
  expect(retries).toBe(0);
  await page
    .getByRole("button", { name: "続きから再実行", exact: true })
    .click();
  await dialog.getByRole("checkbox").check();
  await dialog.getByRole("button", { name: "確認して再実行" }).click();
  await expect(dialog).toHaveCount(0);
  expect(retries).toBe(1);
});

test("接続の保存結果が不明な場合は復旧確認してから保存する", async ({
  page,
}) => {
  await loginViaKeycloak(page);
  await page.route("**/api/org-admin/directory", async (route) => {
    const response = await route.fetch();
    await route.fulfill({
      json: {
        ...(await response.json()),
        ldap_allowed_hosts: ["ad.example.test"],
      },
    });
  });
  const connection = {
    configured: true,
    enabled: true,
    connection_url: "ldaps://ad.example.test:636",
    users_dn: "OU=Users,DC=example",
    groups_dn: "OU=Groups,DC=example",
    bind_dn: "CN=Reader,DC=example",
    credential_saved: true,
    preserve_group_inheritance: false,
    recovery_required: true,
    identity_current: true,
  };
  let saved = false;
  await page.route("**/api/org-admin/directory/connection", async (route) => {
    if (route.request().method() === "PUT") {
      expect(route.request().postDataJSON().recovery_confirmed).toBe(true);
      expect(route.request().postDataJSON().bind_password).toBeNull();
      saved = true;
    }
    await route.fulfill({ json: { ...connection, recovery_required: !saved } });
  });
  await page.goto("/admin/directory");
  await page.getByRole("button", { name: "接続設定", exact: true }).click();
  const dialog = page.getByRole("dialog", { name: "AD接続設定" });
  await expect(dialog).toContainText("前回の保存結果を確認してください");
  const save = dialog.getByRole("button", {
    name: "テストして保存",
    exact: true,
  });
  await expect(save).toBeDisabled();
  await dialog
    .getByRole("checkbox", {
      name: "旧処理の停止とKeycloakの処理完了を確認しました",
    })
    .check();
  await expect(save).toBeEnabled();
  await save.click();
  await expect(dialog).toContainText("接続設定を保存しました。");
  await expect(
    dialog.getByText("前回の保存結果を確認してください", { exact: true }),
  ).toHaveCount(0);
  expect(saved).toBe(true);
});

test("旧AD形式の更新では対応付けの解除を確認してから保存する", async ({
  page,
}) => {
  await loginViaKeycloak(page);
  await page.route("**/api/org-admin/directory", async (route) => {
    const response = await route.fetch();
    await route.fulfill({
      json: {
        ...(await response.json()),
        ldap_allowed_hosts: ["ad.example.test"],
      },
    });
  });
  const connection = {
    configured: true,
    enabled: true,
    connection_url: "ldaps://ad.example.test:636",
    users_dn: "OU=Users,DC=example",
    groups_dn: "OU=Groups,DC=example",
    bind_dn: "CN=reader,DC=example",
    credential_saved: true,
    preserve_group_inheritance: false,
    recovery_required: false,
    identity_current: false,
  };
  let saved = false;
  await page.route("**/api/org-admin/directory/connection", async (route) => {
    if (route.request().method() === "PUT") {
      expect(route.request().postDataJSON().identity_reset_confirmed).toBe(
        true,
      );
      saved = true;
    }
    await route.fulfill({ json: { ...connection, identity_current: saved } });
  });
  await page.goto("/admin/directory");
  await page.getByRole("button", { name: "接続設定", exact: true }).click();
  const dialog = page.getByRole("dialog", { name: "AD接続設定" });
  await expect(dialog).toContainText("連携形式の更新が必要です");
  const save = dialog.getByRole("button", {
    name: "テストして保存",
    exact: true,
  });
  await expect(save).toBeDisabled();
  await dialog
    .getByRole("checkbox", {
      name: "旧対応付けを解除し、同期後に設定し直すことを確認しました",
    })
    .check();
  await save.click();
  await expect(
    dialog.getByText("連携形式の更新が必要です", { exact: true }),
  ).toHaveCount(0);
  expect(saved).toBe(true);
});

// 異常応答と応答順序は、ブラウザの通信境界で再現する。保存操作は行わない。
const reviewPrefix = "shiki_ui_test_";
const reviewRole = {
  id: `${reviewPrefix}sales`,
  label: "営業",
  parent_id: null,
  version: 1,
  path_ids: [`${reviewPrefix}sales`],
  path_labels: ["営業"],
  child_count: 0,
};
const reviewGroup = {
  id: "source-sales",
  name: "AD営業",
  role_ids: [reviewRole.id],
  mapping_version: 1,
  ou_path: [],
  parent_ids: [],
  claim_path: "/test-guid/sales",
  distinguished_name: null,
};
async function directoryUiFixture(page: Page) {
  await page.route("**/api/org-admin/directory", (route) =>
    route.fulfill({
      json: {
        role_id_prefix: reviewPrefix,
        keycloak_available: true,
        ldap_allowed_hosts: [],
      },
    }),
  );
  await page.route("**/api/org-admin/directory/operations", (route) =>
    route.fulfill({ json: [] }),
  );
  await page.route("**/api/org-admin/directory/folders?**", (route) =>
    route.fulfill({ json: { items: [], next_cursor: null } }),
  );
  await page.route("**/api/org-admin/directory/groups?**", (route) =>
    route.fulfill({
      json: { items: [reviewGroup], total: 1, next_cursor: null },
    }),
  );
  await page.route("**/api/org-admin/directory/roles?**", (route) => {
    const query = new URL(route.request().url()).searchParams;
    return route.fulfill({
      json: {
        items: query.get("parent") ? [] : [reviewRole],
        total: query.get("parent") ? 0 : 1,
        next_cursor: null,
      },
    });
  });
}

test("保存済み末端ロールに追加した未反映の子を階層から開ける", async ({
  page,
}) => {
  await loginViaKeycloak(page);
  await directoryUiFixture(page);
  await page.goto("/admin/roles");
  await expect(
    page.getByRole("button", { name: "営業の配下を開く", exact: true }),
  ).toHaveCount(0);
  await page.getByRole("button", { name: "ロールを作成", exact: true }).click();
  await page.getByLabel("ロール名", { exact: true }).fill("営業の子");
  await page.getByRole("button", { name: "親を選ぶ", exact: true }).click();
  await page
    .getByRole("radio", { name: "営業を親にする", exact: true })
    .click();
  await page
    .getByRole("button", { name: "編集内容に追加", exact: true })
    .click();
  await page
    .getByRole("button", { name: "営業の配下を開く", exact: true })
    .click();
  await expect(
    page.getByText("営業の子", { exact: true }).first(),
  ).toBeVisible();
});

test.describe("対応付けの比較画像", () => {
  test.use({ deviceScaleFactor: 2 });
  test("対応付けの現在欄は改名前、反映後欄は改名後を表示する", async ({
    page,
  }) => {
    await loginViaKeycloak(page);
    await directoryUiFixture(page);
    await page.route("**/api/org-admin/directory/preview", async (route) => {
      const changes = route.request().postDataJSON();
      await route.fulfill({
        json: {
          changes,
          nodes: changes.roles,
          before_roles: [reviewRole],
          sources: [reviewGroup],
          group_count: 1,
          existing_roles: 0,
        },
      });
    });
    await page.goto("/admin/directory");
    await page.getByRole("button", { name: "AD営業", exact: false }).click();
    await page.getByRole("button", { name: "営業を編集", exact: true }).click();
    await page.getByLabel("ロール名", { exact: true }).fill("法人営業");
    await page
      .getByRole("button", { name: "編集内容に追加", exact: true })
      .click();
    await page
      .getByRole("checkbox", { name: "法人営業を対応付ける", exact: true })
      .uncheck();
    await page
      .getByRole("checkbox", { name: "法人営業を対応付ける", exact: true })
      .check();
    await page.getByRole("button", { name: "変更を確認", exact: true }).click();
    const dialog = page.getByRole("dialog");
    await expect(
      dialog.getByText("現在", { exact: true }).locator(".."),
    ).toContainText("営業");
    await expect(
      dialog.getByText("現在", { exact: true }).locator(".."),
    ).not.toContainText("法人営業");
    await expect(
      dialog.getByText("反映後", { exact: true }).locator(".."),
    ).toContainText("法人営業");
    await page.screenshot({
      path: test.info().outputPath("mapping-light.png"),
      animations: "disabled",
    });
    await page.evaluate(() => document.documentElement.classList.add("dark"));
    await page.waitForTimeout(400);
    await page.screenshot({
      path: test.info().outputPath("mapping-dark.png"),
      animations: "disabled",
    });
    await page.setViewportSize({ width: 768, height: 900 });
    await page.screenshot({
      path: test.info().outputPath("mapping-narrow.png"),
      animations: "disabled",
    });
  });
});

test("再読込で古いグループの選択と対応を解除する", async ({ page }) => {
  await loginViaKeycloak(page);
  await directoryUiFixture(page);
  await page.goto("/admin/directory");
  await page.getByRole("button", { name: "AD営業", exact: false }).click();
  await expect(
    page.getByRole("checkbox", { name: "営業を対応付ける", exact: true }),
  ).toBeChecked();
  await page.getByRole("button", { name: "再読込", exact: true }).click();
  await expect(
    page.getByRole("heading", { name: "ADグループを選択", exact: true }),
  ).toBeVisible();
  await expect(
    page.getByRole("checkbox", { name: "営業を対応付ける", exact: true }),
  ).toHaveCount(0);
});

test("OUの取得に失敗したら移動前の子OUを操作できない", async ({ page }) => {
  await loginViaKeycloak(page);
  await directoryUiFixture(page);
  await page.route("**/api/org-admin/directory/folders?**", (route) => {
    const parent = JSON.parse(
      new URL(route.request().url()).searchParams.get("parent") ?? "[]",
    );
    if (parent[0] === "B")
      return route.fulfill({ status: 503, json: { message: "Bの取得失敗" } });
    return route.fulfill({
      json: {
        items: (parent.length ? ["Aの子"] : ["A", "B"]).map((name) => ({
          name,
          group_count: 1,
        })),
        next_cursor: null,
      },
    });
  });
  await page.goto("/admin/directory");
  await page.getByRole("button", { name: "A 1", exact: true }).click();
  await expect(
    page.getByRole("button", { name: "Aの子 1", exact: true }),
  ).toBeVisible();
  await page
    .getByRole("navigation", { name: "ADのOU階層" })
    .getByRole("button", { name: "すべて", exact: true })
    .click();
  await page.getByRole("button", { name: "B 1", exact: true }).click();
  await expect(
    page.getByRole("alert").filter({ hasText: "Bの取得失敗" }),
  ).toBeVisible();
  await expect(
    page.getByRole("button", { name: "Aの子 1", exact: true }),
  ).toHaveCount(0);
});

test("古い操作一覧の応答で受付済みの進捗を消さない", async ({ page }) => {
  await loginViaKeycloak(page);
  await directoryUiFixture(page);
  let calls = 0;
  let polling = 0;
  let release = () => {};
  const delayed = new Promise<void>((resolve) => {
    release = resolve;
  });
  await page.route("**/api/org-admin/directory/operations", async (route) => {
    calls += 1;
    if (calls > 1) await delayed;
    await route.fulfill({ json: [] });
  });
  const operation = {
    id: "10000000-0000-4000-8000-000000000001",
    state: "running",
    processed: 0,
    total: 1,
    attempts: 1,
    error_code: null,
  };
  await page.route("**/api/org-admin/directory/sync", (route) =>
    route.fulfill({ json: operation }),
  );
  await page.route(
    `**/api/org-admin/directory/operations/${operation.id}`,
    (route) => {
      polling += 1;
      return route.fulfill({ json: operation });
    },
  );
  await page.goto("/admin/directory");
  await expect(
    page.getByRole("button", { name: "ADを同期", exact: true }),
  ).toBeEnabled();
  await page.getByRole("button", { name: "再読込", exact: true }).click();
  await expect.poll(() => calls).toBe(2);
  await page.getByRole("button", { name: "ADを同期", exact: true }).click();
  await expect(page.getByText("サーバで反映中", { exact: true })).toBeVisible();
  release();
  await expect.poll(() => polling).toBeGreaterThan(0);
  await expect(page.getByText("サーバで反映中", { exact: true })).toBeVisible();
});

test("既存の子の親変更を未反映の階層一覧にも反映する", async ({ page }) => {
  await loginViaKeycloak(page);
  await directoryUiFixture(page);
  const a = {
    ...reviewRole,
    id: `${reviewPrefix}a`,
    label: "親A",
    path_ids: [`${reviewPrefix}a`],
    path_labels: ["親A"],
    child_count: 1,
  };
  const b = {
    ...reviewRole,
    id: `${reviewPrefix}b`,
    label: "親B",
    path_ids: [`${reviewPrefix}b`],
    path_labels: ["親B"],
  };
  const c = {
    ...reviewRole,
    id: `${reviewPrefix}c`,
    label: "子C",
    parent_id: a.id,
    path_ids: [a.id, `${reviewPrefix}c`],
    path_labels: ["親A", "子C"],
  };
  await page.route("**/api/org-admin/directory/roles?**", (route) => {
    const query = new URL(route.request().url()).searchParams;
    const ids = query.get("ids")?.split(",");
    const items = ids
      ? [a, b, c].filter((r) => ids.includes(r.id))
      : query.get("parent") === a.id
        ? [c]
        : query.get("parent") === b.id
          ? []
          : [a, b];
    return route.fulfill({
      json: { items, total: items.length, next_cursor: null },
    });
  });
  await page.goto("/admin/roles");
  await page
    .getByRole("button", { name: "親Aの配下を開く", exact: true })
    .click();
  await page.getByRole("button", { name: "子Cを編集", exact: true }).click();
  await page.getByRole("button", { name: "親を選ぶ", exact: true }).click();
  await page.getByRole("radio", { name: "親Bを親にする", exact: true }).click();
  await page
    .getByRole("button", { name: "編集内容に追加", exact: true })
    .click();
  await page
    .getByRole("navigation", { name: "ロール階層" })
    .getByRole("button", { name: "最上位", exact: true })
    .click();
  await expect(
    page.getByRole("button", { name: "親Aの配下を開く", exact: true }),
  ).toHaveCount(0);
  await page
    .getByRole("button", { name: "親Bの配下を開く", exact: true })
    .click();
  await expect(
    page.getByRole("button", { name: "子Cを編集", exact: true }),
  ).toBeVisible();
});
