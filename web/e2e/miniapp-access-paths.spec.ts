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

/// 使う人の権限の確かめ（GAP-37）の続きの E2E。
/// - 分かれた流れの片方だけが書くテーブルの権限が無くても、使う人のボタンは止まらず、押すと動く（BUG-216）。作る人の
///   共有の画面は、それを「場合によって要る」と言い分ける。
/// - 止まった表は、画面に戻ったときに確かめ直して出る。確かめ直しは間隔を空け、同時に 1 つだけ問う（BUG-217）。
/// - スクリプトやドライブのトップへの保存など、前もって確かめられないブロックがあれば、名前を挙げて言い切らない（GAP-82）。
///   文書検索とフォルダーを選ばない一覧も、AI エージェントと同じく、使う人が見られるものだけを使うと言って挙げる。
///   足りない相手がいるときの公開の確認でも、そのようなブロックがあることを言う。
/// - 共有されたワークフローの「いま実行する」も、自分に足りない権限を押す前に言い、共有されれば開き直さずに実行できる
///   （GAP-37）。
/// - 失敗を「エラー時」の出口で扱うブロックの権限が無くても、ボタンと「いま実行する」は止めず、作った人が用意した
///   誤りの返答で終わる（7g の前と同じ）。
/// - 範囲が狭いだけの部品（追加だけの共有で読むワークフロー）と、自分が追加したレコードだけを出す表（作った人がそう決めた
///   落ち着いた状態）では、画面に戻っても確かめ直さない（BUG-232）。
/// - 持ち主の「いま実行する」は、持ち主かを確かめの答えと一緒に受け、別の問い合わせに失敗しても、作成した人に頼めとは
///   言わない（BUG-230）。
/// - 毎回動く繰り返しの中で 1 件ずつテーブルに足すワークフローは、追加の権限が無い使う人のボタンを押す前に止め、作る人
///   には「場合によって要る」を付けずに言う。共有されれば押したときに確かめ直して動く（GAP-97）。
/// PC（1440px）とスマホ（390px）で確かめる。作ったもの（アプリ・ワークフロー・ビュー・テーブル）は最後に消す。
/// `E2E_SHOTS_DIR` を渡すとスクリーンショットを撮る。

const SHOTS = process.env.E2E_SHOTS_DIR;

const cleanup = cleanupQueue();
test.afterEach(() => cleanup.run());

async function csrf(page: Page): Promise<string> {
  return (await page.context().cookies()).find((c) => c.name === "shiki_csrf")?.value ?? "";
}

async function api<T = { id: string; version?: number }>(page: Page, method: "POST" | "PUT", path: string, body: unknown): Promise<T> {
  const res = await page.request.fetch(`/api${path}`, {
    method,
    headers: { "Content-Type": "application/json", "X-CSRF-Token": await csrf(page) },
    data: body,
  });
  expect(res.ok(), `${method} ${path} → ${res.status()} ${await res.text()}`).toBeTruthy();
  return (res.status() === 204 ? {} : await res.json()) as T;
}

async function createTable(page: Page, name: string): Promise<{ id: string; titleKey: string }> {
  const t = await api<{ id: string; schema?: { fields: { key: string; display_name: string }[] } }>(page, "POST", "/data/tables", {
    name,
    schema: { fields: [{ key: "title", display_name: "件名", type: "text" }] },
  });
  cleanup.add(() => deleteTableApi(page, t.id));
  return { id: t.id, titleKey: t.schema?.fields.find((f) => f.display_name === "件名")?.key ?? "title" };
}

async function createFlow(page: Page, ir: Record<string, unknown>): Promise<string> {
  const wf = await api(page, "POST", "/workflows", { ir });
  cleanup.add(() => removeWorkflowApi(page, wf.id));
  return wf.id;
}

async function shareTable(page: Page, tableId: string, userId: string, role: "viewer" | "submitter") {
  await api(page, "PUT", `/data/tables/${tableId}/shares`, { target: { type: "user", id: userId }, role });
}

async function shareArtifact(page: Page, id: string, userId: string) {
  await api(page, "PUT", `/artifacts/${id}/shares`, { target: { type: "user", id: userId }, role: "viewer" });
}

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

async function meId(page: Page): Promise<string> {
  return ((await (await page.request.get("/api/me")).json()) as { id: string }).id;
}

