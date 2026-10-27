import { test, expect, type Browser, type Page } from "@playwright/test";

import { loginAs, loginViaKeycloak, uniqueName } from "./helpers";

/// ミニアプリの実行画面の共有ボタンと、共同編集者の保存の誤り（BUG-78・BUG-80）の E2E。
/// - 共有（相手の一覧と付与・解除）は owner だけができるので、共有ボタンは owner にだけ出す（解決の応答の `can_share`）。
///   閲覧・編集の共有を受けた人には出さない。
/// - 共同編集者が自分の読めない部品を束ねたアプリを保存すると止まる（部品の権限の扱いは D-16 で未決）。そのときの誤りは
///   部品を別名と版で言い、部品の id も本文の中の位置（`workflows[0]`）も出さない。
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
