import { test, expect, type Browser, type Page } from "@playwright/test";

import { cleanupAll, loginAs, loginViaKeycloak, uniqueName } from "./helpers";

/// ミニアプリの実行画面の共有ボタンと、共同編集者の保存の誤り（BUG-78・BUG-80）の E2E。
/// - 共有（相手の一覧と付与・解除）は owner だけができるので、共有ボタンは owner にだけ出す（解決の応答の `can_share`）。
///   閲覧・編集の共有を受けた人には出さない。
/// - 共同編集者が自分の読めない部品を束ねたアプリを保存すると止まる（部品の権限の扱いは D-16 で未決）。そのときの誤りは
///   部品を別名と版で言い、部品の id も本文の中の位置（`workflows[0]`）も出さない。
/// - アプリの一覧（/apps）には共有されたアプリも並ぶ。カードのメニューの「共有」と「削除」は owner のアプリにだけ出す。
///   共有の欄が共有中の相手を読めない（403）ときは、空の一覧に見せず「持ち主だけ」と知らせる。
/// 作ったアプリ・ワークフローは最後に消す。`E2E_SHOTS_DIR` を渡すとスクリーンショットを撮る。

const SHOTS = process.env.E2E_SHOTS_DIR;

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
  return ((await res.json()) as { id: string }).id;
}

async function createApp(page: Page, body: Record<string, unknown>): Promise<string> {
  const res = await api(page, "POST", "/mini-apps", { name: uniqueName("share-app"), body });
  return ((await res.json()) as { id: string }).id;
}

async function share(page: Page, appId: string, userId: string, role: "viewer" | "editor") {
  await api(page, "PUT", `/artifacts/${appId}/shares`, { target: { type: "user", id: userId }, role });
}

async function cleanup(page: Page, ids: string[]) {
  for (const id of ids) {
    const res = await page.request.delete(`/api/artifacts/${id}`, { headers: { "X-CSRF-Token": await csrf(page) } });
    expect([204, 404], `DELETE /artifacts/${id} → ${res.status()}`).toContain(res.status());
  }
}

/// bob のタッチ端末（390px）の文脈。ログインは PC の画面で済ませたセッションを持ち込む。
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

test("ミニアプリの共有ボタンは owner にだけ出し、閲覧・編集の共有を受けた人には出さない（BUG-78）", async ({
  page,
  browser,
}) => {
  test.setTimeout(180_000);
  await loginViaKeycloak(page);
  const appId = await createApp(page, {
    description: "備品の貸出の案内",
    screen: "備品の置き場所と借り方の案内です。\n",
    workflows: [],
  });
  const bobCtx = await browser.newContext({ viewport: { width: 1440, height: 900 } });
  const bob = await bobCtx.newPage();
  let phone: Page | null = null;
  try {
    // 持ち主（owner）には出る。解決の応答も owner だと言う。
    await page.setViewportSize({ width: 1440, height: 900 });
    await page.goto(`/apps/${appId}`);
    await expect(page.getByRole("heading", { name: "備品の貸出の案内", level: 1 })).toBeVisible({ timeout: 15_000 });
    await expect(page.getByRole("button", { name: "共有" })).toBeVisible();
    const own = (await (await page.request.get(`/api/mini-apps/${appId}/resolved`)).json()) as {
      can_edit: boolean;
      can_share: boolean;
    };
    expect(own).toMatchObject({ can_edit: true, can_share: true });
    if (SHOTS) await page.screenshot({ path: `${SHOTS}/bug78-owner-pc.png` });

    await loginAs(bob, "bob");
    const bobId = ((await (await bob.request.get("/api/me")).json()) as { id: string }).id;

    // 閲覧の共有: 使えるが、編集も共有もできない。ボタンはどちらも出ない（版は出る）。
    await share(page, appId, bobId, "viewer");
    await bob.goto(`/apps/${appId}`);
    await expect(bob.getByRole("heading", { name: "備品の貸出の案内", level: 1 })).toBeVisible({ timeout: 15_000 });
    await expect(bob.getByRole("button", { name: "バージョン" })).toBeVisible();
    await expect(bob.getByRole("button", { name: "共有" })).toHaveCount(0);
    await expect(bob.getByRole("button", { name: "編集" })).toHaveCount(0);
    const viewer = (await (await bob.request.get(`/api/mini-apps/${appId}/resolved`)).json()) as {
      can_edit: boolean;
      can_share: boolean;
    };
    expect(viewer).toMatchObject({ can_edit: false, can_share: false });
    // 共有の一覧は owner が要る（ボタンを出さない理由・サーバは 403 で断る）。
    expect((await bob.request.get(`/api/artifacts/${appId}/shares`)).status()).toBe(403);

    // 編集の共有: 編集ボタンは出るが、共有ボタンは出ない（共有は owner だけ）。
    await share(page, appId, bobId, "editor");
    await bob.reload();
    await expect(bob.getByRole("button", { name: "編集" })).toBeVisible({ timeout: 15_000 });
    await expect(bob.getByRole("button", { name: "バージョン" })).toBeVisible();
    await expect(bob.getByRole("button", { name: "共有" })).toHaveCount(0);
    if (SHOTS) await bob.screenshot({ path: `${SHOTS}/bug78-editor-pc.png` });

    // スマホ（右上のボタン）でも同じ。
    phone = await phoneOf(browser, bob);
    await phone.goto(`/apps/${appId}`);
    await expect(phone.getByRole("heading", { name: "備品の貸出の案内", level: 1 })).toBeVisible({ timeout: 15_000 });
    await expect(phone.getByRole("button", { name: "編集" })).toBeVisible();
    await expect(phone.getByRole("button", { name: "共有" })).toHaveCount(0);
    expect(await phone.evaluate(() => document.documentElement.scrollWidth)).toBeLessThanOrEqual(390);
    if (SHOTS) await phone.screenshot({ path: `${SHOTS}/bug78-editor-phone.png` });
  } finally {
    await cleanup(page, [appId]);
    await bobCtx.close().catch(() => undefined);
    await phone?.context().close().catch(() => undefined);
  }
});

