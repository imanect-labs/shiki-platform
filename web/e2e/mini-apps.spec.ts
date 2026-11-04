import { test, expect, type Page } from "@playwright/test";

import {
  cleanupQueue,
  deleteArtifactApi,
  deleteTableApi,
  loginAs,
  loginViaKeycloak,
  removeWorkflowApi,
  trackCreatedResources,
  uniqueName,
} from "./helpers";

/// テストで作ったもの（アプリ・ワークフロー・スキル・テーブル・ビュー）を、テストの後に片付ける（失敗したテストでも
/// 走る・BUG-126）。画面から作ったアプリ（作成・会話の「アプリとして保存」）は画面の応答から拾って積む。
const cleanup = cleanupQueue();
test.beforeEach(({ page }) => trackCreatedResources(page, cleanup));
test.afterEach(() => cleanup.run());

/// API の作成の経路と、作ったものの片付け方（`apiPost` が作ったものを積む）。
function removerFor(page: Page, path: string): ((id: string) => Promise<void>) | null {
  if (path === "/workflows") return (id) => removeWorkflowApi(page, id);
  if (path === "/mini-apps" || path === "/skills" || path === "/data/views") return (id) => deleteArtifactApi(page, id);
  if (path === "/data/tables") return (id) => deleteTableApi(page, id);
  return null;
}

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
  const created = (await res.json()) as { id: string };
  const remove = removerFor(page, path);
  if (remove && created.id) cleanup.add(() => remove(created.id));
  return created;
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

    // ③ アプリ**本体だけ**を bob に共有する（ワークフローは共有しない）。まだ公開していないことを共有の前に言う。
    await page.getByRole("button", { name: "共有" }).click();
    await expect(page.getByRole("dialog").getByTestId("share-dialog-note")).toContainText("まだ公開していません");
    await page.getByPlaceholder("名前・メールで検索").fill("bob");
    const bobRow = page.getByRole("dialog").locator("li", { hasText: "bob" }).first();
    await bobRow.getByRole("button", { name: "共有" }).click();
    await expect(page.getByRole("dialog").getByText("共有中の相手")).toBeVisible();
    await page.getByRole("dialog").getByRole("button", { name: "閉じる" }).click();
    // 共有した人が使えるのは公開したバージョンだけ（D-16）。公開の帯から v1 を公開する。
    await page.getByTestId("miniapp-publish-bar").getByRole("button", { name: "公開", exact: true }).click();
    const confirm = page.getByTestId("miniapp-publish-dialog");
    await expect(confirm.getByTestId("miniapp-publish-changes")).toBeVisible({ timeout: 15_000 });
    await confirm.getByRole("button", { name: "公開", exact: true }).click();
    await expect(page.getByTestId("miniapp-publish-state")).toHaveText("公開中", { timeout: 15_000 });

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
    // プレビューへ移っても保存できないまま。戻るとエラーの段がそのまま残っている。
    await dialog.getByRole("tab", { name: "プレビュー" }).click();
    await expect(dialog.getByRole("button", { name: "作成" })).toBeDisabled();
    await dialog.getByRole("tab", { name: "組み立てる" }).click();
    await expect(editor.getByText("ラベルに「=」は使えません").first()).toBeVisible();
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

  test("「組み立てる」の実行先の入力の候補は、ワークフローの入力項目の並び（x-order）の順に出る（BUG-34・D-60）", async ({
    page,
  }) => {
    test.setTimeout(120_000);
    await loginViaKeycloak(page);
    const flow = uniqueName("order-flow");
    // キーの辞書順（due・memo・title）と違う順を x-order に書く。properties は保存で辞書順に並び替わる。
    const wf = await apiPost(page, "/workflows", {
      ir: {
        ir_version: 1,
        name: flow,
        display_name: flow,
        triggers: [{ kind: "interactive" }],
        input_schema: {
          type: "object",
          properties: {
            due: { type: "string", format: "date", title: "期日" },
            memo: { type: "string", title: "メモ" },
            title: { type: "string", title: "件名" },
          },
          required: ["title"],
          "x-order": ["title", "memo", "due"],
        },
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

    // 途中で落ちても作ったワークフローを残さない（BUG-63・66）。
    try {
      await page.goto("/apps");
      await page.getByRole("button", { name: "アプリを作成" }).first().click();
      const dialog = page.getByRole("dialog");
      await dialog.getByLabel("追加するワークフロー").selectOption({ label: `${flow}（v1）` });
      await dialog.getByRole("button", { name: "追加" }).click();
      // 雛形（金額・区分・申請する）の機能を選んだまま、まだ部品の無い入力が x-order の順に並ぶ。
      const editor = dialog.getByTestId("screen-builder").locator("[data-yui=app-editor]");
      const hints = editor
        .locator(".ye-pal-group")
        .filter({ has: page.locator(".ye-pal-title", { hasText: "実行先の入力（まだ無いもの）" }) })
        .locator(".ye-pal-item");
      await expect(hints).toHaveCount(3);
      await expect(hints).toHaveText([/^件名送る名前 title・必須/, /^メモ送る名前 memo/, /^期日送る名前 due/]);
      await dialog.getByRole("button", { name: "キャンセル" }).click();
      await expect(dialog).toHaveCount(0);
    } finally {
      await cleanupArtifacts(page, [wf.id]);
    }
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

/// 画面の図（金額・区分・申請する）。`-> workflow` の名前はミニアプリに束ねる別名。
function expenseScreen(flow: string): string {
  return (
    "# 経費の申請\n\n```ui\napp 経費の申請\n金額 = number 必須 円 as amount\n" +
    `区分 = choice 交通費 | 交際費 as category\nbutton 申請する -> workflow ${flow}\n\`\`\`\n`
  );
}

/// ワークフロー（IR）と、それだけを束ねたミニアプリを API で作り、実行画面を開く。
async function openAppWith(page: Page, flow: string, nodes: unknown[], edges: unknown[], scopes: string[] = []) {
  const wf = await apiPost(page, "/workflows", {
    ir: {
      ir_version: 1,
      name: flow,
      display_name: flow,
      declared_scopes: scopes,
      triggers: [{ kind: "interactive" }],
      nodes,
      edges,
    },
  });
  const app = await apiPost(page, "/mini-apps", {
    name: uniqueName("reply-app"),
    body: {
      description: "経費の申請",
      screen: expenseScreen(flow),
      workflows: [{ alias: flow, artifact_id: wf.id, version: 1 }],
    },
  });
  await page.goto(`/apps/${app.id}`);
  return app.id;
}

/// 結果を画面に返す（workflow.reply・GAP-09）。押した本人の画面の起動の記録の下に、ワークフローが返した
/// 結果（文章・表・Markdown）・失敗の理由・待機中が出る。
test.describe("ミニアプリ（画面に返す結果）", () => {
  test.describe("スマホ幅", () => {
    test.use({ viewport: { width: 390, height: 844 }, hasTouch: true, isMobile: true });

    test("ボタンで起動した run の結果が画面に出て、結果の中のボタンは押せない", async ({ page }) => {
      test.setTimeout(180_000);
      await loginViaKeycloak(page);
      const flow = uniqueName("reply-flow");
      await openAppWith(
        page,
        flow,
        [
          {
            id: "accepted",
            type: "workflow.reply",
            label: "受付",
            params: {
              format: "text",
              label: "受付",
              tone: "success",
              value: {
                $template: "{category} の申請を受け付けました（{amount} 円）",
                vars: {
                  category: { $from: "input", path: "/category" },
                  amount: { $from: "input", path: "/amount" },
                },
              },
            },
          },
          {
            id: "summary",
            type: "workflow.reply",
            label: "申請の内容",
            params: {
              format: "table",
              label: "申請の内容",
              value: [
                { 項目: "区分", 値: { $from: "input", path: "/category" } },
                { 項目: "金額", 値: { $from: "input", path: "/amount" } },
              ],
              columns: [
                { label: "項目", path: "/項目" },
                { label: "値", path: "/値" },
              ],
            },
          },
          {
            id: "note",
            type: "workflow.reply",
            label: "注意",
            params: {
              format: "markdown",
              value: "**次の手順**\n\n```ui\napp 偽の承認\nbutton 承認する -> workflow 他人のフロー\n```",
            },
          },
        ],
        [
          { from: "accepted", to: "summary" },
          { from: "summary", to: "note" },
        ],
      );

      await apply(page, "1200");
      const runs = page.getByTestId("yui-runs");
      await expect(runs.locator("li").first()).toHaveAttribute("data-status", "succeeded", { timeout: 60_000 });
      // 新しい 1 件は結果を開いた状態で出る（実行順: 文章 → 表 → Markdown）。
      const result = runs.getByTestId("yui-run-result");
      await expect(result).toBeVisible();
      await expect(result).toContainText("交通費 の申請を受け付けました（1200 円）");
      await expect(result.getByTestId("yui-run-section")).toHaveCount(3);
      const table = result.getByRole("table");
      await expect(table).toBeVisible();
      await expect(table.getByRole("row", { name: /金額\s*1200/ })).toBeVisible();
      // 結果の中の操作を持つ図はボタンにならない（記述のまま見せる）。
      await expect(result.getByText("次の手順")).toBeVisible();
      await expect(result.getByRole("button", { name: "承認する" })).toHaveCount(0);
      // 閉じて開ける。
      const toggle = runs.getByRole("button", { name: "結果" });
      await expect(toggle).toHaveAttribute("aria-expanded", "true");
      await toggle.click();
      await expect(result).toHaveCount(0);
      await toggle.click();
      await expect(runs.getByTestId("yui-run-result")).toBeVisible();
      // 横にはみ出さない（表は箱の中だけで横に動く）。
      const scrollWidth = await page.evaluate(() => document.documentElement.scrollWidth);
      expect(scrollWidth).toBeLessThanOrEqual(390);
    });
  });

  test("失敗した run は利用者向けの理由と参照番号を出し、先に返した成功の結果に注記する", async ({ page }) => {
    test.setTimeout(180_000);
    await loginViaKeycloak(page);
    const flow = uniqueName("reply-fail");
    await openAppWith(
      page,
      flow,
      [
        {
          id: "accepted",
          type: "workflow.reply",
          params: { format: "text", tone: "success", value: "受け付けました" },
        },
        // 存在しないファイルを読む（実行主体の権限で読めず失敗する）。ファイルを文字の組み立てで決めるので、押す前の
        // 権限の確かめ（GAP-37・書き先が決まっているボタンは押す前に止める）は掛からず、押した後に失敗する。
        {
          id: "read",
          type: "storage.read",
          params: { file: { $template: "00000000-0000-4000-8000-000000000000" } },
        },
      ],
      [{ from: "accepted", to: "read" }],
      ["storage.read"],
    );
    await apply(page, "800");
    const runs = page.getByTestId("yui-runs");
    await expect(runs.locator("li").first()).toHaveAttribute("data-status", "failed", { timeout: 60_000 });
    const failure = runs.getByTestId("yui-run-failure");
    await expect(failure).toBeVisible();
    await expect(failure).toContainText(/権限が足りない|見つかりませんでした/);
    await expect(failure).toContainText(/参照番号: [0-9a-f]{8}/);
    await expect(runs.getByTestId("yui-run-result")).toContainText("この後の処理で失敗しました");
  });

  test("待機中の run は「待機中」と出し、3 秒ごとに読み直し続けない（BUG-20）", async ({ page }) => {
    test.setTimeout(180_000);
    await loginViaKeycloak(page);
    const flow = uniqueName("reply-wait");
    const appId = await openAppWith(
      page,
      flow,
      [
        {
          id: "accepted",
          type: "workflow.reply",
          params: { format: "text", value: "受け付けました。1 時間後に続きを進めます" },
        },
        { id: "later", type: "control.wait", params: { kind: "duration", duration_sec: 3600 } },
      ],
      [{ from: "accepted", to: "later" }],
    );
    await apply(page, "500");
    const runs = page.getByTestId("yui-runs");
    await expect(runs.locator("li").first()).toHaveAttribute("data-status", "waiting", { timeout: 60_000 });
    await expect(runs).toContainText("待機中");
    await expect(runs.getByTestId("yui-run-result")).toContainText("受け付けました。1 時間後に続きを進めます");
    await expect(runs.getByTestId("yui-run-result")).toContainText("決めた時間まで待っています");
    // 待機中だけになったら間隔を空ける（10 秒の間に読み直しは高々 1 回）。
    let reads = 0;
    page.on("request", (req) => {
      if (req.method() === "GET" && req.url().includes(`/api/mini-apps/${appId}/runs`)) reads += 1;
    });
    await page.waitForTimeout(10_000);
    expect(reads).toBeLessThanOrEqual(1);
  });

  test("古い記録の結果は開いたときに 1 回だけ読み、記録を読み直しても取り直さない（GAP-09）", async ({ page }) => {
    test.setTimeout(240_000);
    await loginViaKeycloak(page);
    const flow = uniqueName("reply-old");
    const appId = await openAppWith(
      page,
      flow,
      [
        {
          id: "accepted",
          type: "workflow.reply",
          params: {
            format: "text",
            value: { $template: "{amount} 円で受け付けました", vars: { amount: { $from: "input", path: "/amount" } } },
          },
        },
      ],
      [],
    );
    const reads = new Map<string, number>();
    const one = new RegExp(`/api/mini-apps/${appId}/runs/([0-9a-f-]{36})`);
    page.on("request", (req) => {
      const m = one.exec(req.url());
      if (req.method() === "GET" && m) reads.set(m[1], (reads.get(m[1]) ?? 0) + 1);
    });
    const runs = page.getByTestId("yui-runs");
    // 新しいものが下。2 回押すと、上の 1 件（最初の起動）は結果を付けずに返る古い記録になる。
    await apply(page, "100");
    await expect(runs.locator("li").last()).toHaveAttribute("data-status", "succeeded", { timeout: 60_000 });
    await apply(page, "200");
    await expect(runs.locator("li")).toHaveCount(2);
    await expect(runs.locator("li").last()).toHaveAttribute("data-status", "succeeded", { timeout: 60_000 });
    const old = runs.locator("li").first();
    const oldId = (await old.getAttribute("data-run-id")) ?? "";
    expect(oldId).not.toBe("");
    await old.getByRole("button", { name: "結果" }).click();
    await expect(old.getByTestId("yui-run-result")).toContainText("100 円で受け付けました");
    expect(reads.get(oldId)).toBe(1);

    // もう 1 回押して記録を何度か読み直させても、開いている古い行の結果は取り直さない。
    await apply(page, "300");
    await expect(runs.locator("li")).toHaveCount(3);
    await expect(runs.locator("li").last()).toHaveAttribute("data-status", "succeeded", { timeout: 60_000 });
    await expect(runs.locator("li").last().getByTestId("yui-run-result")).toContainText("300 円で受け付けました");
    await expect(old.getByTestId("yui-run-result")).toContainText("100 円で受け付けました");
    expect(reads.get(oldId)).toBe(1);
  });

  test("古い記録の結果は run の状態が変わったら取り直し、前に読んだ失敗を出し続けない（GAP-09）", async ({ page }) => {
    test.setTimeout(240_000);
    await loginViaKeycloak(page);
    const flow = uniqueName("reply-resume");
    const appId = await openAppWith(
      page,
      flow,
      [
        {
          id: "accepted",
          type: "workflow.reply",
          params: {
            format: "text",
            value: { $template: "{amount} 円で受け付けました", vars: { amount: { $from: "input", path: "/amount" } } },
          },
        },
      ],
      [],
    );
    const runs = page.getByTestId("yui-runs");
    await apply(page, "100");
    await expect(runs.locator("li").last()).toHaveAttribute("data-status", "succeeded", { timeout: 60_000 });
    const old = runs.locator("li").first();
    const oldId = (await old.getAttribute("data-run-id")) ?? "";
    expect(oldId).not.toBe("");

    // 古い行の run が、いったん失敗していた（あとで再開して成功する・run_id は変わらない）形を、記録の一覧と
    // 1 件の読み込みの両方に作る。
    const failure = { message: "一時的に失敗しました（e2e）", reference: "e2e00001" };
    let failed = true;
    let reads = 0;
    await page.route(
      (url) => url.pathname === `/api/mini-apps/${appId}/runs`,
      async (route) => {
        const res = await route.fetch();
        const body = (await res.json()) as { runs: Record<string, unknown>[] };
        for (const r of body.runs) {
          if (failed && r.run_id === oldId) Object.assign(r, { status: "failed", failure });
        }
        await route.fulfill({ response: res, json: body });
      },
    );
    await page.route(
      (url) => url.pathname === `/api/mini-apps/${appId}/runs/${oldId}`,
      async (route) => {
        reads += 1;
        const res = await route.fetch();
        const body = (await res.json()) as Record<string, unknown>;
        if (failed) Object.assign(body, { status: "failed", failure });
        await route.fulfill({ response: res, json: body });
      },
    );
    // もう 1 回押すと古い行は結果を付けずに返る（失敗の形）。開くと 1 回だけ読み、失敗と結果を出す。
    await apply(page, "200");
    await expect(runs.locator("li")).toHaveCount(2);
    await expect(runs.locator("li").last()).toHaveAttribute("data-status", "succeeded", { timeout: 60_000 });
    await expect(old).toHaveAttribute("data-status", "failed");
    await old.getByRole("button", { name: "結果" }).click();
    await expect(old.getByTestId("yui-run-failure")).toContainText("一時的に失敗しました（e2e）");
    await expect(old.getByTestId("yui-run-result")).toContainText("100 円で受け付けました");
    expect(reads).toBe(1);

    // 再開して成功した後に記録が読み直されると、状態が変わった古い行の結果を 1 回だけ取り直し、前の失敗は消える。
    failed = false;
    await apply(page, "300");
    await expect(runs.locator("li")).toHaveCount(3);
    await expect(old).toHaveAttribute("data-status", "succeeded", { timeout: 60_000 });
    await expect(old.getByTestId("yui-run-failure")).toHaveCount(0);
    await expect(old.getByTestId("yui-run-result")).toContainText("100 円で受け付けました");
    await expect.poll(() => reads).toBe(2);
  });

  test("ボタンで行を足す短いフローが終わると、ページを読み直さずに画面の表に行が出る（BUG-19）", async ({ page }) => {
    test.setTimeout(180_000);
    await loginViaKeycloak(page);
    const table = (await apiPost(page, "/data/tables", {
      name: uniqueName("e2e-申請台帳"),
      schema: { fields: [{ key: "title", display_name: "件名", type: "text" }] },
    })) as { id: string; schema?: { fields: { key: string; display_name: string }[] } };
    const titleKey = table.schema?.fields.find((f) => f.display_name === "件名")?.key ?? "title";
    const view = (await apiPost(page, "/data/views", {
      name: uniqueName("e2e-申請の一覧"),
      table_id: table.id,
      query: {},
    })) as { id: string; version: number };
    // 行を 1 つ足すだけのフロー（押した直後の最初の一覧で、もう終わっていることがある）。
    const flow = uniqueName("grid-flow");
    const wf = await apiPost(page, "/workflows", {
      ir: {
        ir_version: 1,
        name: flow,
        display_name: flow,
        declared_scopes: ["data.write"],
        triggers: [{ kind: "interactive" }],
        nodes: [
          {
            id: "add",
            type: "data.record.create",
            label: "台帳に足す",
            params: { table: table.id, data: { [titleKey]: { $from: "input", path: "/category" } } },
          },
        ],
        edges: [],
      },
    });
    const app = await apiPost(page, "/mini-apps", {
      name: uniqueName("grid-app"),
      body: {
        description: "経費の申請",
        screen: `${expenseScreen(flow)}\n\`\`\`data_grid\nrequests\n\`\`\`\n`,
        workflows: [{ alias: flow, artifact_id: wf.id, version: 1 }],
        data_views: [{ alias: "requests", artifact_id: view.id, version: view.version }],
      },
    });
    // 押した直後の最初の起動の一覧を、run が終わるまで止める（最初に見える起動を必ず「もう終わった」状態にする。
    // 止めないと、ワーカーが拾う前の queued が先に見えることが多く、「動いていた → 終わった」の経路でも通ってしまう）。
    // 状態は page.request（page.route に掛からない）で確かめる。
    let pressed = false;
    let held = false;
    page.on("request", (req) => {
      if (req.method() === "POST" && req.url().includes(`/api/mini-apps/${app.id}/ui-actions`)) pressed = true;
    });
    await page.route(
      (url) => url.pathname === `/api/mini-apps/${app.id}/runs`,
      async (route) => {
        if (!pressed || held || route.request().method() !== "GET") return route.continue();
        held = true;
        await expect
          .poll(
            async () => {
              const list = (await (await page.request.get(`/api/mini-apps/${app.id}/runs`)).json()) as {
                runs: { status: string }[];
              };
              return list.runs.map((r) => r.status).join(",");
            },
            { timeout: 60_000, intervals: [500] },
          )
          .toBe("succeeded");
        await route.continue();
      },
    );
    await page.goto(`/apps/${app.id}`);
    const grid = page.getByTestId("miniapp-data-grid-requests");
    await expect(grid.getByTestId("miniapp-data-grid-empty")).toBeVisible({ timeout: 30_000 });

    await apply(page, "900");
    const runs = page.getByTestId("yui-runs");
    await expect(runs.locator("li").first()).toHaveAttribute("data-status", "succeeded", { timeout: 60_000 });
    expect(held).toBe(true);
    // 読み直さずに、フローが足した行が表に出る。
    await expect(grid.getByTestId("miniapp-data-grid-empty")).toHaveCount(0, { timeout: 30_000 });
    await expect(grid.locator("figcaption")).toContainText("1 件");
  });

  test("結果の Markdown のリンクは、自分のオリジンだけを内部として扱い、プロトコル相対の URL にもホスト名を添える", async ({
    page,
  }) => {
    test.setTimeout(180_000);
    await loginViaKeycloak(page);
    const flow = uniqueName("reply-links");
    await openAppWith(
      page,
      flow,
      [
        {
          id: "links",
          type: "workflow.reply",
          params: {
            format: "markdown",
            value:
              "[社内の承認](/approvals) ・ [社内ポータルで再認証](//login.evil.example/) ・ " +
              "[外部の資料](https://example.com/doc)",
          },
        },
      ],
      [],
    );
    await apply(page, "100");
    const result = page.getByTestId("yui-runs").getByTestId("yui-run-result");
    await expect(result).toContainText("社内の承認", { timeout: 60_000 });
    // 自分のオリジンへのリンクは注記なしで同じタブ。
    const internal = result.getByRole("link", { name: "社内の承認" });
    await expect(internal).not.toHaveAttribute("target", "_blank");
    expect(await internal.evaluate((a) => (a as HTMLAnchorElement).origin === location.origin)).toBe(true);
    // 「/」で始まっても別オリジンになるプロトコル相対の URL は外部: ホスト名を添えて別タブ。
    await expect(result.getByRole("link", { name: "社内ポータルで再認証" })).toHaveAttribute("target", "_blank");
    await expect(result).toContainText("社内ポータルで再認証（login.evil.example）");
    await expect(result).toContainText("外部の資料（example.com）");
    await expect(result).not.toContainText("社内の承認（");
  });
});

/// ログイン済みページのセッションで backend API を叩く（PUT・DELETE 用・CSRF 二重送信つき）。
async function apiSend(page: Page, method: "PUT" | "DELETE", path: string, body?: unknown) {
  const cookies = await page.context().cookies();
  const csrf = cookies.find((c) => c.name === "shiki_csrf")?.value ?? "";
  const res = await page.request.fetch(`/api${path}`, {
    method,
    headers: { "Content-Type": "application/json", "X-CSRF-Token": csrf },
    ...(body === undefined ? {} : { data: body }),
  });
  expect(res.ok(), `${method} ${path} → ${res.status()}`).toBeTruthy();
}

/// テストで作ったアーティファクトを片付ける（もう消えているものは飛ばす）。
async function cleanupArtifacts(page: Page, ids: string[]) {
  const cookies = await page.context().cookies();
  const csrf = cookies.find((c) => c.name === "shiki_csrf")?.value ?? "";
  for (const id of ids) {
    const res = await page.request.delete(`/api/artifacts/${id}`, { headers: { "X-CSRF-Token": csrf } });
    expect([204, 404], `DELETE /artifacts/${id} → ${res.status()}`).toContain(res.status());
  }
}

/// 束ねた部品が壊れたとき（BUG-17）と、共有で編集にした人の編集（BUG-18）。
test.describe("ミニアプリ（部品のピン・共同編集）", () => {
  test("束ねたスキルが削除されても、アプリは開けてスキルだけが使えないと出し、エディタで外せる（BUG-17）", async ({
    page,
  }) => {
    test.setTimeout(180_000);
    await loginViaKeycloak(page);
    const flow = uniqueName("skill-flow");
    const wf = await apiPost(page, "/workflows", {
      ir: {
        ir_version: 1,
        name: flow,
        display_name: flow,
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
    const skill = await apiPost(page, "/skills", {
      name: uniqueName("e2e-skill"),
      body: { description: "経費規程の案内", instructions: "経費規程に沿って答える。" },
    });
    const app = await apiPost(page, "/mini-apps", {
      name: uniqueName("skill-app"),
      body: {
        description: "経費の申請",
        screen: expenseScreen(flow),
        skill: { artifact_id: skill.id, version: 1 },
        workflows: [{ alias: flow, artifact_id: wf.id, version: 1 }],
      },
    });
    try {
      await page.goto(`/apps/${app.id}`);
      await expect(page.getByTestId("ui-figure")).toBeVisible({ timeout: 15_000 });
      await expect(page.getByText("・スキル適用")).toBeVisible();
      await expect(page.getByTestId("miniapp-skill-unavailable")).toHaveCount(0);

      // スキルを消しても「見つかりません」にならず、スキルだけが使えないと理由つきで出る。
      await apiSend(page, "DELETE", `/artifacts/${skill.id}`);
      await page.reload();
      await expect(page.getByTestId("ui-figure")).toBeVisible({ timeout: 15_000 });
      await expect(page.getByText("アプリが見つかりません")).toHaveCount(0);
      const notice = page.getByTestId("miniapp-skill-unavailable");
      await expect(notice).toContainText("スキル（v1）が見つかりません");
      await expect(notice).toContainText("画面とボタンはそのまま使えます");
      await expect(page.getByText("・スキルは使えません")).toBeVisible();
      // 「このアプリについて AI に聞く」は押せず、理由をボタンの近くに出す（D-16 の (b)）。
      await expect(page.getByTestId("miniapp-ask-ai")).toBeDisabled();
      await expect(page.getByText("スキルを使えないため、AI に聞けません。")).toBeVisible();
      // 画面のボタンはそのまま使える。
      await apply(page, "300");
      await expect(page.getByTestId("yui-runs").locator("li").first()).toHaveAttribute("data-status", "succeeded", {
        timeout: 60_000,
      });

      // エディタは読めないスキルを選択欄に残して見せ、「なし」にすれば保存できる。
      await page.getByRole("button", { name: "編集" }).click();
      const dialog = page.getByRole("dialog");
      const skillSelect = dialog.getByLabel("スキル（任意）");
      await expect(skillSelect.locator("option:checked")).toHaveText("読めないスキル（v1）");
      // 変えなければそのまま保存できる（保存で確かめるのは足した部品・版を変えた部品だけ・BUG-80）。
      const unreadable = "束ねているスキルは、削除されたか、あなたに見る権限がありません。変えなければ、このまま保存できます。";
      await expect(dialog.getByText(unreadable)).toBeVisible();
      await skillSelect.selectOption({ label: "なし" });
      await expect(dialog.getByText(unreadable)).toHaveCount(0);
      await dialog.getByRole("button", { name: "保存" }).click();
      await expect(page.getByText("v2 を下書きとして保存しました", { exact: true })).toBeVisible({ timeout: 15_000 });
      await expect(page.getByTestId("miniapp-skill-unavailable")).toHaveCount(0);
      await expect(page.getByText("・スキルは使えません")).toHaveCount(0);
      // スキルを外したアプリには「AI に聞く」を出さない。
      await expect(page.getByTestId("miniapp-ask-ai")).toHaveCount(0);
    } finally {
      // テストで作ったアプリ・ワークフロー・スキルを片付ける（スキルは途中で消していることがある）。
      await cleanupArtifacts(page, [app.id, wf.id, skill.id]);
    }
  });

  test("束ねたスキルがスキルの一覧の先頭のページに無くても、エディタは読めないと決めつけずにピンを残す（BUG-17）", async ({
    page,
  }) => {
    test.setTimeout(120_000);
    await loginViaKeycloak(page);
    const skillName = uniqueName("e2e-skill-outside");
    const skill = await apiPost(page, "/skills", {
      name: skillName,
      body: { description: "経費規程の案内", instructions: "経費規程に沿って答える。" },
    });
    const app = await apiPost(page, "/mini-apps", {
      name: uniqueName("outside-app"),
      body: {
        description: "経費の案内",
        screen: "経費の申請の案内です。\n",
        skill: { artifact_id: skill.id, version: 1 },
        workflows: [],
      },
    });
    // スキルの一覧（GET /artifacts の先頭のページ）に束ねたスキルが入らない状況を作る（実際に 51 件は作らない）。
    const skillList = /\/api\/artifacts\?kind=skill$/;
    try {
      await page.route(skillList, (route) => route.fulfill({ json: { items: [] } }));
      await page.goto(`/apps/${app.id}`);
      await expect(page.getByRole("heading", { name: "経費の案内", level: 1 })).toBeVisible({ timeout: 15_000 });
      await expect(page.getByTestId("miniapp-skill-unavailable")).toHaveCount(0);
      await page.getByRole("button", { name: "編集" }).click();
      const dialog = page.getByRole("dialog");
      const skillSelect = dialog.getByLabel("スキル（任意）");
      // ピンの版を直に読めたので、名前つきで選んだまま。「読めない」とは言わない。
      await expect(skillSelect.locator("option:checked")).toHaveText(`${skillName}（v1）`);
      await expect(dialog.getByText("束ねているスキルは、削除されたか、あなたに見る権限がありません")).toHaveCount(0);
      await dialog.getByLabel(/画面の見出し/).fill("経費の案内（改訂）");
      await dialog.getByRole("button", { name: "保存" }).click();
      await expect(page.getByText("v2 を下書きとして保存しました", { exact: true })).toBeVisible({ timeout: 15_000 });
      // 保存した版もスキルのピンをそのまま持つ。
      const resolved = (await (await page.request.get(`/api/mini-apps/${app.id}/resolved`)).json()) as {
        version: number;
        body: { skill: { artifact_id: string; version: number } | null };
      };
      expect(resolved.version).toBe(2);
      expect(resolved.body.skill).toEqual({ artifact_id: skill.id, version: 1 });
    } finally {
      await page.unroute(skillList);
      await cleanupArtifacts(page, [app.id, skill.id]);
    }
  });

  test("共有で「編集」にした人は編集ボタンから新しい版を保存できる（BUG-18）", async ({ page, browser }) => {
    test.setTimeout(180_000);
    await loginViaKeycloak(page);
    const app = await apiPost(page, "/mini-apps", {
      name: uniqueName("editor-app"),
      body: { description: "備品の案内", screen: "備品の置き場所と借り方の案内です。\n", workflows: [] },
    });
    const bobContext = await browser.newContext();
    const bobPage = await bobContext.newPage();
    try {
      await loginAs(bobPage, "bob");
      const bob = (await (await bobPage.request.get("/api/me")).json()) as { id: string };
      await apiSend(page, "PUT", `/artifacts/${app.id}/shares`, { target: { type: "user", id: bob.id }, role: "editor" });

      await bobPage.goto(`/apps/${app.id}`);
      await expect(bobPage.getByRole("heading", { name: "備品の案内", level: 1 })).toBeVisible({ timeout: 15_000 });
      // 閲覧の共有では出ない編集ボタンが、編集の共有では出る。
      const edit = bobPage.getByRole("button", { name: "編集" });
      await expect(edit).toBeVisible({ timeout: 15_000 });
      await edit.click();
      const dialog = bobPage.getByRole("dialog");
      await dialog.getByLabel(/画面の見出し/).fill("備品の案内（改訂）");
      await dialog.getByRole("button", { name: "保存" }).click();
      await expect(bobPage.getByText("v2 を下書きとして保存しました", { exact: true })).toBeVisible({ timeout: 15_000 });
      await expect(bobPage.getByRole("heading", { name: "備品の案内（改訂）", level: 1 })).toBeVisible();

      // 持ち主の画面にも bob が保存した版が出る。
      await page.goto(`/apps/${app.id}`);
      await expect(page.getByRole("heading", { name: "備品の案内（改訂）", level: 1 })).toBeVisible({ timeout: 15_000 });
      await expect(page.getByText("v2", { exact: true }).first()).toBeVisible();
    } finally {
      await cleanupArtifacts(page, [app.id]);
      await bobContext.close().catch(() => undefined);
    }
  });
});
