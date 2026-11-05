import { test, expect, type Browser, type Page } from "@playwright/test";

import {
  cleanupAll,
  cleanupQueue,
  deleteArtifactApi,
  deleteTableApi,
  loginAs,
  loginViaKeycloak,
  removeWorkflowApi,
  uniqueName,
} from "./helpers";

/// 使う人の権限の確かめ（GAP-37）の E2E。
/// - 作る人（持ち主）は、共有の画面で、アプリの部品が要る権限と、共有している相手ごとに足りない権限と直し方を見る。
///   テーブルを共有すると、次に開いたときから足りると言う。
/// - 使う人は、アプリを開いたとき、足りない権限のあるボタンと表が押す前に止まり、理由を読める。共有されると使える。
/// - 公開の確認は、共有している相手に、公開するバージョンの部品に要る権限が足りなければ知らせる（公開は止めない）。
/// - ワークフローの共有の画面も、手動で実行するのに要る権限を相手ごとに言う。
/// PC（1440px）とスマホ（390px）で確かめる。作ったもの（アプリ・ワークフロー・ビュー・テーブル）は最後に消す。
/// `E2E_SHOTS_DIR` を渡すとスクリーンショットを撮る。

const SHOTS = process.env.E2E_SHOTS_DIR;

const cleanup = cleanupQueue();
test.afterEach(() => cleanup.run());

async function csrf(page: Page): Promise<string> {
  return (await page.context().cookies()).find((c) => c.name === "shiki_csrf")?.value ?? "";
}

async function api(page: Page, method: "POST" | "PUT", path: string, body: unknown): Promise<{ id: string; version?: number }> {
  const res = await page.request.fetch(`/api${path}`, {
    method,
    headers: { "Content-Type": "application/json", "X-CSRF-Token": await csrf(page) },
    data: body,
  });
  expect(res.ok(), `${method} ${path} → ${res.status()} ${await res.text()}`).toBeTruthy();
  return res.status() === 204 ? { id: "" } : ((await res.json()) as { id: string; version?: number });
}

async function createTable(page: Page, name: string): Promise<{ id: string; titleKey: string }> {
  const t = (await api(page, "POST", "/data/tables", {
    name,
    schema: { fields: [{ key: "title", display_name: "件名", type: "text" }] },
  })) as { id: string; schema?: { fields: { key: string; display_name: string }[] } };
  cleanup.add(() => deleteTableApi(page, t.id));
  return { id: t.id, titleKey: t.schema?.fields.find((f) => f.display_name === "件名")?.key ?? "title" };
}

/// 件名を受け取ってテーブルにレコードを追加するだけのワークフロー（最後に消す）。
async function createAddFlow(page: Page, name: string, table: { id: string; titleKey: string }): Promise<string> {
  const wf = await api(page, "POST", "/workflows", {
    ir: {
      ir_version: 1,
      name,
      display_name: name,
      declared_scopes: ["data.write"],
      triggers: [{ kind: "interactive" }],
      nodes: [
        {
          id: "add",
          type: "data.record.create",
          label: "台帳に足す",
          params: { table: table.id, data: { [table.titleKey]: { $from: "input", path: "/title" } } },
        },
      ],
      edges: [],
    },
  });
  cleanup.add(() => removeWorkflowApi(page, wf.id));
  return wf.id;
}

async function shareTable(page: Page, tableId: string, userId: string, role: "viewer" | "submitter") {
  await api(page, "PUT", `/data/tables/${tableId}/shares`, { target: { type: "user", id: userId }, role });
}

function screen(flows: string[], grid: string): string {
  const buttons = flows.map((f, i) => `button ${i === 0 ? "申請する" : "経理に回す"} -> workflow ${f}`).join("\n");
  return `# 経費\n\n\`\`\`ui\napp 経費の申請\n件名 = text 必須 as title\n${buttons}\n\`\`\`\n\n\`\`\`data_grid\n${grid}\n\`\`\`\n`;
}