test("共同編集者が読めないワークフローを束ねたアプリを保存すると、部品を名前と版で言い、id を出さない（BUG-80）", async ({
  page,
  browser,
}) => {
  test.setTimeout(180_000);
  await loginViaKeycloak(page);
  const flow = uniqueName("lend-flow");
  const wfId = await createFlow(page, flow);
  const appId = await createApp(page, {
    description: "備品の貸出",
    screen: `# 貸出の受付\n\n\`\`\`ui\napp 貸出の受付\n備品 = text 必須 as item\nbutton 借りる -> workflow ${flow}\n\`\`\`\n`,
    workflows: [{ alias: flow, artifact_id: wfId, version: 1 }],
  });
  const bobCtx = await browser.newContext({ viewport: { width: 1440, height: 900 } });
  const bob = await bobCtx.newPage();
  let phone: Page | null = null;
  const expected = `ワークフロー「${flow}」（v1）が見つからないか、あなたには読めません`;
  try {
    await loginAs(bob, "bob");
    const bobId = ((await (await bob.request.get("/api/me")).json()) as { id: string }).id;
    // アプリだけを編集で共有する（束ねたワークフローは共有しない）。
    await share(page, appId, bobId, "editor");

    await bob.goto(`/apps/${appId}`);
    await expect(bob.getByRole("heading", { name: "備品の貸出", level: 1 })).toBeVisible({ timeout: 15_000 });
    await bob.getByRole("button", { name: "編集" }).click();
    const dialog = bob.getByRole("dialog");
    await dialog.getByLabel(/画面の見出し/).fill("備品の貸出（改訂）");
    await dialog.getByRole("button", { name: "保存" }).click();
    const alert = dialog.getByRole("alert");
    await expect(alert).toContainText(expected, { timeout: 15_000 });
    // 部品の id・種類の内部名・本文の中の位置（workflows[0]）を出さない。
    await expect(alert).toHaveText(expected);
    await expect(alert).not.toContainText(wfId);
    await expect(alert).not.toContainText("kind=");
    await expect(alert).not.toContainText("workflows[0]");
    await alert.scrollIntoViewIfNeeded();
    if (SHOTS) await bob.screenshot({ path: `${SHOTS}/bug80-collaborator-pc.png` });

    // スマホのシートでも同じ文言で、横にはみ出さない。
    phone = await phoneOf(browser, bob);
    await phone.goto(`/apps/${appId}`);
    await expect(phone.getByRole("heading", { name: "備品の貸出", level: 1 })).toBeVisible({ timeout: 15_000 });
    await phone.getByRole("button", { name: "編集" }).click();
    const sheet = phone.getByRole("dialog");
    await sheet.getByRole("button", { name: "保存" }).click();
    const phoneAlert = sheet.getByRole("alert");
    await expect(phoneAlert).toContainText(expected, { timeout: 15_000 });
    await expect(phoneAlert).toHaveText(expected);
    await phoneAlert.scrollIntoViewIfNeeded();
    expect(await sheet.evaluate((el) => el.scrollWidth <= el.clientWidth + 1)).toBe(true);
    if (SHOTS) await phone.screenshot({ path: `${SHOTS}/bug80-collaborator-phone.png` });

    // 保存は止まったまま（版は増えない）。
    const resolved = (await (await page.request.get(`/api/mini-apps/${appId}/resolved`)).json()) as { version: number };
    expect(resolved.version).toBe(1);
  } finally {
    await cleanup(page, [appId, wfId]);
    await bobCtx.close().catch(() => undefined);
    await phone?.context().close().catch(() => undefined);
  }
});

