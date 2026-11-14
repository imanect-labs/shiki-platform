import { expect, test, type Page } from "@playwright/test";

import { cleanupAll, loginAs, loginViaKeycloak, uniqueName } from "./helpers";

/// インストールしたアプリのテーブルを使える部署・グループを、インストールの後から足す・外す（GAP-68）。
///
/// alice（アプリの持ち主）がカードの「使える部署・グループ」から営業部に「閲覧」を渡すと、営業部の bob がアプリの
/// テーブルを開ける。「追加だけ」に変え、外すと、サーバーの状態（GET /apps/installations/{id}/roles）も変わる。持ち主で
/// ない bob が開くと、変えられないことを言う。テーブルを持たないアプリのカードには口を出さない。
/// PC（1440px）とスマホ（390px）で見る。`E2E_SHOTS_DIR` があれば画面を撮る。

const SHOTS = process.env.E2E_SHOTS_DIR;

async function csrf(page: Page): Promise<string> {
  return (await page.context().cookies()).find((c) => c.name === "shiki_csrf")?.value ?? "";
}

async function postJson(page: Page, path: string, body: unknown): Promise<unknown> {
  const res = await page.request.post(`/api${path}`, {
    headers: { "content-type": "application/json", "x-csrf-token": await csrf(page) },
    data: JSON.stringify(body),
  });
  expect(res.ok(), `${path}: ${res.status()} ${await res.text().catch(() => "")}`).toBeTruthy();
  return res.json();
}

/// テーブルを `tables` 個持つアプリを publish してインストールし、`{ appId, tableIds }` を返す。
async function installApp(page: Page, name: string, tables: string[]): Promise<{ appId: string; tableIds: string[] }> {
  const manifest = {
    name,
    version: "1.0.0",
    description: "経費の申請",
    requested_scopes: ["data.read"],
    tools: [],
    tables: tables.map((t) => ({ name: t, schema: { fields: [{ key: "title", display_name: "件名", type: "text" }] } })),
    workflows: [],
    budget: {},
    frontend: null,
    server: null,
    trust_tier: "in_house",
  };
  const created = (await postJson(page, "/apps/manifests", { manifest })) as { id: string };
  await postJson(page, `/apps/manifests/${created.id}/publish`, {});
  const installed = (await postJson(page, "/apps/installations", {
    name,
    version: "1.0.0",
    granted_scopes: ["data.read"],
  })) as { table_ids: string[] };
  return { appId: created.id, tableIds: installed.table_ids };
}

/// 開く動き（フェード）が終わるまで待つ（撮る画面が半透明にならないように）。
async function settled(page: Page): Promise<void> {
  await page.evaluate(() => Promise.all(document.getAnimations().map((a) => a.finished.catch(() => undefined))));
}

async function rolesOf(page: Page, appId: string) {
  const res = await page.request.get(`/api/apps/installations/${appId}/roles`);
  expect(res.ok(), await res.text()).toBeTruthy();
  return ((await res.json()) as { roles: Record<string, string[]> }).roles;
}

