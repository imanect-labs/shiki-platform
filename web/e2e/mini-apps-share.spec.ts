import { test, expect, type Browser, type Page } from "@playwright/test";

import { loginAs, loginViaKeycloak, uniqueName } from "./helpers";

/// ミニアプリの実行画面の共有ボタン（BUG-78）の E2E。
/// - 共有（相手の一覧と付与・解除）は owner だけができるので、共有ボタンは owner にだけ出す（解決の応答の `can_share`）。
///   閲覧・編集の共有を受けた人には出さない。
/// 作ったアプリは最後に消す。`E2E_SHOTS_DIR` を渡すとスクリーンショットを撮る。

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