/// 別の人の画面（PC かスマホ）。ログインに失敗したら、作った画面を閉じてから落とす。
async function personOn(browser: Browser, user: string, phone: boolean): Promise<Page> {
  const ctx = await browser.newContext(
    phone
      ? { viewport: { width: 390, height: 844 }, hasTouch: true, isMobile: true, deviceScaleFactor: 2, ignoreHTTPSErrors: true }
      : { viewport: { width: 1440, height: 900 }, ignoreHTTPSErrors: true },
  );
  try {
    const page = await ctx.newPage();
    await loginAs(page, user);
    return page;
  } catch (e) {
    await ctx.close();
    throw e;
  }
}

/// ログインしている画面を、同じ人のスマホの画面にする（セッションを持ち込む）。
async function phoneOf(browser: Browser, loggedIn: Page): Promise<Page> {
  const ctx = await browser.newContext({
    storageState: await loggedIn.context().storageState(),
    viewport: { width: 390, height: 844 },
    hasTouch: true,
    isMobile: true,
    deviceScaleFactor: 2,
    ignoreHTTPSErrors: true,
  });
  return ctx.newPage();
}

async function noSideScroll(page: Page) {
  expect(await page.evaluate(() => document.documentElement.scrollWidth)).toBeLessThanOrEqual(
    page.viewportSize()?.width ?? 390,
  );
}

/// アプリの画面の共有を開く（右上の「共有」）。
async function openShare(page: Page, appId: string) {
  await page.goto(`/apps/${appId}`);
  await expect(page.getByRole("heading", { name: "経費の申請", level: 1 })).toBeVisible({ timeout: 20_000 });
  await page.getByRole("button", { name: "共有" }).click();
  const dialog = page.getByRole("dialog");
  await expect(dialog.getByTestId("access-report")).toBeVisible({ timeout: 20_000 });
  return dialog;
}