test("共同編集者が読めないワークフローを束ねたアプリを開くと、入力が無いとは言わず、読めないと言う（BUG-122）", async ({
  page,
  browser,
}) => {
  test.setTimeout(180_000);
  await page.setViewportSize({ width: 1440, height: 900 });
  await loginViaKeycloak(page);
  const flow = uniqueName("lend-flow");
  const wfId = await createFlow(page, flow);
  const appId = await createApp(page, {
    description: "備品の貸出",
    screen: `# 貸出の受付\n\n\`\`\`ui\napp 貸出の受付\n備品 = text 必須 as item\nbutton 借りる -> workflow ${flow}\n\`\`\`\n`,
    workflows: [{ alias: flow, artifact_id: wfId, version: 1 }],
  });
  const bobCtx = await browser.newContext({ viewport: { width: 1440, height: 900 } });
  const bob = await bobCtx.newPage();
  const note = `ワークフロー「${flow}」が見つかりません。削除されたか、見る権限がありません。`;
  try {
    // 持ち主（ワークフローを読める）には、読めないという注記を出さない。
    await page.goto(`/apps/${appId}`);
    await page.getByRole("button", { name: "編集" }).click();
    const own = page.getByRole("dialog");
    await expect(own.getByText("貸出の受付").first()).toBeVisible({ timeout: 15_000 });
    await expect(own.getByTestId("miniapp-workflow-unreadable")).toHaveCount(0);
    await page.keyboard.press("Escape");

    await loginAs(bob, "bob");
    const bobId = ((await (await bob.request.get("/api/me")).json()) as { id: string }).id;
    // アプリだけを編集で共有する（束ねたワークフローは共有しない）。
    await share(page, appId, bobId, "editor");
    await bob.goto(`/apps/${appId}`);
    await expect(bob.getByRole("heading", { name: "備品の貸出", level: 1 })).toBeVisible({ timeout: 15_000 });
    await bob.getByRole("button", { name: "編集" }).click();
    const dialog = bob.getByRole("dialog");
    // 入力の定義を読めないことを言い、組み立てるは「という入力はありません」と言わない（入力が無いのではない）。
    await expect(dialog.getByTestId("miniapp-workflow-unreadable")).toContainText(note, { timeout: 15_000 });
    await expect(dialog.getByText("貸出の受付").first()).toBeVisible();
    await expect(dialog.getByText(/という入力はありません/)).toHaveCount(0);
    if (SHOTS) await bob.screenshot({ path: `${SHOTS}/bug122-unreadable-workflow-pc.png` });
  } finally {
    await cleanup(page, [appId, wfId]);
    await bobCtx.close().catch(() => undefined);
  }
});

