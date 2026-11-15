import { test, expect } from "@playwright/test";

import { loginAs, loginViaKeycloak } from "./helpers";
import { importMapping } from "../src/components/admin/directory-import";
import { planAutoRoles } from "../src/components/admin/directory-auto-plan";
import { DEFAULT_ROLE_CATALOG } from "../src/components/admin/directory-role-catalog";
import { AD_GROUPS, INITIAL_MAPPING } from "../src/components/admin/mock-directory-model";

/// 管理コンソール（AI）とチャットのモデル選択（#553）の E2E。
/// 前提: dev seed で alice が組織管理者（organization#admin）、bob は一般メンバー。LLM=stub。
test.describe("管理コンソール", () => {
  test("管理者はアカウントメニューから入り、AI の画面を開ける", async ({ page }) => {
    await loginViaKeycloak(page);
    await page.getByRole("button", { name: "アカウントメニューを開く" }).click();
    await page.getByRole("menuitem", { name: "管理コンソール" }).click();
    await page.waitForURL(/\/admin$/);
    await expect(page.getByRole("heading", { name: "ホーム" })).toBeVisible();

    // レールから AI へ（利用量が入口）。
    await page.getByRole("link", { name: "AI" }).click();
    await page.waitForURL(/\/admin\/ai\/usage$/);
    await expect(page.getByRole("heading", { name: "利用量" })).toBeVisible();
    await expect(page.getByRole("region", { name: "指標" })).toContainText("呼び出し");

    // モデル: カタログ表と Auto 行が出る。
    await page.getByRole("link", { name: "モデル" }).click();
    // 完全一致で引く。部分一致だと、データ読込後に出る「モデルカタログ」パネルの見出しにも当たり、
    // 読込が先に終わると strict mode violation で落ちる（タイミング依存のフレーキー）。
    await expect(page.getByRole("heading", { name: "モデル", exact: true })).toBeVisible();
    await expect(page.getByRole("radio", { name: "Auto を既定にする" })).toBeChecked();

    // 円換算レートを入れて保存できる（保存後は読み直した値が入っている）。
    // 再実行でも差分が出るよう、いまの値と違う値を入れる。
    const rate = page.getByLabel("円換算レート");
    const next = (await rate.inputValue()) === "150" ? "151" : "150";
    await rate.fill(next);
    await page.getByRole("button", { name: "変更を保存" }).click();
    await expect(page.getByText("AI の設定を保存しました", { exact: true })).toBeVisible();
    await expect(rate).toHaveValue(next);

    // 範囲外は保存ボタンが無効で理由が出る。
    await rate.fill("5000");
    await expect(page.getByText("円換算レートは 1〜1000 円で入力してください")).toBeVisible();
    await expect(page.getByRole("button", { name: "変更を保存" })).toBeDisabled();
    await page.getByRole("button", { name: "取り消す" }).click();

    // プロバイダ: 接続先が 1 つ以上出る（秘密は出さない）。
    await page.getByRole("link", { name: "プロバイダ" }).click();
    await expect(page.getByRole("heading", { name: "プロバイダ" })).toBeVisible();
    await expect(page.getByText("API キー").first()).toBeVisible();
  });

  test("一般メンバーには管理コンソールが開かない", async ({ page }) => {
    await loginAs(page, "bob");
    await page.getByRole("button", { name: "アカウントメニューを開く" }).click();
    await expect(page.getByRole("menuitem", { name: "管理コンソール" })).toHaveCount(0);
    await page.keyboard.press("Escape");
    await page.goto("/admin/ai/models");
    await expect(page.getByText("組織の管理者だけが使えます")).toBeVisible();
  });
});

