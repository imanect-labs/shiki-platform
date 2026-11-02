import { test, expect, type Browser, type Page } from "@playwright/test";

import {
  cleanupAll,
  cleanupQueue,
  deleteArtifactApi,
  loginAs,
  loginViaKeycloak,
  removeWorkflowApi,
  trackCreatedResources,
  uniqueName,
} from "./helpers";

/// ミニアプリの下書きと公開（D-16・GAP-36）の E2E。
/// - 保存は下書きのバージョンになり、作る人（editor）は下書きを試せる。共有した人（使う人）には公開したバージョン
///   だけが見え、公開すると使う人の画面が変わる。開いたままの前のバージョンのボタンは、開き直しを促す。
/// - 公開を戻す（前に公開していたバージョンを公開し直す）と、使う人の画面も戻る。
/// - まだ公開していないアプリは、共有した人には「まだ公開されていません」と出る。
/// - スキルを束ねたアプリには「このアプリについて AI に聞く」があり、アプリを付けた会話を始める（D-16 の (b)）。
/// PC（1440px）とスマホ（390px）で確かめる。作ったもの（アプリ・ワークフロー・スキル・会話）は最後に消す。
/// `E2E_SHOTS_DIR` を渡すとスクリーンショットを撮る。

const SHOTS = process.env.E2E_SHOTS_DIR;

const cleanup = cleanupQueue();
test.beforeEach(({ page }) => trackCreatedResources(page, cleanup));
test.afterEach(() => cleanup.run());

async function csrf(page: Page): Promise<string> {
  return (await page.context().cookies()).find((c) => c.name === "shiki_csrf")?.value ?? "";
}

async function api(page: Page, method: "POST" | "PUT" | "DELETE", path: string, body?: unknown) {
  const res = await page.request.fetch(`/api${path}`, {
    method,
    headers: { "Content-Type": "application/json", "X-CSRF-Token": await csrf(page) },
    ...(body === undefined ? {} : { data: body }),
  });
  expect(res.ok(), `${method} ${path} → ${res.status()} ${await res.text()}`).toBeTruthy();
  return res;
}