test.describe("使う人の権限の確かめ（GAP-37）", () => {
  test("作る人は共有の画面で相手ごとに足りない権限を見て、使う人は押す前に理由を読み、共有すると使える", async ({
    page,
    browser,
  }) => {
    test.setTimeout(300_000);
    await page.setViewportSize({ width: 1440, height: 900 });
    // bob を先に一度ログインさせる（ディレクトリに載せ、共有の相手に名前で出す）。
    const bob = await personOn(browser, "bob", false);
    let bobPhone: Page | null = null;
    let alicePhone: Page | null = null;
    try {
      await loginViaKeycloak(page); // alice（作る人）
      const bobId = ((await (await bob.request.get("/api/me")).json()) as { id: string }).id;
      const claims = await createTable(page, uniqueName("e2e-申請"));
      const ledgerName = uniqueName("e2e-台帳");
      const ledger = await createTable(page, ledgerName);
      const view = await api(page, "POST", "/data/views", { name: uniqueName("e2e-台帳の一覧"), table_id: ledger.id, query: {} });
      cleanup.add(() => deleteArtifactApi(page, view.id));
      const flow = uniqueName("access-flow");
      const flowId = await createAddFlow(page, flow, claims);
      const body = {
        description: "経費の申請",
        screen: screen([flow], "ledger"),
        workflows: [{ alias: flow, artifact_id: flowId, version: 1 }],
        data_views: [{ alias: "ledger", artifact_id: view.id, version: view.version ?? 1 }],
      };
      const app = await api(page, "POST", "/mini-apps", { name: uniqueName("access-app"), body });
      cleanup.add(() => deleteArtifactApi(page, app.id));
      await api(page, "POST", `/mini-apps/${app.id}/publish`, { version: 1 });
      await api(page, "PUT", `/artifacts/${app.id}/shares`, { target: { type: "user", id: bobId }, role: "viewer" });

      // 作る人: 共有の画面に、部品が要る権限と、bob に足りない権限・直し方が出る。
      let dialog = await openShare(page, app.id);
      const needs = dialog.getByTestId("access-report-needs");
      await expect(needs).toContainText(`ワークフロー「${flow}」: テーブル「`);
      await expect(needs).toContainText("にレコードを追加する");
      await expect(needs).toContainText(`表「ledger」: テーブル「${ledgerName}」を見る`);
      const bobCard = dialog.getByTestId("access-report-subject");
      await expect(bobCard).toHaveCount(1);
      await expect(bobCard).toContainText("は一部を使えません");
      await expect(bobCard).toContainText("テーブルを「追加だけ」か「編集」で共有してください。");
      await expect(bobCard).toContainText("テーブルを「閲覧」で共有してください。");
      if (SHOTS) await page.screenshot({ path: `${SHOTS}/access-share-pc.png` });
      await dialog.getByRole("button", { name: "閉じる" }).click();
      await expect(dialog).toHaveCount(0);

      // 作る人（スマホ）: 同じ確かめが下からのシートに収まる。
      alicePhone = await phoneOf(browser, page);
      dialog = await openShare(alicePhone, app.id);
      await expect(dialog.getByTestId("access-report-subject")).toContainText("は一部を使えません");
      await dialog.getByTestId("access-report").scrollIntoViewIfNeeded();
      await noSideScroll(alicePhone);
      if (SHOTS) await alicePhone.screenshot({ path: `${SHOTS}/access-share-phone.png` });
      await dialog.getByRole("button", { name: "閉じる" }).click();

      // 使う人: ボタンは押す前に止まり、理由が出る。表も読みに行かずに理由を出す。
      await bob.goto(`/apps/${app.id}`);
      await expect(bob.getByRole("heading", { name: "経費の申請", level: 1 })).toBeVisible({ timeout: 20_000 });
      const figure = bob.getByTestId("ui-figure");
      await expect(figure).toContainText(
        "このボタンが使うテーブルが見つからないか、レコードを追加する権限がありません。アプリを作成した人に頼んでください。",
        { timeout: 20_000 },
      );
      await expect(figure.getByRole("button", { name: "申請する" })).toBeDisabled();
      const blockedGrid = bob.getByTestId("miniapp-data-grid-blocked-ledger");
      await expect(blockedGrid).toContainText("表「ledger」を表示できません。この表が使うテーブルが見つからないか、見る権限がありません。");
      if (SHOTS) await bob.screenshot({ path: `${SHOTS}/access-user-blocked-pc.png` });
      bobPhone = await phoneOf(browser, bob);
      await bobPhone.goto(`/apps/${app.id}`);
      await expect(bobPhone.getByTestId("ui-figure").getByRole("button", { name: "申請する" })).toBeDisabled({ timeout: 20_000 });
      await expect(bobPhone.getByTestId("miniapp-data-grid-blocked-ledger")).toBeVisible();
      await noSideScroll(bobPhone);
      if (SHOTS) await bobPhone.screenshot({ path: `${SHOTS}/access-user-blocked-phone.png` });

      // 作る人が申請のテーブルを「追加だけ」、台帳を「閲覧」で共有すると、共有の画面は全員足りると言う。
      await shareTable(page, claims.id, bobId, "submitter");
      await shareTable(page, ledger.id, bobId, "viewer");
      dialog = await openShare(page, app.id);
      await expect(dialog.getByTestId("access-report-all-ok")).toContainText("共有している相手は全員、要る権限を持っています。");
      await expect(dialog.getByTestId("access-report-subject")).toHaveCount(0);
      await dialog.getByRole("button", { name: "閉じる" }).click();

      // 使う人: 開き直すとボタンを押せ、表も出る。
      await bob.reload();
      await expect(bob.getByTestId("ui-figure").getByRole("button", { name: "申請する" })).toBeEnabled({ timeout: 20_000 });
      await expect(bob.getByTestId("miniapp-data-grid-ledger")).toBeVisible({ timeout: 20_000 });
      await expect(bob.getByTestId("ui-figure")).not.toContainText("権限がありません");

      // 公開の確認: 経理のテーブルに書くボタンを足した v2 は、bob には一部を使えないと知らせる（公開は止めない）。
      const hidden = await createTable(page, uniqueName("e2e-経理"));
      const flow2 = uniqueName("access-flow2");
      const flow2Id = await createAddFlow(page, flow2, hidden);
      await api(page, "PUT", `/mini-apps/${app.id}`, {
        expected_version: 1,
        body: {
          ...body,
          screen: screen([flow, flow2], "ledger"),
          workflows: [...body.workflows, { alias: flow2, artifact_id: flow2Id, version: 1 }],
        },
      });
      await page.goto(`/apps/${app.id}`);
      await page.getByTestId("miniapp-publish-bar").getByRole("button", { name: "下書きを試す" }).click();
      await page.getByTestId("miniapp-publish").click();
      const publish = page.getByTestId("miniapp-publish-dialog");
      await expect(publish.getByTestId("access-report")).toContainText(
        "共有している相手のうち 1 件は、このバージョンの一部を使えません。",
        { timeout: 20_000 },
      );
      await expect(publish.getByTestId("access-report-missing")).toContainText(`ワークフロー「${flow2}」: テーブル「`);
      await expect(publish.getByRole("button", { name: "公開", exact: true })).toBeEnabled();
      if (SHOTS) await page.screenshot({ path: `${SHOTS}/access-publish-pc.png` });
      await publish.getByRole("button", { name: "キャンセル" }).click();

      // 使う人の画面（v1 のまま）は変わらない。
      await bob.reload();
      await expect(bob.getByTestId("ui-figure").getByRole("button", { name: "経理に回す" })).toHaveCount(0, { timeout: 20_000 });
    } finally {
      await cleanupAll(
        async () => bobPhone?.context().close(),
        async () => alicePhone?.context().close(),
        () => bob.context().close(),
      );
    }
  });

  test("図の一部のボタンだけが使えないときは、押せないボタンと理由を図の下に出し、押しても送らない", async ({ page, browser }) => {
    test.setTimeout(240_000);
    await page.setViewportSize({ width: 1440, height: 900 });
    const bob = await personOn(browser, "bob", false);
    try {
      await loginViaKeycloak(page);
      const bobId = ((await (await bob.request.get("/api/me")).json()) as { id: string }).id;
      const claims = await createTable(page, uniqueName("e2e-申請"));
      const hidden = await createTable(page, uniqueName("e2e-経理"));
      const ledger = await createTable(page, uniqueName("e2e-台帳"));
      const view = await api(page, "POST", "/data/views", { name: uniqueName("e2e-台帳の一覧"), table_id: ledger.id, query: {} });
      cleanup.add(() => deleteArtifactApi(page, view.id));
      const flow = uniqueName("access-ok");
      const flow2 = uniqueName("access-ng");
      const flowId = await createAddFlow(page, flow, claims);
      const flow2Id = await createAddFlow(page, flow2, hidden);
      const app = await api(page, "POST", "/mini-apps", {
        name: uniqueName("access-app"),
        body: {
          description: "経費の申請",
          screen: screen([flow, flow2], "ledger"),
          workflows: [
            { alias: flow, artifact_id: flowId, version: 1 },
            { alias: flow2, artifact_id: flow2Id, version: 1 },
          ],
          data_views: [{ alias: "ledger", artifact_id: view.id, version: view.version ?? 1 }],
        },
      });
      cleanup.add(() => deleteArtifactApi(page, app.id));
      await api(page, "POST", `/mini-apps/${app.id}/publish`, { version: 1 });
      await api(page, "PUT", `/artifacts/${app.id}/shares`, { target: { type: "user", id: bobId }, role: "viewer" });
      await shareTable(page, claims.id, bobId, "submitter");
      await shareTable(page, ledger.id, bobId, "viewer");

      let sent = 0;
      // 表が先頭のページ（1 件）を読んだ回数を、権限の確かめの結果が届く前と後で数える。結果は、表が先頭のページを
      // 読みに行ってから返す。結果が届いて画面の部品が作り直されても、表は先頭のページを読み直さない。
      let accessReleased = false;
      let gridBeforeAccess = 0;
      let gridAfterAccess = 0;
      bob.on("request", (req) => {
        if (req.method() === "POST" && req.url().includes(`/api/mini-apps/${app.id}/ui-actions`)) {
          const body = req.postDataJSON() as { action_id?: string; params?: { limit?: number } } | null;
          const action = body?.action_id ?? "";
          if (!action.startsWith("data_grid:")) sent += 1;
          else if (body?.params?.limit === 1) {
            if (accessReleased) gridAfterAccess += 1;
            else gridBeforeAccess += 1;
          }
        }
      });
      await bob.route(
        (url) => url.pathname === `/api/mini-apps/${app.id}/access`,
        async (route) => {
          await expect.poll(() => gridBeforeAccess, { timeout: 20_000 }).toBeGreaterThan(0);
          accessReleased = true;
          await route.continue();
        },
      );
      await bob.goto(`/apps/${app.id}`);
      const figure = bob.getByTestId("ui-figure");
      const notes = figure.getByTestId("ui-figure-blocked");
      await expect(notes).toContainText(
        "「経理に回す」は押せません。このボタンが使うテーブルが見つからないか、レコードを追加する権限がありません。",
        { timeout: 20_000 },
      );
      await expect(notes).not.toContainText("申請する");
      await expect(bob.getByTestId("miniapp-data-grid-ledger")).toBeVisible({ timeout: 20_000 });
      // 使えるボタンは押せる。押せないボタンは押しても送らず、理由を出す。
      await figure.getByLabel("件名").fill("出張");
      await expect(figure.getByRole("button", { name: "申請する" })).toBeEnabled();
      await figure.getByRole("button", { name: "経理に回す" }).click();
      // ワークフローのボタンは実行の確認を挟む（Yui の既定）。確かめた後も、足りない権限があれば送らない。
      await figure.getByRole("group", { name: "実行の確認" }).getByRole("button", { name: "実行する" }).click();
      await expect(figure.getByRole("status").filter({ hasText: "権限がありません" })).toBeVisible({ timeout: 10_000 });
      expect(sent).toBe(0);
      expect(gridAfterAccess).toBe(0);
      if (SHOTS) await bob.screenshot({ path: `${SHOTS}/access-user-partial-pc.png` });
    } finally {
      await bob.context().close();
    }
  });

  test("相手の権限は自分が持ち主の対象だけ確かめ、ほかの人のテーブルは確かめていないと言う", async ({ page, browser }) => {
    test.setTimeout(180_000);
    await page.setViewportSize({ width: 1440, height: 900 });
    const bob = await personOn(browser, "bob", false);
    let bobTable: string | null = null;
    let alicePhone: Page | null = null;
    try {
      await loginViaKeycloak(page);
      const aliceId = ((await (await page.request.get("/api/me")).json()) as { id: string }).id;
      const bobId = ((await (await bob.request.get("/api/me")).json()) as { id: string }).id;
      // bob のテーブルに、alice は追加だけで共有されている（共有の一覧は bob にしか見えない）。
      const tableName = uniqueName("e2e-bob の表");
      const t = (await api(bob, "POST", "/data/tables", {
        name: tableName,
        schema: { fields: [{ key: "title", display_name: "件名", type: "text" }] },
      })) as { id: string; schema?: { fields: { key: string; display_name: string }[] } };
      bobTable = t.id;
      const titleKey = t.schema?.fields.find((f) => f.display_name === "件名")?.key ?? "title";
      await api(bob, "PUT", `/data/tables/${t.id}/shares`, { target: { type: "user", id: aliceId }, role: "submitter" });
      const flowId = await createAddFlow(page, uniqueName("access-others"), { id: t.id, titleKey });
      await api(page, "PUT", `/artifacts/${flowId}/shares`, { target: { type: "user", id: bobId }, role: "viewer" });

      await page.goto(`/workflows/${flowId}`);
      await page.getByTestId("workflow-share").click();
      const dialog = page.getByRole("dialog");
      await expect(dialog.getByTestId("access-report-needs")).toContainText(`テーブル「${tableName}」にレコードを追加する`, {
        timeout: 20_000,
      });
      await expect(dialog.getByTestId("access-report-notes")).toContainText(
        "ほかの人が持ち主のテーブルやファイルは、相手の権限を確かめていません。その持ち主に確かめてもらってください。",
      );
      // 何も確かめていないので、「全員使える」とも「足りない」とも言わない。
      await expect(dialog.getByTestId("access-report-all-ok")).toHaveCount(0);
      await expect(dialog.getByTestId("access-report-subject")).toHaveCount(0);
      if (SHOTS) await page.screenshot({ path: `${SHOTS}/access-others-table-pc.png` });
      await dialog.getByRole("button", { name: "閉じる" }).click();

      // スマホでも同じ注記が出て、横にはみ出さない。
      alicePhone = await phoneOf(browser, page);
      await alicePhone.goto(`/workflows/${flowId}`);
      await alicePhone.getByTestId("chrome-menu").click();
      await alicePhone.getByRole("menuitem", { name: "共有" }).click();
      await expect(alicePhone.getByRole("dialog").getByTestId("access-report-notes")).toContainText(
        "ほかの人が持ち主のテーブルやファイル",
        { timeout: 20_000 },
      );
      await noSideScroll(alicePhone);
      if (SHOTS) await alicePhone.screenshot({ path: `${SHOTS}/access-others-table-phone.png` });
    } finally {
      await cleanupAll(
        async () => alicePhone?.context().close(),
        async () => (bobTable ? deleteTableApi(bob, bobTable) : undefined),
        () => bob.context().close(),
      );
    }
  });

  test("ワークフローの共有の画面も、手動で実行するのに要る権限を相手ごとに言う", async ({ page, browser }) => {
    test.setTimeout(180_000);
    await page.setViewportSize({ width: 1440, height: 900 });
    const bob = await personOn(browser, "bob", false);
    let alicePhone: Page | null = null;
    try {
      await loginViaKeycloak(page);
      const bobId = ((await (await bob.request.get("/api/me")).json()) as { id: string }).id;
      const tableName = uniqueName("e2e-申請");
      const table = await createTable(page, tableName);
      const flow = uniqueName("access-wf");
      const flowId = await createAddFlow(page, flow, table);
      await api(page, "PUT", `/artifacts/${flowId}/shares`, { target: { type: "user", id: bobId }, role: "viewer" });

      await page.goto(`/workflows/${flowId}`);
      await page.getByTestId("workflow-share").click();
      const dialog = page.getByRole("dialog");
      const report = dialog.getByTestId("access-report");
      await expect(report).toBeVisible({ timeout: 20_000 });
      await expect(dialog.getByTestId("access-report-needs")).toContainText(`テーブル「${tableName}」にレコードを追加する`);
      await expect(dialog.getByTestId("access-report-subject")).toContainText(
        "テーブルを「追加だけ」か「編集」で共有してください。",
      );
      if (SHOTS) await page.screenshot({ path: `${SHOTS}/access-workflow-share-pc.png` });
      await dialog.getByRole("button", { name: "閉じる" }).click();

      // 共有したら足りると言う（相手を足し外しした後も、開き直したときも確かめ直す）。
      await shareTable(page, table.id, bobId, "submitter");
      await page.getByTestId("workflow-share").click();
      await expect(page.getByRole("dialog").getByTestId("access-report-all-ok")).toBeVisible({ timeout: 20_000 });
      await page.getByRole("dialog").getByRole("button", { name: "閉じる" }).click();

      alicePhone = await phoneOf(browser, page);
      await alicePhone.goto(`/workflows/${flowId}`);
      // スマホでは共有は右上の「その他の操作」のメニューにある。
      await alicePhone.getByTestId("chrome-menu").click();
      await alicePhone.getByRole("menuitem", { name: "共有" }).click();
      await expect(alicePhone.getByRole("dialog").getByTestId("access-report-all-ok")).toBeVisible({ timeout: 20_000 });
      await noSideScroll(alicePhone);
      if (SHOTS) await alicePhone.screenshot({ path: `${SHOTS}/access-workflow-share-phone.png` });
    } finally {
      await cleanupAll(
        async () => alicePhone?.context().close(),
        () => bob.context().close(),
      );
    }
  });
});