test.describe("チャットのモデル選択", () => {
  test("送信の左に常に出て、選ぶと「モデル名・思考の深さ」になり、Auto に戻せる", async ({ page }) => {
    await loginViaKeycloak(page);
    await page.goto("/");
    const button = page.getByTestId("model-button");
    await expect(button).toHaveText(/Auto/);

    // 一覧（#583）の先頭のモデルを選ぶ。
    await button.click();
    await expect(page.getByTestId("model-picker")).toBeVisible();
    const first = page.locator('[data-testid^="model-option-"]:not([data-testid="model-option-default"])').first();
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
  test("選んだツールは本文の先頭に入り、押すか先頭で Backspace を押すと外れる", async ({ page }) => {
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

// AD連携はフロントエンド内のMock。実IdPに依存せず確認する。
test.describe("AD連携Mock", () => {
  test.beforeEach(async ({ page, context, baseURL }) => {
    await context.addCookies([{ name: "shiki_session", value: "mock-directory", url: baseURL ?? "http://localhost:3000" }]);
    await page.route("**/api/me", (route) => route.fulfill({ json: {
      user_id: "mock", display_name: "テスト管理者", email: "admin@example.co.jp",
      is_admin: true, can_view_audit: true,
    } }));
    await page.goto("/admin/directory");
  });

  test("OUと入れ子を区別し、親ロールの所属を上方向だけに展開する", async ({ page }) => {
    await page.getByRole("tab", { name: "グループの入れ子", exact: true }).click();
    await expect(page.getByRole("button", { name: /SG-Sales-1/ })).toHaveCount(2);
    await page.getByRole("tab", { name: "OUで探す", exact: true }).click();
    await expect(page.getByRole("button", { name: /SG-Sales-1/ })).toHaveCount(1);
    await page.getByText("ユーザーへの反映を確認", { exact: true }).click();
    await page.getByLabel("確認するユーザー").selectOption("佐藤 美咲");
    await expect(page.getByTestId("directory-direct-roles")).toHaveText("営業部、営業1課");
    await page.getByLabel("ADグループの入れ子を展開して試す").uncheck();
    await expect(page.getByTestId("directory-direct-roles")).toHaveText("営業1課");
    await expect(page.getByTestId("directory-inherited-roles")).toHaveText(/営業部/);
    await page.getByLabel("確認するユーザー").selectOption("小林 大輔");
    await expect(page.getByTestId("directory-direct-roles")).toHaveText("営業部");
    await expect(page.getByTestId("directory-inherited-roles")).toHaveText("全社");
    await page.getByRole("button", { name: /SG-Planning/ }).click();
    await page.getByLabel("ロールを検索").fill("planning");
    await page.getByRole("checkbox", { name: "経営企画 planning" }).check();
    await page.getByLabel("確認するユーザー").selectOption("山田 葵");
    await expect(page.getByTestId("directory-direct-roles")).toHaveText("経営企画");
    await page.getByRole("button", { name: "変更を取り消す", exact: true }).click();
    await expect(page.getByTestId("directory-direct-roles")).toHaveText("対応するロールがありません。");
  });

  test("接続失敗から再試行できる", async ({ page }) => {
    await page.getByRole("button", { name: /接続設定/ }).click();
    const dialog = page.getByRole("dialog");
    await dialog.getByLabel("接続失敗をシミュレート").check();
    await dialog.getByRole("button", { name: "接続をテスト", exact: true }).click();
    await expect(dialog.getByRole("status")).toContainText("接続テスト失敗");
    await dialog.getByLabel("接続失敗をシミュレート").uncheck();
    await dialog.getByRole("button", { name: "接続をテスト", exact: true }).click();
    await expect(dialog.getByRole("status")).toContainText("接続テスト成功");
  });

  test("CSVのエラーと差分を確認してから取り込み・反映する", async ({ page }) => {
    await page.getByRole("button", { name: "CSVインポート" }).click();
    const dialog = page.getByRole("dialog");
    const upload = dialog.getByLabel("対応表CSV");
    await upload.setInputFiles({ name: "bad.csv", mimeType: "text/csv", buffer: Buffer.from("group_id,role_id\nunknown,planning") });
    await expect(dialog.getByRole("alert")).toContainText("グループID");
    await expect(dialog.getByRole("button", { name: "編集内容に取り込む" })).toBeDisabled();
    await upload.setInputFiles({ name: "mapping.csv", mimeType: "text/csv", buffer: Buffer.from("group_id,role_id\n00000000-0000-4000-8000-000000000005,planning") });
    await expect(dialog.getByText(/1グループを変更/)).toBeVisible();
    await dialog.getByRole("button", { name: "キャンセル", exact: true }).click();
    await expect(page.getByRole("button", { name: "Mockに反映", exact: true })).toHaveCount(0);
    await page.getByRole("button", { name: "CSVインポート" }).click();
    await page.getByLabel("対応表CSV").setInputFiles({ name: "mapping.csv", mimeType: "text/csv", buffer: Buffer.from("group_id,role_id\n00000000-0000-4000-8000-000000000005,planning") });
    await page.getByRole("button", { name: "編集内容に取り込む" }).click();
    await page.getByText("ユーザーへの反映を確認", { exact: true }).click();
    await page.getByLabel("確認するユーザー").selectOption("山田 葵");
    await expect(page.getByTestId("directory-direct-roles")).toHaveText("経営企画");
    await page.getByRole("button", { name: "Mockに反映", exact: true }).click();
    await expect(page.getByRole("main").getByRole("status")).toContainText("1グループの対応付けをMockに反映しました");
    await page.reload();
    await expect(page.getByRole("button", { name: "Mockに反映", exact: true })).toHaveCount(0);
  });
});


test.describe("AD連携Mock CSV検証", () => {
  test("BOM・引用符・複数ロールを処理し、指定のないグループを保持する", () => {
    const id = AD_GROUPS[4].id;
    const result = importMapping(`\uFEFFgroup_id,role_id\r\n"${id}","planning"\r\n${id},dev`, INITIAL_MAPPING);
    expect(result.errors).toEqual([]);
    expect(result.mapping[id]).toEqual(["planning", "dev"]);
    expect(result.mapping[AD_GROUPS[0].id]).toEqual(["sales"]);
    expect(INITIAL_MAPPING[id]).toEqual([]);
  });
  test("空のロールで解除し、不明ID・重複・引用符不正を拒否する", () => {
    const id = AD_GROUPS[0].id;
    expect(importMapping(`group_id,role_id\n${id},`, INITIAL_MAPPING).mapping[id]).toEqual([]);
    for (const body of [
      "unknown,sales", `${id},unknown`, `${id},sales\n${id},sales`,
      `${id},\n${id},sales`, `"${id},sales`, `${id},sales,extra`,
    ]) expect(importMapping(`group_id,role_id\n${body}`, INITIAL_MAPPING).errors.length).toBeGreaterThan(0);
  });
});


test.describe("AD連携Mock 大規模ロール", () => {
  test.beforeEach(async ({ page, context, baseURL }) => {
    await context.addCookies([{ name: "shiki_session", value: "mock-directory", url: baseURL ?? "http://localhost:3000" }]);
    await page.route("**/api/me", (route) => route.fulfill({ json: { user_id: "mock", is_admin: true, can_view_audit: true } }));
    await page.goto("/admin/directory");
  });
  test("1万件超から階層を含めて検索し、選択を保持したまま一覧を切り替えられる", async ({ page }) => {
    const browser = page.getByTestId("directory-role-browser");
    await expect(page.getByText("ロール 10,006件", { exact: true })).toBeVisible();
    const search = page.getByLabel("ロールを検索");
    await search.fill("全社");
    await expect(browser).toContainText("10,006件中 1〜40件");
    await expect(browser.getByTestId("directory-role-row")).toHaveCount(40);
    await search.fill("事業部099 チーム099");
    const rare = page.getByRole("checkbox", { name: "チーム099 division-099/team-099", exact: true });
    await rare.check();
    await page.getByRole("tab", { name: "選択済み 2", exact: true }).click();
    await expect(search).toHaveValue("");
    await expect(rare).toBeChecked();
    await page.getByRole("tab", { name: "ロールを探す", exact: true }).click();
    await search.fill("no-such-role");
    await expect(browser.getByText("該当するロールはありません")).toBeVisible();
    await search.fill("division-099/team-099");
    await expect(rare).toBeChecked();
    await page.getByRole("button", { name: "Mockに反映", exact: true }).click();
    await expect(page.getByRole("main").getByRole("status")).toContainText("1グループの対応付けをMockに反映しました");
  });
  test("階層の子一覧を40件で区切り、ページを移ってもチェックを保持する", async ({ page }) => {
    const browser = page.getByTestId("directory-role-browser");
    await browser.getByRole("button", { name: "全社", exact: true }).click();
    await expect(browser.getByTestId("directory-role-row")).toHaveCount(40);
    const first = browser.getByRole("checkbox", { name: "事業部001 division-001", exact: true });
    await first.check();
    await page.getByRole("button", { name: "ロールの次のページ", exact: true }).click();
    await expect(browser).toContainText("41〜80件");
    await expect(browser.getByTestId("directory-role-row")).toHaveCount(40);
    await page.getByRole("button", { name: "ロールの前のページ", exact: true }).click();
    await expect(first).toBeChecked();
    await browser.getByRole("button", { name: "事業部001の配下を開く", exact: true }).click();
    await expect(browser).toContainText("99件中 1〜40件");
    await expect(browser.getByTestId("directory-role-row")).toHaveCount(40);
  });
});


test.describe("AD連携Mock ロールの簡易管理", () => {
  test.beforeEach(async ({ page, context, baseURL }) => {
    await context.addCookies([{ name: "shiki_session", value: "mock-directory", url: baseURL ?? "http://localhost:3000" }]);
    await page.route("**/api/me", (route) => route.fulfill({ json: { user_id: "mock", is_admin: true, can_view_audit: true } }));
    await page.goto("/admin/directory");
  });
  test("作成から対応付け・名前変更・親変更・反映まで画面内で完結する", async ({ page }) => {
    await page.getByRole("button", { name: "ロールを追加", exact: true }).click();
    let dialog = page.getByRole("dialog");
    await expect(dialog.getByTestId("role-editor-parent")).toHaveText("全社 / 営業部");
    await dialog.getByLabel("ロール名", { exact: true }).fill("営業推進チーム");
    await dialog.getByRole("button", { name: "作成して対応付ける", exact: true }).click();
    const created = page.getByRole("checkbox", { name: /^営業推進チーム custom-/ });
    await expect(created).toBeChecked();
    await page.getByRole("button", { name: "営業推進チームを編集", exact: true }).click();
    dialog = page.getByRole("dialog");
    await dialog.getByLabel("ロール名", { exact: true }).fill("営業支援チーム");
    await dialog.getByRole("button", { name: "親を変更", exact: true }).click();
    await dialog.getByLabel("親ロールを検索").fill("経営企画");
    await dialog.getByRole("button", { name: "全社 / 経営企画", exact: true }).click();
    await dialog.getByRole("button", { name: "変更を編集内容に追加", exact: true }).click();
    await expect(page.getByRole("checkbox", { name: /^営業支援チーム custom-/ })).toBeChecked();
    await page.getByText("ユーザーへの反映を確認", { exact: true }).click();
    await expect(page.getByTestId("directory-inherited-roles")).toContainText("経営企画");
    await page.getByText("変更内容を確認", { exact: true }).click();
    await expect(page.getByText("ロール追加: 営業支援チーム", { exact: true })).toBeVisible();
    await page.getByRole("button", { name: "Mockに反映", exact: true }).click();
    await expect(page.getByRole("main").getByRole("status")).toContainText("ロール1件の変更も反映しました");
  });
  test("同名・循環を防ぎ、取消でロールと対応付けを元に戻す", async ({ page }) => {
    await page.getByRole("button", { name: "営業1課を編集", exact: true }).click();
    const dialog = page.getByRole("dialog");
    await dialog.getByLabel("ロール名", { exact: true }).fill("営業2課");
    await dialog.getByRole("button", { name: "変更を編集内容に追加", exact: true }).click();
    await expect(dialog.getByRole("alert")).toContainText("同名のロール");
    await dialog.getByRole("button", { name: "親を変更", exact: true }).click();
    await dialog.getByLabel("親ロールを検索").fill("sales-1");
    await expect(dialog.getByText("候補がありません。自身と配下のロールは選べません。", { exact: true })).toBeVisible();
    await dialog.getByRole("button", { name: "キャンセル", exact: true }).click();
    await page.getByRole("button", { name: "ロールを追加", exact: true }).click();
    await page.getByLabel("ロール名", { exact: true }).fill("取消テスト");
    await page.getByRole("button", { name: "作成して対応付ける", exact: true }).click();
    await expect(page.getByRole("checkbox", { name: /^取消テスト custom-/ })).toBeChecked();
    await page.getByRole("button", { name: "変更を取り消す", exact: true }).click();
    await expect(page.getByRole("checkbox", { name: /^取消テスト custom-/ })).toHaveCount(0);
    await expect(page.getByText("ロール 10,006件", { exact: true })).toBeVisible();
  });
});


test("AD連携Mockで対応付けずに作成したロールもすぐ確認・反映できる", async ({ page, context, baseURL }) => {
  await context.addCookies([{ name: "shiki_session", value: "mock", url: baseURL ?? "http://localhost:3000" }]);
  await page.route("**/api/me", (route) => route.fulfill({ json: { user_id: "mock", is_admin: true, can_view_audit: true } }));
  await page.goto("/admin/directory");
  await page.getByLabel("ロールを検索").fill("新設部署");
  await expect(page.getByText("該当するロールはありません", { exact: true })).toBeVisible();
  await page.getByRole("button", { name: "ロールを追加", exact: true }).first().click();
  const dialog = page.getByRole("dialog");
  await expect(dialog.getByLabel("ロール名", { exact: true })).toHaveValue("新設部署");
  await dialog.getByRole("checkbox", { name: /作成後に/ }).uncheck();
  await dialog.getByRole("button", { name: "ロールを作成", exact: true }).click();
  await expect(page.getByRole("checkbox", { name: /^新設部署 custom-/ })).not.toBeChecked();
  await page.getByRole("button", { name: "Mockに反映", exact: true }).click();
  await expect(page.getByRole("main").getByRole("status")).toContainText("ロール1件の変更も反映しました");
});


test("AD連携Mock 自動作成計画は名前・階層・既存対応を保持し、再実行で増殖しない", () => {
  const plan = planAutoRoles(DEFAULT_ROLE_CATALOG, INITIAL_MAPPING, AD_GROUPS.map((g) => g.id), "ou", true);
  expect(plan.errors).toEqual([]);
  expect(plan.rows).toHaveLength(6);
  const sales1 = plan.rows.find((row) => row.group.name === "SG-Sales-1")!;
  expect(sales1.path).toBe("全社 / 営業本部 / 営業1課 / SG-Sales-1");
  expect(sales1.after).toContain("sales-1");
  expect(plan.catalog.index.get(sales1.target)?.label).toBe("SG-Sales-1");
  expect(INITIAL_MAPPING[AD_GROUPS[1].id]).toEqual(["sales-1"]);
  const repeated = planAutoRoles(plan.catalog, plan.mapping, AD_GROUPS.map((g) => g.id), "ou", true);
  expect(repeated.nodes.filter((n) => n.action === "create")).toHaveLength(0);
  expect(repeated.changed).toBe(0);
  expect(repeated.catalog.count).toBe(plan.catalog.count);
  const changed = plan.catalog.save({ ...plan.catalog.index.get(sales1.target)!, label: "手動で変更した名前" });
  expect(planAutoRoles(changed, plan.mapping, [AD_GROUPS[1].id], "ou", true).errors.join()).toContain("自動で上書きできません");
});

test("AD連携Mock 入れ子の複数親・循環を平坦化せず停止する", () => {
  const plan = planAutoRoles(DEFAULT_ROLE_CATALOG, INITIAL_MAPPING, AD_GROUPS.map((g) => g.id), "membership", true);
  expect(plan.errors.join()).toContain("親が複数");
  expect(plan.catalog).toBe(DEFAULT_ROLE_CATALOG);
  expect(plan.mapping).toBe(INITIAL_MAPPING);
  const branch = planAutoRoles(DEFAULT_ROLE_CATALOG, INITIAL_MAPPING, [AD_GROUPS[3].id], "membership", true);
  expect(branch.errors).toEqual([]);
  expect(branch.rows.map((row) => row.group.name).sort()).toEqual(["SG-Engineering", "SG-Project-A"]);
  const cyclic = AD_GROUPS.slice(0, 2).map((g, i) => ({ ...g, groups: [AD_GROUPS[1 - i].id] }));
  expect(planAutoRoles(DEFAULT_ROLE_CATALOG, INITIAL_MAPPING, [cyclic[0].id], "membership", true, cyclic).errors.join()).toContain("循環");
});

test.describe("AD連携Mock 自動作成", () => {
  test.beforeEach(async ({ page, context, baseURL }) => {
    await context.addCookies([{ name: "shiki_session", value: "mock", url: baseURL ?? "http://localhost:3000" }]);
    await page.route("**/api/me", (route) => route.fulfill({ json: { user_id: "mock", is_admin: true, can_view_audit: true } }));
    await page.goto("/admin/directory");
    await page.getByRole("button", { name: "ADから自動作成", exact: true }).click();
  });
  test("選択対象の階層を確認し、キャンセル・取込・取消ができる", async ({ page }) => {
    const dialog = page.getByRole("dialog");
    await expect(dialog.getByRole("checkbox", { name: "SG-Sales-1を自動作成の対象にする", exact: true })).toBeChecked();
    await dialog.getByRole("button", { name: "作成内容を確認", exact: true }).click();
    await expect(dialog.getByTestId("auto-role-target-path")).toBeVisible();
    await expect(dialog.locator('[aria-label="選択項目の対応付け"]').getByText("営業1課", { exact: true })).toBeVisible();
    await page.keyboard.press("Escape");
    await expect(page.getByText("ロール 10,006件", { exact: true })).toBeVisible();
    await page.getByRole("button", { name: "ADから自動作成", exact: true }).click();
    await dialog.getByRole("button", { name: "作成内容を確認", exact: true }).click();
    await dialog.getByRole("button", { name: "この内容で作成", exact: true }).click();
    await expect(page.getByText("ロール 10,009件", { exact: true })).toBeVisible();
    await page.getByRole("button", { name: "変更を取り消す", exact: true }).click();
    await expect(page.getByText("ロール 10,006件", { exact: true })).toBeVisible();
  });
  test("全件の複数親を明示し、OU配置へ切り替えて反映できる", async ({ page }) => {
    const dialog = page.getByRole("dialog");
    await dialog.getByRole("tab", { name: "すべて 6件", exact: true }).click();
    await dialog.getByRole("radio", { name: /グループの入れ子と名前/ }).check();
    await dialog.getByRole("button", { name: "作成内容を確認", exact: true }).click();
    await expect(dialog.getByRole("alert")).toContainText("親が複数");
    await expect(dialog.getByRole("button", { name: "この内容で作成", exact: true })).toBeDisabled();
    await dialog.getByRole("button", { name: "OUの配置で確認し直す", exact: true }).click();
    await expect(dialog.getByRole("alert")).toHaveCount(0);
    await dialog.getByRole("button", { name: "この内容で作成", exact: true }).click();
    await page.getByRole("button", { name: "Mockに反映", exact: true }).click();
    await page.getByRole("button", { name: "ADから自動作成", exact: true }).click();
    await dialog.getByRole("tab", { name: "すべて 6件", exact: true }).click();
    await dialog.getByRole("button", { name: "作成内容を確認", exact: true }).click();
    await expect(dialog.getByText("すべて作成・対応付け済みです。追加の変更はありません。", { exact: true })).toBeVisible();
    await expect(dialog.getByRole("button", { name: "この内容で作成", exact: true })).toBeDisabled();
  });
});
