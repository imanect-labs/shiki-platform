import { test, expect } from "@playwright/test";

import { cleanupQueue, deleteArtifactApi, loginViaKeycloak, trackCreatedResources, uniqueName } from "./helpers";

/// 画面から作ったスキルを、テストの後に片付ける（画面の応答から拾う・失敗したテストでも走る・BUG-126）。
const cleanup = cleanupQueue();
test.beforeEach(({ page }) => trackCreatedResources(page, cleanup));
test.afterEach(() => cleanup.run());

/// skill 管理と適用（Phase 6 Task 6.7/6.9/6.11）の E2E。
/// 前提: LLM=stub（skill を選んでも stub は「回答: <質問>」を返す＝適用経路の疎通を検証）。
test.describe("skill（作成・共有・チャット適用）", () => {
  test("UI から skill を作成し、選択してチャットを開始できる", async ({ page }) => {
    await loginViaKeycloak(page);
    await page.goto("/skills");

    // スキルエディタ（#919・専用ページ）。
    // 一覧が空のときは空状態にも同名の作成ボタンが出るため先頭（見出し行）を押す。
    await page.getByRole("button", { name: "スキルを作成" }).first().click();
    await page.waitForURL(/\/skills\/new/);
    const name = uniqueName("e2e-skill");
    await page.getByTestId("skill-name-input").fill(name);
    await page.getByTestId("skill-description-input").fill("E2E 検証用のスキル");
    await page.getByTestId("skill-body-editor").click();
    await page.keyboard.type("あなたは E2E テストのアシスタントです。");
    await page.getByTestId("skill-save-button").click();

    // 作成すると編集画面（/skills/{id}）へ移り、保存済みの版が出る。
    await page.waitForURL(/\/skills\/[0-9a-f-]{36}/i, { timeout: 15_000 });
    await expect(page.getByText("保存済み v1").first()).toBeVisible();

    // 一覧に現れる。
    await page.goto("/skills");
    await expect(page.getByRole("heading", { name })).toBeVisible({ timeout: 15_000 });

    // 共有ダイアログが開き、bob に閲覧権限を付与できる（Task 6.7 ロール共有の実経路）。
    const card = page.locator("li", { has: page.getByRole("heading", { name }) });
    // カードの操作は主ボタン＋⋯（共有・版などは ⋯ の中）。
    await card.getByRole("button", { name: `${name} のその他の操作` }).click();
    await page.getByRole("menuitem", { name: "共有" }).click();
    await expect(page.getByRole("dialog")).toContainText("を共有");
    await page.getByPlaceholder("名前・メールで検索").fill("bob");
    const bobRow = page.getByRole("dialog").locator("li", { hasText: "bob" }).first();
    await bobRow.getByRole("button", { name: "共有" }).click();
    await expect(page.getByRole("dialog").getByText("共有中の相手")).toBeVisible();
    // 閉じるボタンで明示的に閉じる（`workflows-history.spec.ts` と同じ作法）。
    // Escape ではこのダイアログが閉じないことが CI で確認できている（5s 経っても
    // data-state="open" のまま）。他の spec にも Escape を `.catch()` で握り潰している
    // 箇所があり、Escape での dismiss は当てにできない。
    //
    // 閉じ切るまで待つのも必須。Radix Dialog は開いている間、背景コンテンツに
    // aria-hidden を付けるため、閉じる前に背景をロール検索すると要素がアクセシビリティ
    // ツリーに現れず、原因の分からない 60s タイムアウトになる（CI で実際に発生した）。
    await page.getByRole("dialog").getByRole("button", { name: "閉じる" }).click();
    await expect(page.getByRole("dialog")).toBeHidden();

    // バージョン履歴が開く。
    await card.getByRole("button", { name: `${name} のその他の操作` }).click();
    await page.getByRole("menuitem", { name: "バージョン履歴" }).click();
    // 行は data-testid で数える（#380）。`getByText("v1")` は部分一致のため行コンテナと
    // ラベル span の両方に当たり得て strict mode 違反になる（たまたま通っていた）。
    const versionsDialog = page.getByRole("dialog").filter({ hasText: "のバージョン履歴" });
    const versionRows = versionsDialog.getByTestId("version-row");
    await expect(versionRows).toHaveCount(1);
    await expect(versionRows.first().getByTestId("version-label")).toHaveText("v1");
    await page.getByRole("dialog").getByRole("button", { name: "閉じる" }).click();
    await expect(page.getByRole("dialog")).toBeHidden();

    // このスキルでチャット → skill ピン付きスレッドが作られ、生成が通る（適用経路の疎通）。
    await card.getByRole("button", { name: "このスキルでチャット" }).click();
    await page.waitForURL(/\/c\/[0-9a-f-]+/i, { timeout: 20_000 });
    const input = page.getByLabel("メッセージを入力");
    await input.fill("経費について教えて");
    await page.getByRole("button", { name: "送信" }).click();
    await expect(page.getByText(/回答/).first()).toBeVisible({ timeout: 30_000 });
  });

  test("ホームでスキルを選んでチャットを開始できる", async ({ page }) => {
    await loginViaKeycloak(page);
    // 前のテストに頼らず、このテストのスキルを作る（前のテストのスキルは片付けで消える・BUG-126）。
    const csrf = (await page.context().cookies()).find((c) => c.name === "shiki_csrf")?.value ?? "";
    const created = await page.request.post("/api/skills", {
      headers: { "Content-Type": "application/json", "X-CSRF-Token": csrf },
      data: {
        name: uniqueName("e2e-skill-home"),
        body: {
          description: "ホームから選ぶスキル",
          instructions: "あなたは E2E テストのアシスタントです。",
          knowledge_scope: null,
          allowed_tools: null,
          model: null,
        },
      },
    });
    expect(created.ok(), await created.text()).toBeTruthy();
    const skillId = ((await created.json()) as { id: string }).id;
    cleanup.add(() => deleteArtifactApi(page, skillId));
    await page.goto("/");

    // 作ったスキルがピッカーに出る（読み込みは非同期なので待つ）。
    const picker = page.getByLabel("スキルを選択");
    await picker.waitFor({ timeout: 10_000 });

    await picker.getByRole("button").first().click();
    const input = page.getByLabel("メッセージを入力");
    await input.fill("こんにちは");
    await page.getByRole("button", { name: "送信" }).click();
    await page.waitForURL(/\/c\/[0-9a-f-]+/i, { timeout: 20_000 });
    await expect(page.getByText(/回答/).first()).toBeVisible({ timeout: 30_000 });
  });

  test("保存の誤りは 1 件 1 行で出し、位置（description など）を添えない（BUG-121）", async ({ page }) => {
    await loginViaKeycloak(page);
    // サーバーの検証の誤り（位置つき）を返させる。画面は文言だけを出す（ミニアプリのエディターと同じ扱い）。
    await page.route("**/api/skills", (route) =>
      route.request().method() === "POST"
        ? route.fulfill({
            status: 400,
            json: {
              errors: [
                { code: "skill.too_long", message: "説明が長すぎます。", path: "description" },
                { code: "skill.too_long", message: "答え方のお手本の 1 組目が長すぎます。", path: "few_shot[0]" },
              ],
            },
          })
        : route.continue(),
    );
    await page.goto("/skills");
    await page.getByRole("button", { name: "スキルを作成" }).first().click();
    const dialog = page.getByRole("dialog");
    await dialog.getByLabel(/名前/).fill(uniqueName("e2e-skill-error"));
    await dialog.getByLabel(/説明/).fill("E2E 検証用のスキル");
    await dialog.getByLabel(/指示文/).fill("あなたは E2E テストのアシスタントです。");
    await dialog.getByRole("button", { name: "作成" }).click();
    const alert = dialog.getByRole("alert");
    await expect(alert).toContainText("説明が長すぎます。", { timeout: 15_000 });
    expect(await alert.innerText()).toBe("説明が長すぎます。\n答え方のお手本の 1 組目が長すぎます。");
    await expect(alert).not.toContainText("（description）");
    await expect(alert).not.toContainText("few_shot[0]");
    await page.unroute("**/api/skills");
  });
});
