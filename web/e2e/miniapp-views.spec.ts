import { test, expect, type Browser, type Page } from "@playwright/test";

import {
  cleanupAll,
  cleanupQueue,
  deleteArtifactApi,
  deleteTableApi,
  loginAs,
  loginViaKeycloak,
  removeWorkflowApi,
  trackCreatedResources,
  uniqueName,
} from "./helpers";

/// アプリのエディターでビューを束ね、束ねた部品の版を上げる（GAP-11）の E2E。
/// - 束ねたワークフローとビューが改訂されていると、新しいバージョンと、上げると変わるところ（入力項目の増減・必須、
///   ビューの絞り込み）が出る。「v2 にする」で上げ、保存の前なら元に戻せる。
/// - ビューを追加すると、組み立てるに表の段が足され、表に出すビューを選べる。保存した下書きでは表が描かれ、公開の確認は
///   エディターと同じ呼び名で差を言う。
/// - ビューを読めない共同編集者も、そのビューに触れなければ保存できる（BUG-80 と同じ扱い）。
/// PC（1440px）とスマホ（390px）で確かめる。作ったもの（アプリ・ワークフロー・ビュー・テーブル）は最後に消す。
/// `E2E_SHOTS_DIR` を渡すとスクリーンショットを撮る。

const SHOTS = process.env.E2E_SHOTS_DIR;

const cleanup = cleanupQueue();
test.beforeEach(({ page }) => trackCreatedResources(page, cleanup));
test.afterEach(() => cleanup.run());

async function csrf(page: Page): Promise<string> {
  return (await page.context().cookies()).find((c) => c.name === "shiki_csrf")?.value ?? "";
}

async function api<T = Record<string, unknown>>(
  page: Page,
  method: "GET" | "POST" | "PUT" | "DELETE",
  path: string,
  body?: unknown,
): Promise<T> {
  const res = await page.request.fetch(`/api${path}`, {
    method,
    headers: { "Content-Type": "application/json", "X-CSRF-Token": await csrf(page) },
    ...(body === undefined ? {} : { data: body }),
  });
  expect(res.ok(), `${method} ${path} → ${res.status()} ${await res.text()}`).toBeTruthy();
  return (res.status() === 204 ? {} : await res.json()) as T;
}

/// 入力項目を持つワークフローの IR（`title` を足すと件名が必須の入力になる）。
function flowIr(name: string, withTitle: boolean) {
  return {
    ir_version: 1,
    name,
    display_name: name,
    triggers: [{ kind: "interactive" }],
    input_schema: withTitle
      ? {
          type: "object",
          required: ["title"],
          "x-order": ["title", "amount"],
          properties: {
            title: { type: "string", title: "件名" },
            amount: { type: "integer", title: "金額" },
          },
        }
      : { type: "object", required: ["amount"], properties: { amount: { type: "integer", title: "金額" } } },
    nodes: [
      {
        id: "compute",
        type: "script.run",
        label: "計算する",
        params: { source: { inline: "function main(i){ return { ok: true }; }" } },
      },
    ],
    edges: [],
  };
}

type Fixture = { appId: string; flow: string; viewA: string; viewB: string };