test("アプリの一覧のメニューの共有・削除は owner のアプリにだけ出し、共有の欄は 403 を空の一覧に見せない（BUG-78）", async ({
  page,
  browser,
}) => {
  test.setTimeout(180_000);
  await loginViaKeycloak(page);
  const appId = await createApp(page, {
    description: "会議室の予約の案内",
    screen: "会議室の予約の仕方の案内です。\n",
    workflows: [],
  });
  const name = ((await (await page.request.get(`/api/artifacts/${appId}`)).json()) as { name: string }).name;
  const bobCtx = await browser.newContext({ viewport: { width: 1440, height: 900 } });
  const bob = await bobCtx.newPage();
  let phone: Page | null = null;
  try {
    await loginAs(bob, "bob");
    const bobId = ((await (await bob.request.get("/api/me")).json()) as { id: string }).id;
    await share(page, appId, bobId, "editor");

    // 持ち主（owner）のメニューには共有・バージョン履歴・削除が出る。
    await page.setViewportSize({ width: 1440, height: 900 });
    await page.goto("/apps");
    await page.getByRole("button", { name: `${name} のその他の操作` }).click();
    const ownerMenu = page.getByRole("menu");
    await expect(ownerMenu.getByRole("menuitem", { name: "共有" })).toBeVisible();
    await expect(ownerMenu.getByRole("menuitem", { name: "バージョン履歴" })).toBeVisible();
    await expect(ownerMenu.getByRole("menuitem", { name: "削除" })).toBeVisible();
    if (SHOTS) await page.screenshot({ path: `${SHOTS}/bug78-list-owner-pc.png` });
    await page.keyboard.press("Escape");

    // 共有を受けた人（編集の共有でも）のメニューには、バージョン履歴だけが出る（共有・削除は owner が要る）。
    await bob.goto("/apps");
    await bob.getByRole("button", { name: `${name} のその他の操作` }).click();
    const bobMenu = bob.getByRole("menu");
    await expect(bobMenu.getByRole("menuitem", { name: "バージョン履歴" })).toBeVisible();
    await expect(bobMenu.getByRole("menuitem", { name: "共有" })).toHaveCount(0);
    await expect(bobMenu.getByRole("menuitem", { name: "削除" })).toHaveCount(0);
    if (SHOTS) await bob.screenshot({ path: `${SHOTS}/bug78-list-editor-pc.png` });
    await bob.keyboard.press("Escape");

    // スマホでも同じ（横にはみ出さない）。
    phone = await phoneOf(browser, bob);
    await phone.goto("/apps");
    await phone.getByRole("button", { name: `${name} のその他の操作` }).click();
    const phoneMenu = phone.getByRole("menu");
    await expect(phoneMenu.getByRole("menuitem", { name: "バージョン履歴" })).toBeVisible();
    await expect(phoneMenu.getByRole("menuitem", { name: "共有" })).toHaveCount(0);
    await expect(phoneMenu.getByRole("menuitem", { name: "削除" })).toHaveCount(0);
    expect(await phone.evaluate(() => document.documentElement.scrollWidth)).toBeLessThanOrEqual(390);
    if (SHOTS) await phone.screenshot({ path: `${SHOTS}/bug78-list-editor-phone.png` });

    // 共有の欄が共有中の相手を読めない（403）なら、空の一覧と付与の欄を出さずに知らせる（持ち主でも、権限を
    // 外された直後などに起こる。ここでは一覧の応答だけを 403 にして確かめる）。
    await page.route(`**/api/artifacts/${appId}/shares`, (route) =>
      route.request().method() === "GET" ? route.fulfill({ status: 403, body: "" }) : route.continue(),
    );
    await page.getByRole("button", { name: `${name} のその他の操作` }).click();
    await page.getByRole("menu").getByRole("menuitem", { name: "共有" }).click();
    const dialog = page.getByRole("dialog");
    const notice = dialog.getByTestId("share-owner-only");
    await expect(notice).toContainText("共有を管理できるのは持ち主だけです");
    await expect(dialog.getByText("まだ誰にも共有していません。")).toHaveCount(0);
    await expect(dialog.getByRole("searchbox")).toHaveCount(0);
    // 開くときの動き（フェード）が終わってから撮る。
    await page.waitForTimeout(500);
    if (SHOTS) await page.screenshot({ path: `${SHOTS}/bug78-share-forbidden-pc.png` });
    await page.keyboard.press("Escape");
    await expect(dialog).toHaveCount(0);
    // スマホ幅（下からのシート）でも同じく知らせ、横にはみ出さない。
    await page.setViewportSize({ width: 390, height: 844 });
    await page.getByRole("button", { name: `${name} のその他の操作` }).click();
    await page.getByRole("menu").getByRole("menuitem", { name: "共有" }).click();
    await expect(page.getByRole("dialog").getByTestId("share-owner-only")).toBeVisible();
    await page.waitForTimeout(500);
    expect(await page.evaluate(() => document.documentElement.scrollWidth)).toBeLessThanOrEqual(390);
    if (SHOTS) await page.screenshot({ path: `${SHOTS}/bug78-share-forbidden-phone.png` });
    await page.keyboard.press("Escape");
    await page.unroute(`**/api/artifacts/${appId}/shares`);
  } finally {
    await cleanupAll(
      () => cleanup(page, [appId]),
      () => bobCtx.close(),
      async () => {
        await phone?.context().close();
      },
    );
  }
});