test("インストールしたアプリのテーブルを使える部署・グループを足す・外す（GAP-68）", async ({ page, browser }) => {
  test.setTimeout(240_000);
  await page.setViewportSize({ width: 1440, height: 900 });
  await loginViaKeycloak(page);
  const name = uniqueName("e2e-経費アプリ");
  const plain = uniqueName("e2e-表の無いアプリ");
  const table = uniqueName("e2e-経費");
  const apps: string[] = [];
  const bobCtx = await browser.newContext({ viewport: { width: 1440, height: 900 } });
  try {
    const { appId, tableIds } = await installApp(page, name, [table]);
    apps.push(appId);
    apps.push((await installApp(page, plain, [])).appId);
    const bob = await bobCtx.newPage();
    await loginAs(bob, "bob");
    // bob は営業部のメンバー（dev の seed）。まだアプリのテーブルは開けない。
    expect((await bob.request.get(`/api/data/tables/${tableIds[0]}`)).status()).toBe(404);

    await page.goto("/apps");
    const card = page.locator("li", { hasText: name });
    await expect(page.locator("li", { hasText: plain }).getByRole("button", { name: /使える部署・グループ/ })).toHaveCount(0);
    await card.getByRole("button", { name: `「${name}」のテーブルを使える部署・グループ` }).click();
    const dialog = page.getByTestId("app-table-roles-dialog");
    await expect(dialog.getByRole("heading")).toHaveText(`「${name}」のテーブルを使える部署・グループ`);
    await expect(dialog.getByTestId("app-table-roles-note")).toHaveText(
      `選んだ部署・グループの人が、このアプリのテーブル（「${table}」）を使えるようになります。外した部署・グループの人は、すぐに使えなくなります。見られるレコードと列は、開いた人の権限で決まります。`,
    );
    await expect(dialog.getByText("まだどの部署・グループも使えません。")).toBeVisible();
    // 相手は部署・グループだけ（メンバーへの切り替えは出さない）。
    await expect(dialog.getByTestId("share-kind-user")).toHaveCount(0);

    // 営業部に「閲覧」を渡す。
    await dialog.getByPlaceholder("部署・ロール名で検索").fill("営業");
    const salesRow = dialog.locator("li", { hasText: "営業部" });
    await expect(salesRow).toBeVisible({ timeout: 10_000 });
    // 操作は「共有」と言わない（テーブルの共有では渡せないもの・付録 C の P2）。
    await expect(salesRow.getByRole("button", { name: "共有", exact: true })).toHaveCount(0);
    await salesRow.getByRole("button", { name: "追加", exact: true }).click();
    await expect(page.getByText("「営業部」を追加しました", { exact: true })).toBeVisible();
    const entry = dialog.getByTestId("share-entry").filter({ hasText: "営業部" });
    await expect(entry).toBeVisible({ timeout: 10_000 });
    expect(await rolesOf(page, appId)).toEqual({ viewer_roles: ["sales"], editor_roles: [], submitter_roles: [] });
    // 営業部の bob が、アプリのテーブルを開ける。
    await expect.poll(async () => (await bob.request.get(`/api/data/tables/${tableIds[0]}`)).status()).toBe(200);
    if (SHOTS) {
      await settled(page);
      await page.screenshot({ path: `${SHOTS}/miniapp-table-roles-pc.png` });
    }

    // スマホ: 同じ画面が横にはみ出さない。
    const phoneCtx = await browser.newContext({
      storageState: await page.context().storageState(),
      viewport: { width: 390, height: 844 },
      deviceScaleFactor: 2,
      isMobile: true,
      hasTouch: true,
    });
    try {
      const p = await phoneCtx.newPage();
      await p.goto("/apps");
      const phoneCard = p.locator("li", { hasText: name });
      await expect(phoneCard.getByRole("button", { name: `「${name}」のテーブルを使える部署・グループ` })).toBeVisible();
      expect(await p.evaluate(() => document.documentElement.scrollWidth <= window.innerWidth + 1)).toBe(true);
      if (SHOTS) await phoneCard.screenshot({ path: `${SHOTS}/miniapp-table-roles-card-phone.png` });
      await phoneCard.getByRole("button", { name: `「${name}」のテーブルを使える部署・グループ` }).click();
      const sheet = p.getByTestId("app-table-roles-dialog");
      await expect(sheet.getByTestId("share-entry").filter({ hasText: "営業部" })).toBeVisible({ timeout: 10_000 });
      expect(await p.evaluate(() => document.documentElement.scrollWidth <= window.innerWidth + 1)).toBe(true);
      if (SHOTS) {
        await settled(p);
        await p.screenshot({ path: `${SHOTS}/miniapp-table-roles-phone.png` });
      }
      await sheet.getByRole("button", { name: "閉じる" }).click();
      await expect(sheet).toBeHidden();
    } finally {
      await phoneCtx.close();
    }

    // 「追加だけ」に変える（付けてから外す順）。
    await entry.getByRole("combobox", { name: "営業部 の権限" }).click();
    await page.getByTestId("share-role-submitter").click();
    await expect
      .poll(() => rolesOf(page, appId))
      .toEqual({ viewer_roles: [], editor_roles: [], submitter_roles: ["sales"] });

    // 外す。bob はまた開けなくなる。
    await entry.getByRole("button", { name: "営業部 を外す" }).click();
    await expect(dialog.getByText("まだどの部署・グループも使えません。")).toBeVisible();
    expect(await rolesOf(page, appId)).toEqual({ viewer_roles: [], editor_roles: [], submitter_roles: [] });
    expect((await bob.request.get(`/api/data/tables/${tableIds[0]}`)).status()).toBe(404);
    await dialog.getByRole("button", { name: "閉じる" }).click();
    await expect(dialog).toBeHidden();

    // 持ち主でない bob が開くと、変えられないことを言う（サーバーも 403）。
    await bob.goto("/apps");
    await bob
      .locator("li", { hasText: name })
      .getByRole("button", { name: `「${name}」のテーブルを使える部署・グループ` })
      .click();
    const denied = bob.getByTestId("app-table-roles-dialog");
    await expect(denied.getByTestId("share-owner-only")).toContainText("変えられるのは、このアプリの持ち主だけです");
    if (SHOTS) {
      await settled(bob);
      await bob.screenshot({ path: `${SHOTS}/miniapp-table-roles-not-owner.png` });
    }
    const res = await bob.request.patch(`/api/apps/installations/${appId}/roles`, {
      headers: { "content-type": "application/json", "x-csrf-token": await csrf(bob) },
      data: JSON.stringify({ add: { viewer_roles: ["sales"] } }),
    });
    expect(res.status()).toBe(403);
  } finally {
    await cleanupAll(
      ...apps.map((id) => async () => {
        const r = await page.request.delete(`/api/apps/installations/${id}`, { headers: { "x-csrf-token": await csrf(page) } });
        expect([200, 204, 404]).toContain(r.status());
      }),
      ...apps.map((id) => async () => {
        // レジストリの登録は消す口が無いので残る（miniapp-b1 と同じ）。
        const r = await page.request.delete(`/api/artifacts/${id}`, { headers: { "x-csrf-token": await csrf(page) } });
        expect([200, 204, 404]).toContain(r.status());
      }),
      () => bobCtx.close(),
    );
  }
});