/// テーブル・ビュー 2 つ・ワークフローと、ワークフローとビュー A（どちらも v1）を束ねたアプリを作って v1 を公開する。
/// その後でワークフローとビュー A を v2 に改訂する（最後に消す）。
async function setup(page: Page): Promise<Fixture> {
  const table = await api<{ id: string; schema?: { fields: { key: string; display_name: string }[] } }>(
    page,
    "POST",
    "/data/tables",
    { name: uniqueName("e2e-申請台帳"), schema: { fields: [{ key: "title", display_name: "件名", type: "text" }] } },
  );
  cleanup.add(() => deleteTableApi(page, table.id));
  const titleKey = table.schema?.fields.find((f) => f.display_name === "件名")?.key ?? "title";
  await api(page, "POST", `/data/tables/${table.id}/records`, { data: { [titleKey]: "出張の交通費" } });
  const viewA = uniqueName("e2e-申請の一覧");
  const a = await api<{ id: string }>(page, "POST", "/data/views", { name: viewA, table_id: table.id, query: {} });
  cleanup.add(() => deleteArtifactApi(page, a.id));
  const viewB = uniqueName("e2e-最近の申請");
  const b = await api<{ id: string }>(page, "POST", "/data/views", { name: viewB, table_id: table.id, query: {} });
  cleanup.add(() => deleteArtifactApi(page, b.id));
  const flow = uniqueName("views-flow");
  const wf = await api<{ id: string }>(page, "POST", "/workflows", { ir: flowIr(flow, false) });
  cleanup.add(() => removeWorkflowApi(page, wf.id));
  const app = await api<{ id: string }>(page, "POST", "/mini-apps", {
    name: uniqueName("views-app"),
    body: {
      description: "経費の申請",
      screen: `申請の一覧です。\n\n\`\`\`data_grid\n${viewA}\n\`\`\`\n\n\`\`\`ui\napp 経費の申請\n金額 = number 円 as amount\nbutton 申請する -> workflow ${flow}\n\`\`\`\n`,
      workflows: [{ alias: flow, artifact_id: wf.id, version: 1 }],
      data_views: [{ alias: viewA, artifact_id: a.id, version: 1 }],
    },
  });
  cleanup.add(() => deleteArtifactApi(page, app.id));
  // v1 を公開しておく（公開の確認が、公開した版との差を言う）。
  await api(page, "POST", `/mini-apps/${app.id}/publish`, { version: 1 });
  // 束ねた後に、ワークフローとビュー A を改訂する（アプリは v1 を固定したまま）。
  await api(page, "PUT", `/workflows/${wf.id}`, { ir: flowIr(flow, true), expected_version: 1 });
  await api(page, "PUT", `/data/views/${a.id}`, { table_id: table.id, query: { limit: 20 }, expected_version: 1 });
  return { appId: app.id, flow, viewA, viewB };
}

/// 別の人の画面（PC かスマホ）。ログインに失敗したら、作った画面を閉じてから落とす。
async function personOn(browser: Browser, user: string): Promise<Page> {
  const ctx = await browser.newContext({ viewport: { width: 1440, height: 900 }, ignoreHTTPSErrors: true });
  try {
    const page = await ctx.newPage();
    await loginAs(page, user);
    return page;
  } catch (e) {
    await ctx.close();
    throw e;
  }
}

async function openEditor(page: Page, appId: string) {
  await page.goto(`/apps/${appId}`);
  await page.getByRole("button", { name: "編集" }).click();
  const dialog = page.getByRole("dialog");
  await expect(dialog.getByRole("heading", { name: "アプリを編集" })).toBeVisible();
  return dialog;
}