/// 手動実行の最小のワークフローを作る（最後に消す）。
async function createFlow(page: Page, name: string): Promise<string> {
  const res = await api(page, "POST", "/workflows", {
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
  const id = ((await res.json()) as { id: string }).id;
  cleanup.add(() => removeWorkflowApi(page, id));
  return id;
}

/// スキルを作る（最後に消す）。
async function createSkill(page: Page): Promise<string> {
  const res = await api(page, "POST", "/skills", {
    name: uniqueName("expense-guide"),
    body: { description: "経費の案内", instructions: "経費の規程の質問に答える。" },
  });
  const id = ((await res.json()) as { id: string }).id;
  cleanup.add(() => deleteArtifactApi(page, id));
  return id;
}

function screen(title: string, flow: string): string {
  return `${title}画面です。\n\n\`\`\`ui\napp 経費の申請\n金額 = number 必須 円 as amount\nbutton 申請する -> workflow ${flow}\n\`\`\`\n`;
}

/// アプリを作る（最後に消す）。
async function createApp(page: Page, body: Record<string, unknown>): Promise<string> {
  const res = await api(page, "POST", "/mini-apps", { name: uniqueName("draft-app"), body });
  const id = ((await res.json()) as { id: string }).id;
  cleanup.add(() => deleteArtifactApi(page, id));
  return id;
}

async function shareWith(page: Page, appId: string, userId: string, role: "viewer" | "editor") {
  await api(page, "PUT", `/artifacts/${appId}/shares`, { target: { type: "user", id: userId }, role });
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

/// 本体を走らせ、落ちても片付けを順にすべて行う（互いに独立・[`cleanupAll`]）。本体が落ちたら、その誤りを片付けの
/// 誤りで置き換えない。
async function withCleanup(body: () => Promise<void>, ...steps: (() => Promise<unknown>)[]): Promise<void> {
  let failure: { error: unknown } | null = null;
  try {
    await body();
  } catch (error) {
    failure = { error };
  }
  try {
    await cleanupAll(...steps);
  } catch (error) {
    if (failure === null) throw error;
    console.warn("片付けに失敗しました（本体の失敗を先に報告します）", error);
  }
  if (failure !== null) throw failure.error;
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

async function heading(page: Page, text: string) {
  await expect(page.getByRole("heading", { name: text, level: 1 })).toBeVisible({ timeout: 20_000 });
}

async function noSideScroll(page: Page) {
  expect(await page.evaluate(() => document.documentElement.scrollWidth)).toBeLessThanOrEqual(
    page.viewportSize()?.width ?? 390,
  );
}

/// 画面の図に金額を入れて「申請する」→ 確認 → 実行する。
async function pressApply(page: Page, amount: string) {
  const figure = page.getByTestId("ui-figure");
  await expect(figure.getByRole("button", { name: "申請する" })).toBeEnabled({ timeout: 15_000 });
  await figure.getByLabel("金額").fill(amount);
  await figure.getByRole("button", { name: "申請する" }).click();
  await figure.getByRole("group", { name: "実行の確認" }).getByRole("button", { name: "実行する" }).click();
}

/// 公開の帯（または履歴）から開いた確認で公開する。
async function confirmPublish(page: Page, version: number) {
  const dialog = page.getByTestId("miniapp-publish-dialog");
  await expect(dialog.getByRole("heading", { name: `v${version} を公開しますか？` })).toBeVisible({ timeout: 15_000 });
  await expect(dialog.getByRole("button", { name: "公開", exact: true })).toBeEnabled({ timeout: 15_000 });
  await dialog.getByRole("button", { name: "公開", exact: true }).click();
  await expect(page.getByText(`v${version} を公開しました`, { exact: true })).toBeVisible({ timeout: 15_000 });
}

test.describe("ミニアプリの下書きと公開（D-16）", () => {
  test("作る人は下書きを試し、使う人には公開したバージョンだけが見え、公開すると使う人の画面が変わる", async ({
    page,
    browser,
  }) => {
    test.setTimeout(300_000);
    await page.setViewportSize({ width: 1440, height: 900 });
    await loginViaKeycloak(page);
    const flow = uniqueName("draft-flow");
    const flowId = await createFlow(page, flow);
    const skillId = await createSkill(page);
    const appId = await createApp(page, {
      description: "経費の申請",
      screen: screen("今の", flow),
      skill: { artifact_id: skillId, version: 1 },
      workflows: [{ alias: flow, artifact_id: flowId, version: 1 }],
    });
    await api(page, "POST", `/mini-apps/${appId}/publish`, { version: 1 });

    const bob = await personOn(browser, "bob", false);
    let bobPhone: Page | null = null;
    let alicePhone: Page | null = null;
    await withCleanup(
      async () => {
        const bobId = ((await (await bob.request.get("/api/me")).json()) as { id: string }).id;
        await shareWith(page, appId, bobId, "viewer");
        // 使う人: 公開した v1 を開く。公開の帯・編集・バージョン履歴は出ない。スキルがあるので AI に聞ける。
        await bob.goto(`/apps/${appId}`);
        await heading(bob, "経費の申請");
        await expect(bob.getByText("今の画面です。")).toBeVisible();
        await expect(bob.getByTestId("miniapp-publish-bar")).toHaveCount(0);
        await expect(bob.getByRole("button", { name: "編集" })).toHaveCount(0);
        await expect(bob.getByRole("button", { name: "バージョン" })).toHaveCount(0);
        await expect(bob.getByTestId("miniapp-ask-ai")).toBeEnabled();

        // 作る人: 公開中の v1 を開き、編集して保存すると下書き（v2）になり、そのまま試せる。
        await page.goto(`/apps/${appId}`);
        await heading(page, "経費の申請");
        await expect(page.getByTestId("miniapp-publish-state")).toHaveText("公開中");
        await page.getByRole("button", { name: "編集" }).click();
        const editor = page.getByRole("dialog");
        await editor.getByLabel(/画面の見出し/).fill("経費の申請（新しい画面）");
        await editor.getByRole("tab", { name: "書く" }).click();
        await editor.getByRole("textbox", { name: /^画面\*?$/ }).fill(screen("新しい", flow));
        await editor.getByRole("button", { name: "保存" }).click();
        await expect(page.getByText("v2 を下書きとして保存しました", { exact: true })).toBeVisible({ timeout: 15_000 });
        await expect(page.getByText("公開するまで、共有した人には v1 が見えます。", { exact: true })).toBeVisible();
        await heading(page, "経費の申請（新しい画面）");
        await expect(page.getByTestId("miniapp-publish-state")).toHaveText("下書き");
        await expect(page.getByTestId("miniapp-publish-bar")).toContainText("共有した人には公開中の v1 が見えます。");
        await pressApply(page, "1200");
        await expect(page.getByTestId("ui-figure").getByText(/を起動しました/)).toBeVisible({ timeout: 30_000 });
        if (SHOTS) await page.screenshot({ path: `${SHOTS}/drafts-editor-draft-pc.png` });

        // 使う人の画面は、公開するまで変わらない（PC・スマホ）。
        await bob.reload();
        await heading(bob, "経費の申請");
        await expect(bob.getByText("今の画面です。")).toBeVisible();
        bobPhone = await phoneOf(browser, bob);
        await bobPhone.goto(`/apps/${appId}`);
        await heading(bobPhone, "経費の申請");
        await expect(bobPhone.getByTestId("miniapp-publish-bar")).toHaveCount(0);
        await expect(bobPhone.getByTestId("ui-figure").getByRole("button", { name: "申請する" })).toBeVisible({
          timeout: 15_000,
        });
        await noSideScroll(bobPhone);
        if (SHOTS) await bobPhone.screenshot({ path: `${SHOTS}/drafts-viewer-before-phone.png` });

        // 作る人が公開する。確認は公開しているバージョンとの差を言う。
        await page.getByTestId("miniapp-publish-bar").getByRole("button", { name: "公開", exact: true }).click();
        const confirm = page.getByTestId("miniapp-publish-dialog");
        await expect(confirm).toContainText("共有した人の画面とボタンの実行先が、v1 から v2 に変わります。");
        await expect(confirm.getByTestId("miniapp-publish-changes")).toContainText(
          "画面の見出し: 「経費の申請」から「経費の申請（新しい画面）」へ",
        );
        await expect(confirm.getByTestId("miniapp-publish-changes")).toContainText("画面の説明・図・ボタン");
        await expect(confirm.getByTestId("miniapp-publish-undo-note")).toHaveText(
          "前に公開していたバージョンには、あとから戻せます。",
        );
        // 開くときの動き（フェード）が終わってから撮る。
        await page.waitForTimeout(500);
        if (SHOTS) await page.screenshot({ path: `${SHOTS}/drafts-publish-dialog-pc.png` });
        await confirmPublish(page, 2);
        await expect(page.getByTestId("miniapp-publish-state")).toHaveText("公開中");
        await expect(page.getByTestId("miniapp-publish-bar").getByRole("button", { name: "公開を v1 に戻す" })).toBeVisible();

        // 開いたままの前のバージョンのボタンは、開き直しを促す。開き直すと公開した v2 が見える。
        await pressApply(bob, "800");
        await expect(bob.getByTestId("ui-figure")).toContainText("公開しているバージョンが変わりました", {
          timeout: 15_000,
        });
        await bob.reload();
        await heading(bob, "経費の申請（新しい画面）");
        await bobPhone.reload();
        await heading(bobPhone, "経費の申請（新しい画面）");
        await expect(bobPhone.getByText("新しい画面です。")).toBeVisible();
        await expect(bobPhone.getByTestId("ui-figure").getByRole("button", { name: "申請する" })).toBeVisible({
          timeout: 15_000,
        });
        await noSideScroll(bobPhone);
        if (SHOTS) await bobPhone.screenshot({ path: `${SHOTS}/drafts-viewer-after-phone.png` });

        // 公開を戻すと、使う人の画面も戻る。
        await page.getByTestId("miniapp-publish-bar").getByRole("button", { name: "公開を v1 に戻す" }).click();
        await expect(page.getByTestId("miniapp-publish-dialog")).toContainText("v2 から v1 に戻ります。");
        await confirmPublish(page, 1);
        await bob.reload();
        await heading(bob, "経費の申請");
        await expect(bob.getByText("今の画面です。")).toBeVisible();

        // 作る人のスマホ: 公開の帯と確認が収まる（下書きを試してから公開の確認を開き、公開せずに閉じる）。
        alicePhone = await phoneOf(browser, page);
        await alicePhone.goto(`/apps/${appId}`);
        await heading(alicePhone, "経費の申請");
        await alicePhone.getByTestId("miniapp-publish-bar").getByRole("button", { name: "下書きを試す" }).click();
        await heading(alicePhone, "経費の申請（新しい画面）");
        await expect(alicePhone.getByTestId("miniapp-publish-state")).toHaveText("下書き");
        await noSideScroll(alicePhone);
        if (SHOTS) await alicePhone.screenshot({ path: `${SHOTS}/drafts-editor-phone.png` });
        await alicePhone.getByTestId("miniapp-publish-bar").getByRole("button", { name: "公開", exact: true }).click();
        const phoneConfirm = alicePhone.getByTestId("miniapp-publish-dialog");
        await expect(phoneConfirm.getByTestId("miniapp-publish-changes")).toBeVisible({ timeout: 15_000 });
        await alicePhone.waitForTimeout(500);
        await noSideScroll(alicePhone);
        if (SHOTS) await alicePhone.screenshot({ path: `${SHOTS}/drafts-publish-dialog-phone.png` });
        await phoneConfirm.getByRole("button", { name: "キャンセル" }).click();
        await expect(phoneConfirm).toHaveCount(0);

        // 使う人が v1 を開いたままのあいだに、作る人が v2 を公開し直す。「このアプリについて AI に聞く」を押すと、
        // 状態コードではなく、公開しているバージョンが変わったことを言い、公開している v2 を開き直す。
        await api(page, "POST", `/mini-apps/${appId}/publish`, { version: 2 });
        const refused = bob.waitForResponse(
          (r) => r.request().method() === "POST" && new URL(r.url()).pathname === "/api/threads",
        );
        await bob.getByTestId("miniapp-ask-ai").click();
        expect((await refused).status()).toBe(409);
        await expect(bob.getByText("公開しているバージョンが変わりました", { exact: true })).toBeVisible({ timeout: 15_000 });
        await expect(bob.getByText(/API \d+/)).toHaveCount(0);
        await heading(bob, "経費の申請（新しい画面）");
        if (SHOTS) await bob.screenshot({ path: `${SHOTS}/drafts-ask-ai-version-changed-pc.png` });

        // もう一度押すと、公開している v2 を付けた会話が始まる。
        const created = bob.waitForResponse(
          (r) => r.request().method() === "POST" && new URL(r.url()).pathname === "/api/threads",
        );
        await bob.getByTestId("miniapp-ask-ai").click();
        const thread = (await (await created).json()) as { id: string; mini_app_id: string | null; mini_app_version: number | null };
        cleanup.add(async () => {
          const res = await bob.request.delete(`/api/threads/${thread.id}`, { headers: { "X-CSRF-Token": await csrf(bob) } });
          expect([200, 204, 404], `DELETE /threads/${thread.id} → ${res.status()}`).toContain(res.status());
        });
        expect(thread.mini_app_id).toBe(appId);
        expect(thread.mini_app_version).toBe(2);
        await bob.waitForURL(new RegExp(`/c/${thread.id}`), { timeout: 30_000 });
      },
      // 会話の片付けは bob の画面で送るので、画面を閉じる前に済ませる。
      () => cleanup.run(),
      () => bob.context().close(),
      async () => {
        await bobPhone?.context().close();
      },
      async () => {
        await alicePhone?.context().close();
      },
    );
  });

  test("まだ公開していないアプリは、共有した人には「まだ公開されていません」と出る", async ({ page, browser }) => {
    test.setTimeout(180_000);
    await page.setViewportSize({ width: 1440, height: 900 });
    await loginViaKeycloak(page);
    const appId = await createApp(page, {
      description: "会議室の予約",
      screen: "会議室の予約の仕方の案内です。\n",
      workflows: [],
    });
    const bob = await personOn(browser, "bob", false);
    let bobPhone: Page | null = null;
    await withCleanup(
      async () => {
        const bobId = ((await (await bob.request.get("/api/me")).json()) as { id: string }).id;
        // 作る人は共有の前に、まだ公開していないことを知る（一覧のカードでも）。
        const name = ((await (await page.request.get(`/api/artifacts/${appId}`)).json()) as { name: string }).name;
        await page.goto("/apps");
        const card = page.locator("li", { has: page.getByRole("heading", { name }) });
        await expect(card).toContainText("未公開", { timeout: 15_000 });
        await page.goto(`/apps/${appId}`);
        await heading(page, "会議室の予約");
        await expect(page.getByTestId("miniapp-publish-state")).toHaveText("未公開");
        await expect(page.getByTestId("miniapp-publish-bar")).toContainText(
          "まだ公開していません。公開するまで、共有した人はこのアプリを使えません。",
        );
        await page.getByRole("button", { name: "共有" }).click();
        await expect(page.getByRole("dialog").getByTestId("share-dialog-note")).toHaveText(
          "まだ公開していません。公開するまで、共有した相手はこのアプリを使えません。",
        );
        await page.waitForTimeout(500);
        if (SHOTS) await page.screenshot({ path: `${SHOTS}/drafts-unpublished-share-pc.png` });
        await page.getByRole("dialog").getByRole("button", { name: "閉じる" }).click();
        // 初めての公開の確認は、戻す先が無いことを言う（あとから戻せるとは言わない）。
        await page.getByTestId("miniapp-publish-bar").getByRole("button", { name: "公開", exact: true }).click();
        const firstPublish = page.getByTestId("miniapp-publish-dialog");
        await expect(firstPublish).toContainText("共有した人が、このアプリを使えるようになります。");
        await expect(firstPublish.getByTestId("miniapp-publish-undo-note")).toHaveText(
          "公開した後は、公開をやめられません。別のバージョンに切り替えることはできます。",
          { timeout: 15_000 },
        );
        await page.waitForTimeout(500);
        if (SHOTS) await page.screenshot({ path: `${SHOTS}/drafts-first-publish-pc.png` });
        await firstPublish.getByRole("button", { name: "キャンセル" }).click();
        await expect(firstPublish).toHaveCount(0);
        await shareWith(page, appId, bobId, "viewer");

        for (const who of [bob, (bobPhone = await phoneOf(browser, bob))]) {
          await who.goto(`/apps/${appId}`);
          await expect(who.getByText("このアプリはまだ公開されていません")).toBeVisible({ timeout: 20_000 });
          await expect(who.getByText("作成した人が公開すると使えるようになります。")).toBeVisible();
          await noSideScroll(who);
        }
        if (SHOTS) await bobPhone.screenshot({ path: `${SHOTS}/drafts-unpublished-viewer-phone.png` });
        // 公開すると開け、一覧のカードは公開しているバージョンを言う。
        await api(page, "POST", `/mini-apps/${appId}/publish`, { version: 1 });
        await bobPhone.reload();
        await heading(bobPhone, "会議室の予約");
        await page.goto("/apps");
        await expect(card).toContainText("公開中 v1", { timeout: 15_000 });
        // 使う人のカードには、バージョン履歴を出さない（作る人のもの・D-16）。
        await bob.goto("/apps");
        const bobCard = bob.locator("li", { has: bob.getByRole("heading", { name }) });
        await expect(bobCard).toContainText("公開中 v1", { timeout: 15_000 });
        await expect(bobCard.getByRole("button", { name: `${name} のその他の操作` })).toHaveCount(0);
      },
      () => bob.context().close(),
      async () => {
        await bobPhone?.context().close();
      },
    );
  });

  test("前のバージョンの内容を下書きにし、書き損じた図のある下書きは公開できない（D-16 の (c)）", async ({ page }) => {
    test.setTimeout(180_000);
    await page.setViewportSize({ width: 1440, height: 900 });
    await loginViaKeycloak(page);
    const flow = uniqueName("draft-flow");
    const flowId = await createFlow(page, flow);
    const appId = await createApp(page, {
      description: "備品の申請",
      screen: screen("一つ目の", flow),
      workflows: [{ alias: flow, artifact_id: flowId, version: 1 }],
    });
    await api(page, "POST", `/mini-apps/${appId}/publish`, { version: 1 });
    // 書き損じた図（入力の種類の誤り）のある v2 を保存する。保存はでき、注意が返る。
    const saved = await api(page, "PUT", `/mini-apps/${appId}`, {
      body: {
        description: "備品の申請",
        screen: screen("二つ目の", flow).replace("number 必須 円", "numbr 必須 円"),
        workflows: [{ alias: flow, artifact_id: flowId, version: 1 }],
      },
      expected_version: 1,
    });
    expect(((await saved.json()) as { warnings: unknown[] }).warnings).toHaveLength(1);

    // 下書き（v2）を開くと、直す箇所と、公開できないことを言う（公開のボタンは押せない）。
    await page.goto(`/apps/${appId}`);
    await heading(page, "備品の申請");
    await page.getByTestId("miniapp-publish-bar").getByRole("button", { name: "下書きを試す" }).click();
    await expect(page.getByTestId("miniapp-publish-state")).toHaveText("下書き");
    const problems = page.getByTestId("miniapp-publish-problems");
    await expect(problems).toContainText("直す箇所があるため、このバージョンは公開できません。");
    await expect(problems).toContainText("画面の 5 行目（図「経費の申請」）: ");
    await expect(page.getByTestId("miniapp-publish")).toBeDisabled();
    if (SHOTS) await page.screenshot({ path: `${SHOTS}/drafts-broken-figure-pc.png` });

    // バージョン履歴から v1 を開き、その内容で新しい下書き（v3）を作る。公開は v1 のまま。
    await page.getByRole("button", { name: "バージョン" }).click();
    const history = page.getByRole("dialog");
    const v1 = history.getByTestId("version-row").filter({ hasText: "v1" });
    await expect(v1.getByTestId("version-badge")).toHaveText(["公開中"]);
    await expect(history.getByTestId("version-row").filter({ hasText: "v2" }).getByTestId("version-badge")).toHaveText([
      "下書き",
    ]);
    await v1.getByRole("button", { name: "開く" }).click();
    await expect(page.getByTestId("miniapp-publish-state")).toHaveText("公開中");
    await expect(page.getByText("一つ目の画面です。")).toBeVisible();
    // 最新でないバージョンを開いているので「この内容を下書きにする」が出る。
    await page.getByTestId("miniapp-publish-bar").getByRole("button", { name: "この内容を下書きにする" }).click();
    await expect(page.getByText("v1 の内容で v3 を下書きとして保存しました", { exact: true })).toBeVisible({
      timeout: 15_000,
    });
    await expect(page.getByTestId("miniapp-publish-state")).toHaveText("下書き");
    await expect(page.getByText("一つ目の画面です。")).toBeVisible();
    await expect(page.getByTestId("miniapp-publish")).toBeEnabled();
  });
});
