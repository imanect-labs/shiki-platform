import { test, expect, type Page } from "@playwright/test";

import { loginAs, loginViaKeycloak, uniqueName } from "./helpers";

/// ミニアプリ（Phase 6 Task 6.10/6.11・#604）の E2E。
/// 画面は Yui の ```ui を含む Markdown で、ボタンは束ねたワークフローを押した本人の権限で起動する。
/// ①UI からアプリを作成（画面の雛形・プレビュー）②実行画面で入力 → 起動 → 起動の記録が成功まで進む
/// ③**本体だけ共有した相手**が部品の個別共有なしで使え、記録は本人の分だけ見える
/// ④会話の ```ui app を「アプリとして保存」してそのまま使える、を実ブラウザで検証する。

/// ログイン済みページのセッションで backend API を叩く（CSRF 二重送信つき）。
async function apiPost(page: Page, path: string, body: unknown): Promise<{ id: string }> {
  const cookies = await page.context().cookies();
  const csrf = cookies.find((c) => c.name === "shiki_csrf")?.value ?? "";
  const res = await page.request.post(`/api${path}`, {
    headers: { "Content-Type": "application/json", "X-CSRF-Token": csrf },
    data: body,
  });
  expect(res.ok(), `${path} → ${res.status()}`).toBeTruthy();
  return res.json();
}

/// 手動実行のワークフロー（最小の IR）を作る。
async function createFlow(page: Page, name: string) {
  await apiPost(page, "/workflows", {
    ir: {
      ir_version: 1,
      name,
      display_name: name,
      triggers: [{ kind: "interactive" }],
      nodes: [
        {
          id: "compute",
          type: "script.run",
          label: "計算する",
          params: { source: { inline: "function main(i){ return { ok: true }; }" } },
        },
      ],
      edges: [],
    },
  });
}

/// 画面の図に入力して「申請する」→ 確認 → 起動し、記録が成功になるまで待つ。
async function apply(page: Page, amount: string) {
  const figure = page.getByTestId("ui-figure");
  await expect(figure.getByRole("button", { name: "申請する" })).toBeEnabled({ timeout: 15_000 });
  await figure.getByLabel("金額").fill(amount);
  await figure.getByRole("radio", { name: "交通費" }).click();
  await figure.getByRole("button", { name: "申請する" }).click();
  await figure.getByRole("group", { name: "実行の確認" }).getByRole("button", { name: "実行する" }).click();
  await expect(figure.getByText(/を起動しました/)).toBeVisible({ timeout: 30_000 });
}

