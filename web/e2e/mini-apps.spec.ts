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
        // 存在しないファイルを読む（実行主体の権限で読めず失敗する）。
        { id: "read", type: "storage.read", params: { file: "00000000-0000-4000-8000-000000000000" } },
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
    await page.goto(`/apps/${app.id}`);
    const grid = page.getByTestId("miniapp-data-grid-requests");
    await expect(grid.getByTestId("miniapp-data-grid-empty")).toBeVisible({ timeout: 30_000 });

    await apply(page, "900");
    const runs = page.getByTestId("yui-runs");
    await expect(runs.locator("li").first()).toHaveAttribute("data-status", "succeeded", { timeout: 60_000 });
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