test.describe("使う人の権限の確かめの続き（BUG-216・BUG-217・GAP-82・GAP-37）", () => {
  test("分かれた流れの片方だけが書くテーブルでは使う人のボタンを止めず、作る人には「場合によって要る」と言う", async ({
    page,
    browser,
  }) => {
    test.setTimeout(240_000);
    await page.setViewportSize({ width: 1440, height: 900 });
    const bob = await personOn(browser, "bob");
    let alicePhone: Page | null = null;
    try {
      await loginViaKeycloak(page);
      const bobId = await meId(bob);
      const claims = await createTable(page, uniqueName("e2e-申請"));
      const bigName = uniqueName("e2e-高額の確認");
      const big = await createTable(page, bigName);
      await shareTable(page, claims.id, bobId, "submitter");
      // 申請を足し、金額が 10 万円を超えるときだけ高額の確認に足す（bob は高額の確認の権限を持たない）。
      const flow = uniqueName("paths-flow");
      const flowId = await createFlow(page, {
        ir_version: 1,
        name: flow,
        display_name: flow,
        declared_scopes: ["data.write"],
        triggers: [{ kind: "interactive" }],
        nodes: [
          {
            id: "add",
            type: "data.record.create",
            label: "申請に足す",
            params: { table: claims.id, data: { [claims.titleKey]: { $from: "input", path: "/title" } } },
          },
          {
            id: "check",
            type: "control.branch",
            label: "高額か",
            params: { condition: { cmp: { left: { $from: "input", path: "/amount" }, op: "gt", right: 100000 } } },
          },
          {
            id: "big",
            type: "data.record.create",
            label: "高額の確認に足す",
            params: { table: big.id, data: { [big.titleKey]: { $from: "input", path: "/title" } } },
          },
        ],
        edges: [
          { from: "add", to: "check" },
          { from: "check", from_port: "true", to: "big" },
        ],
      });
      const screen = `# 経費\n\n\`\`\`ui\napp 経費の申請\n件名 = text 必須 as title\n金額 = number 必須 円 as amount\nbutton 申請する -> workflow ${flow}\n\`\`\`\n`;
      const app = await api(page, "POST", "/mini-apps", {
        name: uniqueName("paths-app"),
        body: { description: "経費の申請", screen, workflows: [{ alias: flow, artifact_id: flowId, version: 1 }] },
      });
      cleanup.add(() => deleteArtifactApi(page, app.id));
      await api(page, "POST", `/mini-apps/${app.id}/publish`, { version: 1 });
      await shareArtifact(page, app.id, bobId);

      // 使う人: ボタンは止まらず、押すと動く（10 万円以下なので高額の確認には書かない）。
      await bob.goto(`/apps/${app.id}`);
      const figure = bob.getByTestId("ui-figure");
      await expect(figure.getByRole("button", { name: "申請する" })).toBeEnabled({ timeout: 20_000 });
      await bob.waitForLoadState("networkidle");
      await expect(figure.getByTestId("ui-figure-blocked")).toHaveCount(0);
      await figure.getByLabel("件名").fill("出張");
      await figure.getByLabel("金額").fill("5000");
      await figure.getByRole("button", { name: "申請する" }).click();
      await figure.getByRole("group", { name: "実行の確認" }).getByRole("button", { name: "実行する" }).click();
      await expect(figure.getByRole("status").filter({ hasText: "起動しました" })).toBeVisible({ timeout: 15_000 });
      // 裏取り: 実行が終わり、申請のテーブルにレコードが 1 件ある。
      await expect
        .poll(
          async () => {
            const res = await bob.request.get(`/api/mini-apps/${app.id}/runs`);
            const runs = ((await res.json()) as { runs: { status: string }[] }).runs;
            return runs[0]?.status ?? "none";
          },
          { timeout: 60_000 },
        )
        .toBe("succeeded");
      const records = (await (await page.request.get(`/api/data/tables/${claims.id}/records`)).json()) as { items: unknown[] };
      expect(records.items).toHaveLength(1);
      if (SHOTS) await bob.screenshot({ path: `${SHOTS}/paths-user-pc.png` });

      // 作る人: 高額の確認は「場合によって要る」と印を付け、bob は場合によって一部を使えないと言う。
      await page.goto(`/apps/${app.id}`);
      await page.getByRole("button", { name: "共有" }).click();
      const dialog = page.getByRole("dialog");
      const needs = dialog.getByTestId("access-report-needs");
      await expect(needs).toContainText(`テーブル「${bigName}」にレコードを追加する`, { timeout: 20_000 });
      await expect(needs.getByTestId("access-conditional")).toHaveCount(1);
      await expect(needs.getByTestId("access-conditional")).toHaveText("場合によって要る");
      await expect(dialog.getByTestId("access-report-subject")).toContainText("は場合によって一部を使えません");
      await expect(dialog.getByTestId("access-report-conditional-note")).toContainText(
        "場合によって要る権限は、毎回は通らないブロックと、失敗を「エラー時」の出口か「失敗を記録して続ける」繰り返しで扱うブロックが使います。足りなくてもボタンは押せますが、そのブロックまで進むと失敗します。",
      );
      if (SHOTS) await page.screenshot({ path: `${SHOTS}/paths-share-pc.png` });
      await dialog.getByRole("button", { name: "閉じる" }).click();

      alicePhone = await phoneOf(browser, page);
      await alicePhone.goto(`/apps/${app.id}`);
      await alicePhone.getByRole("button", { name: "共有" }).click();
      const phoneDialog = alicePhone.getByRole("dialog");
      await expect(phoneDialog.getByTestId("access-conditional").first()).toBeVisible({ timeout: 20_000 });
      await phoneDialog.getByTestId("access-report").scrollIntoViewIfNeeded();
      await noSideScroll(alicePhone);
      if (SHOTS) await alicePhone.screenshot({ path: `${SHOTS}/paths-share-phone.png` });
    } finally {
      await cleanupAll(
        async () => alicePhone?.context().close(),
        () => bob.context().close(),
      );
    }
  });

  test("毎回動く繰り返しの中で 1 件ずつテーブルに足すなら、追加の権限が無い使う人のボタンを押す前に止める（GAP-97）", async ({
    page,
    browser,
  }) => {
    test.setTimeout(240_000);
    await page.setViewportSize({ width: 1440, height: 900 });
    const bob = await personOn(browser, "bob");
    let bobPhone: Page | null = null;
    try {
      await loginViaKeycloak(page);
      const bobId = await meId(bob);
      const source = await createTable(page, uniqueName("e2e-元の一覧"));
      const copiesName = uniqueName("e2e-写し");
      const copies = await createTable(page, copiesName);
      for (const title of ["一件目", "二件目"]) {
        await api(page, "POST", `/data/tables/${source.id}/records`, { data: { [source.titleKey]: title } });
      }
      await shareTable(page, source.id, bobId, "viewer");
      // 一覧を読み、1 件ずつ写しのテーブルに足す（bob は写しのテーブルの権限を持たない）。
      const flow = uniqueName("map-flow");
      const flowId = await createFlow(page, {
        ir_version: 1,
        name: flow,
        display_name: flow,
        declared_scopes: ["data.read", "data.write"],
        triggers: [{ kind: "interactive" }],
        nodes: [
          { id: "find", type: "data.query", label: "一覧を読む", params: { table: source.id } },
          {
            id: "each",
            type: "control.map",
            label: "1 件ずつ",
            params: { items: { $from: "nodes.find.output", path: "/items" } },
          },
          {
            id: "add",
            type: "data.record.create",
            label: "写しに足す",
            parent: "each",
            params: { table: copies.id, data: { [copies.titleKey]: "写し" } },
          },
        ],
        edges: [{ from: "find", to: "each" }],
      });
      const screen = `# 写し\n\n\`\`\`ui\napp 一覧の写し\nbutton 写す -> workflow ${flow}\n\`\`\`\n`;
      const app = await api(page, "POST", "/mini-apps", {
        name: uniqueName("map-app"),
        body: { description: "一覧の写し", screen, workflows: [{ alias: flow, artifact_id: flowId, version: 1 }] },
      });
      cleanup.add(() => deleteArtifactApi(page, app.id));
      await api(page, "POST", `/mini-apps/${app.id}/publish`, { version: 1 });
      await shareArtifact(page, app.id, bobId);

      // 使う人: 繰り返しの中の追加の権限が無いので、押す前に止めて理由を言う。
      await bob.goto(`/apps/${app.id}`);
      const figure = bob.getByTestId("ui-figure");
      await expect(figure.getByTestId("ui-figure-blocked")).toContainText(
        "「写す」はいまは使えません。このボタンが使うテーブルが見つからないか、レコードを追加する権限がありません。アプリを作成した人に頼んでください。",
        { timeout: 20_000 },
      );
      if (SHOTS) await bob.screenshot({ path: `${SHOTS}/map-user-blocked-pc.png` });
      bobPhone = await phoneOf(browser, bob);
      await bobPhone.goto(`/apps/${app.id}`);
      await expect(bobPhone.getByTestId("ui-figure").getByTestId("ui-figure-blocked")).toContainText("「写す」はいまは使えません。", {
        timeout: 20_000,
      });
      await noSideScroll(bobPhone);
      if (SHOTS) await bobPhone.screenshot({ path: `${SHOTS}/map-user-blocked-phone.png` });

      // 作る人: 写しのテーブルへの追加は毎回要る（「場合によって要る」を付けない）。
      await page.goto(`/apps/${app.id}`);
      await page.getByRole("button", { name: "共有" }).click();
      const dialog = page.getByRole("dialog");
      const needs = dialog.getByTestId("access-report-needs");
      await expect(needs).toContainText(`テーブル「${copiesName}」にレコードを追加する`, { timeout: 20_000 });
      await expect(needs.getByTestId("access-conditional")).toHaveCount(0);
      await expect(dialog.getByTestId("access-report-conditional-note")).toHaveCount(0);
      if (SHOTS) await page.screenshot({ path: `${SHOTS}/map-share-pc.png` });
      await dialog.getByRole("button", { name: "閉じる" }).click();

      // 写しのテーブルを「追加だけ」で共有すると、開いたままの画面で押したときに確かめ直して動き、2 件足す。
      await shareTable(page, copies.id, bobId, "submitter");
      // 押したときの確かめ直しは、前の確かめが 2 秒より新しければ使い回す（二度押しで問い直さない・use-my-access.ts の
      // RECHECK_ON_PRESS_MS）。速い機械では画面を開いてから 2 秒が経たないうちに押し直すので、その間を空ける。
      await bob.waitForTimeout(2_100);
      await figure.getByRole("button", { name: "写す" }).click();
      await figure.getByRole("group", { name: "実行の確認" }).getByRole("button", { name: "実行する" }).click();
      await expect(figure.getByRole("status").filter({ hasText: "起動しました" })).toBeVisible({ timeout: 15_000 });
      await expect(figure.getByTestId("ui-figure-blocked")).toHaveCount(0);
      await expect
        .poll(
          async () => {
            const res = await bob.request.get(`/api/mini-apps/${app.id}/runs`);
            const runs = ((await res.json()) as { runs: { status: string }[] }).runs;
            return runs[0]?.status ?? "none";
          },
          { timeout: 60_000 },
        )
        .toBe("succeeded");
      const records = (await (await page.request.get(`/api/data/tables/${copies.id}/records`)).json()) as { items: unknown[] };
      expect(records.items).toHaveLength(2);
    } finally {
      await cleanupAll(
        async () => bobPhone?.context().close(),
        () => bob.context().close(),
      );
    }
  });

  test("止まった表は画面に戻ったときに確かめ直して出し、確かめ直しは間隔を空けて 1 つずつ問う", async ({ page, browser }) => {
    test.setTimeout(180_000);
    await page.setViewportSize({ width: 1440, height: 900 });
    const bob = await personOn(browser, "bob");
    try {
      await loginViaKeycloak(page);
      const bobId = await meId(bob);
      const ledger = await createTable(page, uniqueName("e2e-台帳"));
      const view = await api(page, "POST", "/data/views", { name: uniqueName("e2e-台帳の一覧"), table_id: ledger.id, query: {} });
      cleanup.add(() => deleteArtifactApi(page, view.id));
      const app = await api(page, "POST", "/mini-apps", {
        name: uniqueName("recheck-app"),
        body: {
          description: "台帳",
          screen: "# 台帳\n\n```data_grid\nledger\n```\n",
          data_views: [{ alias: "ledger", artifact_id: view.id, version: view.version ?? 1 }],
        },
      });
      cleanup.add(() => deleteArtifactApi(page, app.id));
      await api(page, "POST", `/mini-apps/${app.id}/publish`, { version: 1 });
      await shareArtifact(page, app.id, bobId);

      let checks = 0;
      bob.on("request", (req) => {
        if (new URL(req.url()).pathname === `/api/mini-apps/${app.id}/access`) checks += 1;
      });
      await bob.goto(`/apps/${app.id}`);
      await expect(bob.getByTestId("miniapp-data-grid-blocked-ledger")).toBeVisible({ timeout: 20_000 });
      const opened = Date.now();
      await bob.waitForLoadState("networkidle");
      const first = checks;
      expect(first).toBeGreaterThan(0);
      // 開いてすぐ画面に戻っても、間隔より短いので問わない。
      await bob.evaluate(() => window.dispatchEvent(new Event("focus")));
      await bob.waitForTimeout(500);
      expect(checks).toBe(first);

      // 作る人が台帳を見られるように共有する。間隔を過ぎてから画面に戻ると、確かめ直して表を出す。2 回続けて戻っても
      // 問うのは 1 回だけ。
      await shareTable(page, ledger.id, bobId, "viewer");
      await bob.waitForTimeout(Math.max(0, 10_500 - (Date.now() - opened)));
      await bob.evaluate(() => {
        window.dispatchEvent(new Event("focus"));
        window.dispatchEvent(new Event("focus"));
      });
      await expect(bob.getByTestId("miniapp-data-grid-ledger")).toBeVisible({ timeout: 20_000 });
      await bob.waitForLoadState("networkidle");
      expect(checks).toBe(first + 1);
      // 止めるものが無くなったら、画面に戻っても問わない。
      await bob.waitForTimeout(10_500);
      await bob.evaluate(() => window.dispatchEvent(new Event("focus")));
      await bob.waitForTimeout(500);
      expect(checks).toBe(first + 1);
    } finally {
      await bob.context().close();
    }
  });

  test("前もって確かめられないブロックは名前を挙げ、確かめられた範囲でだけ全員使えると言う", async ({ page, browser }) => {
    test.setTimeout(180_000);
    await page.setViewportSize({ width: 1440, height: 900 });
    const bob = await personOn(browser, "bob");
    let alicePhone: Page | null = null;
    try {
      await loginViaKeycloak(page);
      const bobId = await meId(bob);
      const tableName = uniqueName("e2e-台帳");
      const ledger = await createTable(page, tableName);
      await shareTable(page, ledger.id, bobId, "viewer");
      const flow = uniqueName("gaps-flow");
      const flowId = await createFlow(page, {
        ir_version: 1,
        name: flow,
        display_name: flow,
        declared_scopes: ["data.read", "data.write", "storage.read", "storage.write", "rag.query"],
        triggers: [{ kind: "interactive" }],
        nodes: [
          { id: "q", type: "data.query", label: "台帳を読む", params: { table: ledger.id } },
          { id: "calc", type: "script.run", label: "金額を足す", params: { source: { inline: "function main(i){ return i; }" } } },
          { id: "keep", type: "storage.write", label: "控えを保存", params: { name: "控え.txt", content: "x" } },
          { id: "find", type: "rag.search", label: "規程を探す", params: { query: "出張の規程" } },
          { id: "ls", type: "storage.list", label: "トップを一覧", params: {} },
        ],
        edges: [
          { from: "q", to: "calc" },
          { from: "calc", to: "keep" },
          { from: "keep", to: "find" },
          { from: "find", to: "ls" },
        ],
      });
      await shareArtifact(page, flowId, bobId);

      await page.goto(`/workflows/${flowId}`);
      await page.getByTestId("workflow-share").click();
      const dialog = page.getByRole("dialog");
      const gaps = dialog.getByTestId("access-report-unchecked");
      await expect(gaps).toContainText("前もって確かめられないブロックがあります。使う人によって、失敗したり、使える文書やファイルが変わったりすることがあります。", {
        timeout: 20_000,
      });
      await expect(gaps).toContainText("「金額を足す」: スクリプトがテーブルやファイルなどを使うかは、動くまで分かりません。");
      await expect(gaps).toContainText("「控えを保存」: ドライブのトップに保存できるのは、組織のメンバーだけです。");
      // 文書検索とフォルダーを選ばない一覧も、AI エージェントと同じ理由（使う人が見られるものだけを使う）で挙げる。
      await expect(gaps).toContainText("「規程を探す」: 使う人が見られる文書だけを検索します。");
      await expect(gaps).toContainText("「トップを一覧」: 使う人が見られるファイルとフォルダーだけを一覧にします。");
      await expect(gaps.getByRole("listitem")).toHaveCount(4);
      await expect(dialog.getByTestId("access-report-all-ok")).toHaveText(
        "確かめられた範囲では、共有している相手は全員、要る権限を持っています。",
      );
      if (SHOTS) await page.screenshot({ path: `${SHOTS}/gaps-share-pc.png` });
      await dialog.getByRole("button", { name: "閉じる" }).click();

      alicePhone = await phoneOf(browser, page);
      await alicePhone.goto(`/workflows/${flowId}`);
      await alicePhone.getByTestId("chrome-menu").click();
      await alicePhone.getByRole("menuitem", { name: "共有" }).click();
      const phoneGaps = alicePhone.getByRole("dialog").getByTestId("access-report-unchecked");
      await expect(phoneGaps).toContainText("「控えを保存」", { timeout: 20_000 });
      await phoneGaps.scrollIntoViewIfNeeded();
      await noSideScroll(alicePhone);
      if (SHOTS) await alicePhone.screenshot({ path: `${SHOTS}/gaps-share-phone.png` });
    } finally {
      await cleanupAll(
        async () => alicePhone?.context().close(),
        () => bob.context().close(),
      );
    }
  });

  test("足りない相手がいる公開の確認でも、前もって確かめられないブロックがあると言う", async ({ page, browser }) => {
    test.setTimeout(180_000);
    await page.setViewportSize({ width: 1440, height: 900 });
    const bob = await personOn(browser, "bob");
    let alicePhone: Page | null = null;
    try {
      await loginViaKeycloak(page);
      const bobId = await meId(bob);
      const claims = await createTable(page, uniqueName("e2e-申請"));
      await shareTable(page, claims.id, bobId, "submitter");
      const hidden = await createTable(page, uniqueName("e2e-経理"));
      const addTo = (name: string, table: { id: string; titleKey: string }, label: string, then?: Record<string, unknown>) => ({
        ir_version: 1,
        name,
        display_name: name,
        declared_scopes: ["data.write"],
        triggers: [{ kind: "interactive" }],
        nodes: [
          {
            id: "add",
            type: "data.record.create",
            label,
            params: { table: table.id, data: { [table.titleKey]: { $from: "input", path: "/title" } } },
          },
          ...(then ? [then] : []),
        ],
        edges: then ? [{ from: "add", to: then.id }] : [],
      });
      // v1 は申請に足すだけ（bob は足せる）。v2 で、経理に足してからスクリプトで整えるボタンを足す（bob は経理に
      // 足せない。スクリプトはテーブルに書けるので、前もって確かめられない）。
      const flow = uniqueName("publish-flow");
      const flowId = await createFlow(page, addTo(flow, claims, "申請に足す"));
      const flow2 = uniqueName("publish-gaps");
      const flow2Id = await createFlow(
        page,
        addTo(flow2, hidden, "経理に足す", {
          id: "tidy",
          type: "script.run",
          label: "控えを整える",
          params: { source: { inline: "function main(i){ return i; }" } },
        }),
      );
      const screen = (withSecond: boolean) =>
        `# 経費\n\n\`\`\`ui\napp 経費の申請\n件名 = text 必須 as title\nbutton 申請する -> workflow ${flow}\n${
          withSecond ? `button 経理に回す -> workflow ${flow2}\n` : ""
        }\`\`\`\n`;
      const body = { description: "経費の申請", screen: screen(false), workflows: [{ alias: flow, artifact_id: flowId, version: 1 }] };
      const app = await api(page, "POST", "/mini-apps", { name: uniqueName("publish-app"), body });
      cleanup.add(() => deleteArtifactApi(page, app.id));
      await api(page, "POST", `/mini-apps/${app.id}/publish`, { version: 1 });
      await shareArtifact(page, app.id, bobId);
      await api(page, "PUT", `/mini-apps/${app.id}`, {
        expected_version: 1,
        body: {
          ...body,
          screen: screen(true),
          workflows: [...body.workflows, { alias: flow2, artifact_id: flow2Id, version: 1 }],
        },
      });

      const openPublish = async (p: Page) => {
        await p.goto(`/apps/${app.id}`);
        await p.getByTestId("miniapp-publish-bar").getByRole("button", { name: "下書きを試す" }).click();
        await p.getByTestId("miniapp-publish").click();
        const dialog = p.getByTestId("miniapp-publish-dialog");
        await expect(dialog.getByTestId("access-report")).toContainText(
          "共有している相手のうち 1 件は、このバージョンの一部を使えません。",
          { timeout: 20_000 },
        );
        return dialog;
      };
      const publish = await openPublish(page);
      await expect(publish.getByTestId("access-report-missing")).toContainText(`ワークフロー「${flow2}」: テーブル「`);
      await expect(publish.getByTestId("access-report-publish-unchecked")).toHaveText(
        "ほかに、前もって確かめられないブロックがあります。",
      );
      await expect(publish.getByTestId("access-report")).toContainText(
        "公開はできます。相手ごとの直し方と、そのブロックの名前は、共有の画面で見られます。",
      );
      await expect(publish.getByRole("button", { name: "公開", exact: true })).toBeEnabled();
      if (SHOTS) await page.screenshot({ path: `${SHOTS}/gaps-publish-missing-pc.png` });
      await publish.getByRole("button", { name: "キャンセル" }).click();

      alicePhone = await phoneOf(browser, page);
      const phonePublish = await openPublish(alicePhone);
      const phoneGaps = phonePublish.getByTestId("access-report-publish-unchecked");
      await expect(phoneGaps).toBeVisible();
      await phoneGaps.scrollIntoViewIfNeeded();
      await noSideScroll(alicePhone);
      if (SHOTS) await alicePhone.screenshot({ path: `${SHOTS}/gaps-publish-missing-phone.png` });
      await phonePublish.getByRole("button", { name: "キャンセル" }).click();
    } finally {
      await cleanupAll(
        async () => alicePhone?.context().close(),
        () => bob.context().close(),
      );
    }
  });

  test("共有されたワークフローの「いま実行する」は、足りない権限を押す前に言い、共有されると開き直さずに実行できる", async ({
    page,
    browser,
  }) => {
    test.setTimeout(180_000);
    await page.setViewportSize({ width: 1440, height: 900 });
    const bob = await personOn(browser, "bob");
    let bobPhone: Page | null = null;
    try {
      await loginViaKeycloak(page);
      const bobId = await meId(bob);
      const claims = await createTable(page, uniqueName("e2e-申請"));
      const flow = uniqueName("run-access");
      const flowId = await createFlow(page, {
        ir_version: 1,
        name: flow,
        display_name: flow,
        declared_scopes: ["data.write"],
        triggers: [{ kind: "interactive" }],
        input_schema: { type: "object", properties: { title: { type: "string", title: "件名" } } },
        nodes: [
          {
            id: "add",
            type: "data.record.create",
            label: "申請に足す",
            params: { table: claims.id, data: { [claims.titleKey]: { $from: "input", path: "/title" } } },
          },
        ],
        edges: [],
      });
      await shareArtifact(page, flowId, bobId);
      const runCount = async () => {
        const res = await page.request.get(`/api/workflows/${flowId}/runs`);
        return ((await res.json()) as { items: unknown[] }).items.length;
      };

      // スマホ: ダイアログに理由が出て、横にはみ出さない。
      bobPhone = await phoneOf(browser, bob);
      await bobPhone.goto(`/workflows/${flowId}`);
      await bobPhone.getByRole("button", { name: "実行", exact: true }).click();
      const phoneDialog = bobPhone.getByRole("dialog", { name: "いま実行する" });
      await expect(phoneDialog.getByTestId("run-now-access")).toBeVisible({ timeout: 20_000 });
      await noSideScroll(bobPhone);
      if (SHOTS) await bobPhone.screenshot({ path: `${SHOTS}/run-access-phone.png` });

      await bob.goto(`/workflows/${flowId}`);
      await bob.getByRole("button", { name: "実行", exact: true }).click();
      const dialog = bob.getByRole("dialog", { name: "いま実行する" });
      await expect(dialog.getByTestId("run-now-access")).toHaveText(
        "このワークフローが使うテーブルが見つからないか、レコードを追加する権限がありません。ワークフローを作成した人に頼んでください。共有してもらった後は、そのまま実行できます。",
        { timeout: 20_000 },
      );
      await dialog.getByRole("textbox", { name: "件名" }).fill("出張");
      await dialog.getByRole("button", { name: "実行する" }).click();
      await expect(dialog.getByTestId("run-now-failure")).toContainText("足りない権限が、まだ共有されていません。", { timeout: 15_000 });
      expect(await runCount()).toBe(0);
      if (SHOTS) await bob.screenshot({ path: `${SHOTS}/run-access-pc.png` });

      // 作る人が申請のテーブルを「追加だけ」で共有すると、開いたままのダイアログから実行できる。
      await shareTable(page, claims.id, bobId, "submitter");
      await dialog.getByRole("button", { name: "実行する" }).click();
      await bob.waitForURL(/\/runs\?run=/, { timeout: 30_000 });
      expect(await runCount()).toBe(1);
    } finally {
      await cleanupAll(
        async () => bobPhone?.context().close(),
        () => bob.context().close(),
      );
    }
  });

  test("持ち主の「いま実行する」は、持ち主かの問い合わせに失敗しても、作成した人に頼めとは言わない（BUG-230）", async ({
    page,
  }) => {
    test.setTimeout(120_000);
    await page.setViewportSize({ width: 1440, height: 900 });
    await loginViaKeycloak(page);
    // 持ち主が書き先のテーブルを消した（持ち主にも足りない権限がある）ワークフロー。
    const claims = await createTable(page, uniqueName("e2e-申請"));
    const flow = uniqueName("owner-run-access");
    const flowId = await createFlow(page, {
      ir_version: 1,
      name: flow,
      display_name: flow,
      declared_scopes: ["data.write"],
      triggers: [{ kind: "interactive" }],
      nodes: [
        {
          id: "add",
          type: "data.record.create",
          label: "申請に足す",
          params: { table: claims.id, data: { [claims.titleKey]: "出張" } },
        },
      ],
      edges: [],
    });
    await deleteTableApi(page, claims.id);
    // 画面が別に問う持ち主の答え（ワークフローの情報）が届かない。確かめの答えだけで、頼む先を言い分ける。
    await page.route(`**/api/artifacts/${flowId}`, (route) =>
      route.request().method() === "GET" ? route.fulfill({ status: 503, body: "" }) : route.fallback(),
    );
    await page.goto(`/workflows/${flowId}`);
    await page.getByRole("button", { name: "実行", exact: true }).click();
    const dialog = page.getByRole("dialog", { name: "いま実行する" });
    await expect(dialog.getByTestId("run-now-access")).toHaveText(
      "このワークフローが使うテーブルが見つからないか、レコードを追加する権限がありません。テーブルの持ち主に共有を頼むか、ブロックの設定で選び直してください。共有してもらった後は、そのまま実行できます。",
      { timeout: 20_000 },
    );
    if (SHOTS) await page.screenshot({ path: `${SHOTS}/run-access-owner-pc.png` });
    await dialog.getByRole("button", { name: "閉じる" }).click();
  });

  test("失敗を「エラー時」の出口で扱うブロックの権限では、ボタンも「いま実行する」も止めず、作った人の返答で終わる", async ({
    page,
    browser,
  }) => {
    test.setTimeout(240_000);
    await page.setViewportSize({ width: 1440, height: 900 });
    const bob = await personOn(browser, "bob");
    try {
      await loginViaKeycloak(page);
      const bobId = await meId(bob);
      const claimsName = uniqueName("e2e-申請");
      const claims = await createTable(page, claimsName);
      // 申請に足し、足せなければ（bob は申請のテーブルの権限を持たない）誤りの返答を出す（ir.md §7.10 の置き方）。
      const flow = uniqueName("handled-flow");
      const flowId = await createFlow(page, {
        ir_version: 1,
        name: flow,
        display_name: flow,
        declared_scopes: ["data.write"],
        triggers: [{ kind: "interactive" }],
        input_schema: { type: "object", properties: { title: { type: "string", title: "件名" } } },
        nodes: [
          {
            id: "add",
            type: "data.record.create",
            label: "申請に足す",
            on_error: "continue",
            params: { table: claims.id, data: { [claims.titleKey]: { $from: "input", path: "/title" } } },
          },
          { id: "done", type: "workflow.reply", label: "受付", params: { format: "text", value: "申請しました" } },
          {
            id: "sorry",
            type: "workflow.reply",
            label: "書けない",
            params: { format: "text", tone: "error", value: "申請先に書けません。管理者に連絡してください。" },
          },
        ],
        edges: [
          { from: "add", to: "done" },
          { from: "add", from_port: "error", to: "sorry" },
        ],
      });
      const screen = `# 経費\n\n\`\`\`ui\napp 経費の申請\n件名 = text 必須 as title\nbutton 申請する -> workflow ${flow}\n\`\`\`\n`;
      const app = await api(page, "POST", "/mini-apps", {
        name: uniqueName("handled-app"),
        body: { description: "経費の申請", screen, workflows: [{ alias: flow, artifact_id: flowId, version: 1 }] },
      });
      cleanup.add(() => deleteArtifactApi(page, app.id));
      await api(page, "POST", `/mini-apps/${app.id}/publish`, { version: 1 });
      await shareArtifact(page, app.id, bobId);
      await shareArtifact(page, flowId, bobId);
      const runCount = async () => {
        const res = await page.request.get(`/api/workflows/${flowId}/runs`);
        return ((await res.json()) as { items: unknown[] }).items.length;
      };

      // 使う人（アプリ）: ボタンは止まらず、押すと作った人が用意した誤りの返答で終わる。
      await bob.goto(`/apps/${app.id}`);
      const figure = bob.getByTestId("ui-figure");
      await expect(figure.getByRole("button", { name: "申請する" })).toBeEnabled({ timeout: 20_000 });
      await bob.waitForLoadState("networkidle");
      await expect(figure.getByTestId("ui-figure-blocked")).toHaveCount(0);
      await figure.getByLabel("件名").fill("出張");
      await figure.getByRole("button", { name: "申請する" }).click();
      await figure.getByRole("group", { name: "実行の確認" }).getByRole("button", { name: "実行する" }).click();
      await expect(bob.getByTestId("yui-run-result")).toContainText("申請先に書けません。管理者に連絡してください。", {
        timeout: 60_000,
      });
      // 裏取り: 申請のテーブルには書いていない（作った人が見る）。
      const records = (await (await page.request.get(`/api/data/tables/${claims.id}/records`)).json()) as { items: unknown[] };
      expect(records.items).toHaveLength(0);
      if (SHOTS) await bob.screenshot({ path: `${SHOTS}/handled-user-pc.png` });

      // 使う人（いま実行する）: 理由を出さず、押すと実行する。
      const before = await runCount();
      await bob.goto(`/workflows/${flowId}`);
      await bob.getByRole("button", { name: "実行", exact: true }).click();
      const dialog = bob.getByRole("dialog", { name: "いま実行する" });
      await dialog.getByRole("textbox", { name: "件名" }).fill("出張");
      await bob.waitForLoadState("networkidle");
      await expect(dialog.getByTestId("run-now-access")).toHaveCount(0);
      await dialog.getByRole("button", { name: "実行する" }).click();
      await bob.waitForURL(/\/runs\?run=/, { timeout: 30_000 });
      expect(await runCount()).toBe(before + 1);

      // 作る人: 申請のテーブルの権限は「場合によって要る」と言う。
      await page.goto(`/apps/${app.id}`);
      await page.getByRole("button", { name: "共有" }).click();
      const share = page.getByRole("dialog");
      const needs = share.getByTestId("access-report-needs");
      await expect(needs).toContainText(`テーブル「${claimsName}」にレコードを追加する`, { timeout: 20_000 });
      await expect(needs.getByTestId("access-conditional")).toHaveCount(1);
      await share.getByRole("button", { name: "閉じる" }).click();
    } finally {
      await bob.context().close();
    }
  });

  test("範囲が狭いだけの部品と、自分が追加したレコードだけを出す表では、画面に戻っても確かめ直さない（BUG-232）", async ({
    page,
    browser,
  }) => {
    test.setTimeout(180_000);
    await page.setViewportSize({ width: 1440, height: 900 });
    const bob = await personOn(browser, "bob");
    try {
      await loginViaKeycloak(page);
      const bobId = await meId(bob);
      const ledger = await createTable(page, uniqueName("e2e-台帳"));
      // bob は台帳を「追加だけ」で共有されている。ボタンのワークフローは台帳を読む（自分が追加したものだけ読める）。
      await shareTable(page, ledger.id, bobId, "submitter");
      const flow = uniqueName("limited-flow");
      const flowId = await createFlow(page, {
        ir_version: 1,
        name: flow,
        display_name: flow,
        declared_scopes: ["data.read"],
        triggers: [{ kind: "interactive" }],
        nodes: [{ id: "q", type: "data.query", label: "台帳を読む", params: { table: ledger.id } }],
        edges: [],
      });
      // 表も、自分が追加したレコードだけを出す（作った人がそう決めた落ち着いた状態・BUG-232）。
      const view = await api(page, "POST", "/data/views", { name: uniqueName("e2e-台帳の一覧"), table_id: ledger.id, query: {} });
      cleanup.add(() => deleteArtifactApi(page, view.id));
      const screen = `\`\`\`ui\napp 台帳\nbutton 読む -> workflow ${flow}\n\`\`\`\n\n\`\`\`data_grid\nledger\n\`\`\`\n`;
      const app = await api(page, "POST", "/mini-apps", {
        name: uniqueName("limited-app"),
        body: {
          description: "台帳",
          screen,
          workflows: [{ alias: flow, artifact_id: flowId, version: 1 }],
          data_views: [{ alias: "ledger", artifact_id: view.id, version: view.version ?? 1 }],
        },
      });
      cleanup.add(() => deleteArtifactApi(page, app.id));
      await api(page, "POST", `/mini-apps/${app.id}/publish`, { version: 1 });
      await shareArtifact(page, app.id, bobId);

      let checks = 0;
      const seen: string[] = [];
      bob.on("request", (req) => {
        if (new URL(req.url()).pathname === `/api/mini-apps/${app.id}/access`) checks += 1;
      });
      bob.on("response", async (res) => {
        if (new URL(res.url()).pathname === `/api/mini-apps/${app.id}/access`) seen.push(await res.text());
      });
      await bob.goto(`/apps/${app.id}`);
      const figure = bob.getByTestId("ui-figure");
      await expect(figure.getByRole("button", { name: "読む" })).toBeEnabled({ timeout: 20_000 });
      await bob.waitForLoadState("networkidle");
      // 前提: 確かめの答えに、範囲が狭いだけのワークフローの部品と、自分が追加したレコードだけを出す表がある。
      const answer = JSON.parse(seen[seen.length - 1] ?? "{}") as { parts?: { part: { kind: string }; status: string }[] };
      expect(answer.parts?.map((p) => `${p.part.kind}:${p.status}`).sort()).toEqual(["data_view:limited", "workflow:limited"]);
      await expect(figure.getByTestId("ui-figure-blocked")).toHaveCount(0);
      await expect(bob.getByTestId("miniapp-data-grid-own-only")).toHaveText("あなたが追加したレコードだけを表示しています。");
      const first = checks;
      expect(first).toBeGreaterThan(0);
      // 間隔を過ぎてから画面に戻っても、止めているものが無いので問わない。
      await bob.waitForTimeout(10_500);
      await bob.evaluate(() => {
        window.dispatchEvent(new Event("focus"));
        document.dispatchEvent(new Event("visibilitychange"));
      });
      await bob.waitForTimeout(1_000);
      expect(checks).toBe(first);
    } finally {
      await bob.context().close();
    }
  });

  test("見つからない部品には共有で直るとは言わず、確かめ直してもまだ足りなければそう言う", async ({ page }) => {
    test.setTimeout(180_000);
    await page.setViewportSize({ width: 1440, height: 900 });
    await loginViaKeycloak(page);
    const ledger = await createTable(page, uniqueName("e2e-台帳"));
    const view = await api(page, "POST", "/data/views", { name: uniqueName("e2e-台帳の一覧"), table_id: ledger.id, query: {} });
    cleanup.add(() => deleteArtifactApi(page, view.id));
    const flow = uniqueName("gone-flow");
    const flowId = await createFlow(page, {
      ir_version: 1,
      name: flow,
      display_name: flow,
      declared_scopes: ["data.read"],
      triggers: [{ kind: "interactive" }],
      nodes: [{ id: "q", type: "data.query", label: "台帳を読む", params: { table: ledger.id } }],
      edges: [],
    });
    const app = await api(page, "POST", "/mini-apps", {
      name: uniqueName("gone-app"),
      body: {
        description: "台帳",
        screen: `\`\`\`ui\napp 台帳\nbutton 読む -> workflow ${flow}\n\`\`\`\n\n\`\`\`data_grid\nledger\n\`\`\`\n`,
        workflows: [{ alias: flow, artifact_id: flowId, version: 1 }],
        data_views: [{ alias: "ledger", artifact_id: view.id, version: view.version ?? 1 }],
      },
    });
    cleanup.add(() => deleteArtifactApi(page, app.id));
    await api(page, "POST", `/mini-apps/${app.id}/publish`, { version: 1 });

    // 確かめの答えを差し替える（部品が消えた・本文が壊れたときの答えと、表だけが見られないときの答え）。
    const gone = (version: number) => ({
      version,
      truncated: false,
      parts: [
        { part: { kind: "workflow", alias: flow }, status: "missing", unavailable: true, items: [] },
        { part: { kind: "data_view", alias: "ledger" }, status: "missing", unavailable: true, items: [] },
      ],
    });
    const hidden = (version: number) => ({
      version,
      truncated: false,
      parts: [
        {
          part: { kind: "data_view", alias: "ledger" },
          status: "missing",
          unavailable: false,
          items: [{ object: "table", need: "read", status: "missing", name: null }],
        },
      ],
    });
    let answer: (version: number) => unknown = gone;
    await page.route(`**/api/mini-apps/${app.id}/access?*`, (route) => {
      const version = Number(new URL(route.request().url()).searchParams.get("version") ?? "1");
      return route.fulfill({ status: 200, contentType: "application/json", body: JSON.stringify(answer(version)) });
    });

    await page.goto(`/apps/${app.id}`);
    const blockedButtons = page.getByTestId("ui-figure").getByTestId("ui-figure-blocked");
    await expect(blockedButtons).toContainText("このボタンで動くワークフローが見つかりません。アプリを作成した人に伝えてください。", {
      timeout: 20_000,
    });
    await expect(blockedButtons).not.toContainText("共有してもらった後は");
    const blockedGrid = page.getByTestId("miniapp-data-grid-blocked-ledger");
    await expect(blockedGrid).toContainText("この表のビューが見つかりません。");
    await expect(blockedGrid.getByRole("button", { name: "もう一度確かめる" })).toHaveCount(0);

    // 表だけが見られないとき: 「もう一度確かめる」を置き、確かめ直してもまだなら、まだ共有されていないと言う。
    answer = hidden;
    await page.goto(`/apps/${app.id}`);
    await expect(blockedGrid).toContainText("この表が使うテーブルが見つからないか、見る権限がありません。", { timeout: 20_000 });
    await expect(blockedGrid.getByTestId("miniapp-data-grid-still-blocked-ledger")).toHaveCount(0);
    await blockedGrid.getByRole("button", { name: "もう一度確かめる" }).click();
    await expect(blockedGrid.getByTestId("miniapp-data-grid-still-blocked-ledger")).toHaveText(
      "確かめ直しましたが、まだ共有されていません。",
    );
    if (SHOTS) await page.screenshot({ path: `${SHOTS}/gone-parts-pc.png` });
  });
});