test.describe("ミニアプリ（画面の作成・実行・バンドル権限の共有）", () => {
  test("作成 → ボタンで起動 → 共有相手も使え、記録は本人の分だけ", async ({ page, browser }) => {
    test.setTimeout(180_000);
    await loginViaKeycloak(page);
    const flow = uniqueName("app-flow");
    await createFlow(page, flow);

    // ① UI からアプリを作成（ワークフローを選ぶと画面の雛形が入る）。
    await page.goto("/apps");
    await page.getByRole("button", { name: "アプリを作成" }).first().click();
    const dialog = page.getByRole("dialog");
    const appName = uniqueName("e2e-app");
    await dialog.getByLabel(/名前/).fill(appName);
    await dialog.getByLabel(/画面の見出し/).fill("経費の申請");
    await dialog.getByLabel("追加するワークフロー").selectOption({ label: `${flow}（v1）` });
    await dialog.getByRole("button", { name: "追加" }).click();
    // 既定は「組み立てる」。記法は「書く」タブで見る。
    await dialog.getByRole("tab", { name: "書く" }).click();
    await expect(dialog.getByRole("textbox", { name: /^画面\*?$/ })).toHaveValue(new RegExp(`-> workflow ${flow}`));
    // プレビューで図になる（ボタンは保存前なので押せない）。
    await dialog.getByRole("tab", { name: "プレビュー" }).click();
    await expect(dialog.getByTestId("miniapp-preview").getByTestId("ui-figure")).toHaveAttribute(
      "data-ui-kind",
      "app",
    );
    await dialog.getByRole("button", { name: "作成" }).click();

    // 一覧から実行画面を開く。
    const card = page.locator("li", { has: page.getByRole("heading", { name: appName }) });
    await expect(card).toBeVisible({ timeout: 15_000 });
    await card.getByRole("link", { name: "開く" }).click();
    await page.waitForURL(/\/apps\/[0-9a-f-]+/i, { timeout: 15_000 });
    const appUrl = page.url();

    // ② 入力して起動 → 記録が成功まで進む（押した本人の権限で起動）。
    await apply(page, "1200");
    const runs = page.getByTestId("yui-runs");
    await expect(runs).toContainText(`申請する → ${flow}`);
    await expect(runs).toContainText("金額: 1,200円");
    await expect(runs.locator("li").first()).toHaveAttribute("data-status", "succeeded", {
      timeout: 60_000,
    });

    // ③ アプリ**本体だけ**を bob に共有する（ワークフローは共有しない）。
    await page.getByRole("button", { name: "共有" }).click();
    await page.getByPlaceholder("名前・メールで検索").fill("bob");
    const bobRow = page.getByRole("dialog").locator("li", { hasText: "bob" }).first();
    await bobRow.getByRole("button", { name: "共有" }).click();
    await expect(page.getByRole("dialog").getByText("共有中の相手")).toBeVisible();

    const bobContext = await browser.newContext();
    const bobPage = await bobContext.newPage();
    try {
      await loginAs(bobPage, "bob");
      await bobPage.goto(appUrl);
      // alice の記録（入力値）は見えない。
      await expect(bobPage.getByTestId("ui-figure")).toBeVisible({ timeout: 15_000 });
      await expect(bobPage.getByTestId("yui-runs")).toHaveCount(0);
      // bob も部品の個別共有なしで起動できる（IR はバンドル権限で読む）。
      await apply(bobPage, "800");
      await expect(bobPage.getByTestId("yui-runs").locator("li")).toHaveCount(1);
      await expect(bobPage.getByTestId("yui-runs")).toContainText("金額: 800円");
      // 編集は持ち主だけ。
      await expect(bobPage.getByRole("button", { name: "編集" })).toHaveCount(0);
    } finally {
      await bobContext.close();
    }
  });

  test("「組み立てる」で部品を足して保存し、足した部品で起動できる", async ({ page }) => {
    test.setTimeout(180_000);
    await loginViaKeycloak(page);
    const flow = uniqueName("build-flow");
    await createFlow(page, flow);

    await page.goto("/apps");
    await page.getByRole("button", { name: "アプリを作成" }).first().click();
    const dialog = page.getByRole("dialog");
    const appName = uniqueName("e2e-build");
    await dialog.getByLabel(/名前/).fill(appName);
    await dialog.getByLabel(/画面の見出し/).fill("備品の申請");
    await dialog.getByLabel("追加するワークフロー").selectOption({ label: `${flow}（v1）` });
    await dialog.getByRole("button", { name: "追加" }).click();

    // 雛形（金額・区分・申請する）が組み立てるに出る。パレットから「1 行の文字」を足し、ラベルを直す。
    const builder = dialog.getByTestId("screen-builder");
    const editor = builder.locator("[data-yui=app-editor]");
    await expect(editor.getByRole("button", { name: /^金額/ })).toBeVisible();
    await editor.getByRole("button", { name: "1 行の文字" }).first().click();
    const label = editor.locator("[data-yui-inspector] input[data-prop=label]");
    await label.fill("品名");
    await editor.locator("[data-yui-inspector] input[type=checkbox][data-prop=required]").check();
    await expect(editor.getByRole("button", { name: /^品名/ })).toBeVisible();

    // 書けない設定（ラベルの =）はその場でエラーになり、保存できない。
    await label.fill("品名=x");
    await expect(editor.getByText("ラベルに「=」は使えません").first()).toBeVisible();
    await expect(dialog.getByRole("button", { name: "作成" })).toBeDisabled();
    await label.fill("品名");
    await expect(dialog.getByRole("button", { name: "作成" })).toBeEnabled();

    // 書くタブには正規形のソースが入っている。
    await dialog.getByRole("tab", { name: "書く" }).click();
    await expect(dialog.getByRole("textbox", { name: /^画面\*?$/ })).toHaveValue(/品名 = text 必須/);
    await dialog.getByRole("button", { name: "作成" }).click();

    const card = page.locator("li", { has: page.getByRole("heading", { name: appName }) });
    await expect(card).toBeVisible({ timeout: 15_000 });
    await card.getByRole("link", { name: "開く" }).click();
    await page.waitForURL(/\/apps\/[0-9a-f-]+/i, { timeout: 15_000 });

    // 足した部品が画面に出て、それを入れて起動できる。
    const figure = page.getByTestId("ui-figure");
    await expect(figure.getByRole("button", { name: "申請する" })).toBeEnabled({ timeout: 15_000 });
    await figure.getByLabel("品名").fill("ノート PC");
    await figure.getByLabel("金額").fill("1200");
    await figure.getByRole("radio", { name: "交通費" }).click();
    await figure.getByRole("button", { name: "申請する" }).click();
    await figure.getByRole("group", { name: "実行の確認" }).getByRole("button", { name: "実行する" }).click();
    await expect(figure.getByText(/を起動しました/)).toBeVisible({ timeout: 30_000 });
    await expect(page.getByTestId("yui-runs")).toContainText("品名: ノート PC");
  });

  test("会話の ```ui app を「アプリとして保存」して、そのまま使える", async ({ page }) => {
    test.setTimeout(180_000);
    await loginViaKeycloak(page);
    const flow = uniqueName("saved-flow");
    await createFlow(page, flow);

    // 会話に送るボタンを含む app は保存できない（ボタンを出さない）。
    await page.goto("/");
    await page.getByLabel("メッセージを入力").fill(`yui:app ${flow}`);
    await page.getByRole("button", { name: "送信" }).click();
    await page.waitForURL(/\/c\/[0-9a-f-]+/i, { timeout: 30_000 });
    const mixed = page.getByTestId("ui-figure");
    await expect(mixed.getByRole("button", { name: "申請する" })).toBeEnabled({ timeout: 30_000 });
    await expect(mixed.getByRole("button", { name: "アプリとして保存" })).toHaveCount(0);

    // ワークフローのボタンだけの app は保存できる。
    await page.goto("/");
    await page.getByLabel("メッセージを入力").fill(`yui:form ${flow}`);
    await page.getByRole("button", { name: "送信" }).click();
    await page.waitForURL(/\/c\/[0-9a-f-]+/i, { timeout: 30_000 });
    const figure = page.getByTestId("ui-figure");
    const save = figure.getByRole("button", { name: "アプリとして保存" });
    await expect(save).toBeVisible({ timeout: 30_000 });
    await save.click();
    const dialog = page.getByRole("dialog");
    await expect(dialog.getByLabel(/画面の見出し/)).toHaveValue("経費の申請");
    const appName = uniqueName("saved-app");
    await dialog.getByLabel(/名前/).fill(appName);
    await dialog.getByRole("button", { name: "保存" }).click();

    // 保存したアプリの画面へ移り、同じ図のボタンで起動できる。
    await page.waitForURL(/\/apps\/[0-9a-f-]+/i, { timeout: 30_000 });
    await apply(page, "500");
    await expect(page.getByTestId("yui-runs").locator("li").first()).toHaveAttribute(
      "data-status",
      "succeeded",
      { timeout: 60_000 },
    );
  });
});