test.describe("アプリのエディターのビューと版を上げる操作（GAP-11）", () => {
  test("新しいバージョンの差を見て上げ、ビューを追加して表を置き、下書きで表が描かれる", async ({ page, browser }) => {
    test.setTimeout(180_000);
    await loginViaKeycloak(page);
    const f = await setup(page);
    const dialog = await openEditor(page, f.appId);

    // ワークフロー: v2 があり、入力項目の変化を言う。
    const wfPins = dialog.getByTestId("miniapp-workflow-pins");
    const wfUpdate = wfPins.getByTestId("miniapp-pin-update");
    await expect(wfUpdate).toContainText("新しいバージョン v2 があります", { timeout: 15_000 });
    await expect(wfUpdate.getByTestId("miniapp-pin-changes")).toContainText("入力項目「件名」（title）が増えます（必須）");
    await expect(wfUpdate.getByTestId("miniapp-pin-changes")).toContainText("入力項目「金額」（amount）が必須でなくなります");
    // ビュー A: v2 があり、絞り込みが変わると言う（列の名前や値は出さない）。
    const viewPins = dialog.getByTestId("miniapp-data_view-pins");
    const viewUpdate = viewPins.getByTestId("miniapp-pin-update");
    await expect(viewUpdate).toContainText("新しいバージョン v2 があります");
    await expect(viewUpdate.getByTestId("miniapp-pin-changes")).toHaveText("絞り込み・並べ替え・表示する列が変わります");
    if (SHOTS) await dialog.screenshot({ path: `${SHOTS}/miniapp-views-updates-pc.png` });

    // ワークフローを v2 に上げると、元に戻せる。戻してから、もう一度上げる。
    await wfUpdate.getByRole("button", { name: "v2 にする" }).click();
    const changed = wfPins.getByTestId("miniapp-pin-changed");
    await expect(changed).toContainText("v1 から v2 にしました");
    await expect(wfPins.getByTestId("miniapp-pin-update")).toHaveCount(0);
    await changed.getByRole("button", { name: "v1 に戻す" }).click();
    await expect(wfPins.getByTestId("miniapp-pin-changed")).toHaveCount(0);
    await wfPins.getByTestId("miniapp-pin-update").getByRole("button", { name: "v2 にする" }).click();
    await viewUpdate.getByRole("button", { name: "v2 にする" }).click();
    await expect(viewPins.getByTestId("miniapp-pin-changed")).toContainText("v1 から v2 にしました");

    // ビュー B を追加すると、組み立てるの最後に表の段が足され、表に出すビューを選べる。
    await dialog.getByLabel("追加するビュー").selectOption({ label: `${f.viewB}（v1）` });
    await dialog.getByRole("button", { name: "ビューを追加" }).click();
    const grids = dialog.getByTestId("screen-builder-grid");
    await expect(grids).toHaveCount(2);
    await expect(grids.last().getByRole("combobox")).toHaveValue(f.viewB);
    await expect(grids.first().getByRole("combobox")).toHaveValue(f.viewA);
    await dialog.getByTestId("screen-builder-grid").last().scrollIntoViewIfNeeded();
    if (SHOTS) await dialog.screenshot({ path: `${SHOTS}/miniapp-views-builder-pc.png` });
    // 「表」で表の段を足すと、追加したビューから選べる。消すと元に戻る。
    await dialog.getByRole("button", { name: "表", exact: true }).click();
    await expect(grids).toHaveCount(3);
    await expect(grids.last().getByRole("combobox").locator("option")).toHaveText([f.viewA, f.viewB]);
    await dialog.getByRole("button", { name: `表「${f.viewA}」を消す` }).last().click();
    await expect(grids).toHaveCount(2);
    // 書くタブの記述にも、ビュー B の表が入っている。
    await dialog.getByRole("tab", { name: "書く" }).click();
    await expect(dialog.getByRole("textbox", { name: /^画面\*?$/ })).toHaveValue(
      new RegExp(`\`\`\`data_grid\\n${f.viewB}\\n\`\`\`\\n?$`),
    );

    await dialog.getByRole("button", { name: "保存", exact: true }).click();
    await expect(page.getByText("v2 を下書きとして保存しました", { exact: true })).toBeVisible({ timeout: 15_000 });
    // 下書きを開くと、2 つの表が描かれる（作る人は下書きを試せる）。
    await expect(page.getByTestId(`miniapp-data-grid-${f.viewA}`)).toBeVisible({ timeout: 30_000 });
    await expect(page.getByTestId(`miniapp-data-grid-${f.viewB}`)).toBeVisible({ timeout: 30_000 });
    const saved = await api<{ body: { workflows: { version: number }[]; data_views: { alias: string; version: number }[] } }>(
      page,
      "GET",
      `/mini-apps/${f.appId}/resolved?version=2`,
    );
    expect(saved.body.workflows[0]!.version).toBe(2);
    expect(saved.body.data_views).toEqual([
      expect.objectContaining({ alias: f.viewA, version: 2 }),
      expect.objectContaining({ alias: f.viewB, version: 1 }),
    ]);

    // 公開の確認は、エディターと同じ呼び名で差を言う。
    await page.getByTestId("miniapp-publish-bar").getByRole("button", { name: "公開", exact: true }).click();
    const confirm = page.getByTestId("miniapp-publish-dialog");
    const changes = confirm.getByTestId("miniapp-publish-changes");
    await expect(changes).toContainText(`ワークフロー「${f.flow}」: v1 から v2 へ`, { timeout: 15_000 });
    await expect(changes).toContainText(`ビュー「${f.viewA}」: v1 から v2 へ`);
    await expect(changes).toContainText(`ビュー「${f.viewB}」を追加する（v1）`);
    await confirm.getByRole("button", { name: "キャンセル" }).click();

    // スマホ幅のエディター（部品の一覧と表の段・横にはみ出さない）。
    if (SHOTS) {
      const ctx = await browser.newContext({
        storageState: await page.context().storageState(),
        viewport: { width: 390, height: 844 },
        hasTouch: true,
        isMobile: true,
        deviceScaleFactor: 2,
        ignoreHTTPSErrors: true,
      });
      try {
        const phone = await ctx.newPage();
        await phone.goto(`/apps/${f.appId}`);
        await phone.getByRole("button", { name: "編集" }).click();
        const d = phone.getByRole("dialog");
        await expect(d.getByTestId("miniapp-views")).toBeVisible({ timeout: 15_000 });
        await d.getByTestId("miniapp-views").scrollIntoViewIfNeeded();
        await phone.screenshot({ path: `${SHOTS}/miniapp-views-editor-phone.png` });
        await d.getByTestId("screen-builder-grid").first().scrollIntoViewIfNeeded();
        await phone.screenshot({ path: `${SHOTS}/miniapp-views-grid-phone.png` });
        const overflow = await phone.evaluate(() => document.documentElement.scrollWidth - window.innerWidth);
        expect(overflow).toBeLessThanOrEqual(0);
      } finally {
        await ctx.close();
      }
    }
  });

  test("ビューを読めない共同編集者も、そのビューに触れなければ保存できる", async ({ page, browser }) => {
    test.setTimeout(180_000);
    await loginViaKeycloak(page);
    const f = await setup(page);
    // bob をアプリの編集者にする（ワークフローとビューは共有しない）。
    const bob = await personOn(browser, "bob");
    try {
      const bobId = (await api<{ id: string }>(bob, "GET", "/me")).id;
      await api(page, "PUT", `/artifacts/${f.appId}/shares`, { target: { type: "user", id: bobId }, role: "editor" });
      const dialog = await openEditor(bob, f.appId);
      const viewPins = dialog.getByTestId("miniapp-data_view-pins");
      await expect(viewPins.getByTestId("miniapp-pin-unreadable")).toContainText(
        `ビュー「${f.viewA}」が見つかりません。削除されたか、見る権限がありません。変えなければ、このまま保存できます。`,
        { timeout: 15_000 },
      );
      // 読めない部品には、新しいバージョンも差も出さない。
      await expect(dialog.getByTestId("miniapp-pin-update")).toHaveCount(0);
      await viewPins.scrollIntoViewIfNeeded();
      if (SHOTS) await bob.screenshot({ path: `${SHOTS}/miniapp-views-unreadable-pc.png` });
      await dialog.getByLabel(/画面の見出し/).fill("経費の申請（直した）");
      await dialog.getByRole("button", { name: "保存", exact: true }).click();
      await expect(bob.getByText("v2 を下書きとして保存しました", { exact: true })).toBeVisible({ timeout: 15_000 });
      const saved = await api<{ body: { data_views: { alias: string; version: number }[] } }>(
        page,
        "GET",
        `/mini-apps/${f.appId}/resolved?version=2`,
      );
      expect(saved.body.data_views).toEqual([expect.objectContaining({ alias: f.viewA, version: 1 })]);
    } finally {
      await cleanupAll(() => bob.context().close());
    }
  });
});
