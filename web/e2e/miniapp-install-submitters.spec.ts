import { expect, test, type Page } from "@playwright/test";

import { cleanupAll, loginViaKeycloak, uniqueName } from "./helpers";

/// コードアプリの同意インストールで、アプリのテーブルに「追加だけ」を渡す部署・グループを選べる（GAP-53・
/// インストールの `submitter_roles`）。テーブルを持つアプリだけに欄が出て、選んだ部署がインストールの要求に載る。
/// 画面かサーバーの処理を持つアプリには、アプリのトークンで何が渡るかを言う（使う人と契約の ID。所属グループは
/// 載らない・D-130）。
/// PC（1440px）とスマホ（390px）で見る。`E2E_SHOTS_DIR` があれば画面を撮る。

const SHOTS = process.env.E2E_SHOTS_DIR;

async function csrf(page: Page): Promise<string> {
  return (await page.context().cookies()).find((c) => c.name === "shiki_csrf")?.value ?? "";
}

async function apiJson(page: Page, method: "post" | "put", path: string, body: unknown): Promise<unknown> {
  const res = await page.request[method](`/api${path}`, {
    headers: { "content-type": "application/json", "x-csrf-token": await csrf(page) },
    data: JSON.stringify(body),
  });
  expect(res.ok(), `${method} ${path}: ${res.status()} ${await res.text().catch(() => "")}`).toBeTruthy();
  return res.status() === 204 ? undefined : res.json();
}

/// テーブルを 1 つ持つアプリを publish する（マニフェストの id = アプリの id）。
async function publishApp(page: Page, name: string, table: string): Promise<string> {
  const manifest = {
    name,
    version: "1.0.0",
    description: "申請を受け付けるアプリ",
    requested_scopes: ["data.read", "data.write"],
    tools: [],
    tables: [{ name: table, schema: { fields: [{ key: "title", display_name: "件名", type: "text" }] } }],
    workflows: [],
    budget: {},
    frontend: null,
    server: null,
    trust_tier: "in_house",
  };
  const created = (await apiJson(page, "post", "/apps/manifests", { manifest })) as { id: string };
  await apiJson(page, "post", `/apps/manifests/${created.id}/publish`, {});
  return created.id;
}

test("コードアプリの同意インストールで、テーブルに「追加だけ」を渡す部署を選べる（GAP-53）", async ({ page, browser }) => {
  test.setTimeout(180_000);
  await page.setViewportSize({ width: 1440, height: 900 });
  await loginViaKeycloak(page);
  const name = uniqueName("e2e-申請アプリ");
  const table = uniqueName("e2e-申請表");
  let appId = "";
  let installed = false;
  try {
    appId = await publishApp(page, name, table);

    // スマホ: 同意のダイアログに欄が出て、横にはみ出さない（ここではインストールしない）。
    const phoneCtx = await browser.newContext({
      storageState: await page.context().storageState(),
      viewport: { width: 390, height: 844 },
      deviceScaleFactor: 2,
      isMobile: true,
      hasTouch: true,
    });
    try {
      const p = await phoneCtx.newPage();
      // 画面（B1）を持つアプリとして見せる（バンドルは作らない・マニフェストの応答だけ差し替える）。画面やサーバーの
      // 処理を持つアプリには、トークンでアプリに渡るものを言う。
      await p.route(`**/api/apps/manifests/${appId}*`, async (route) => {
        const res = await route.fetch();
        const body = (await res.json()) as { manifest: Record<string, unknown> };
        body.manifest.frontend = { bundle_key: "e2e", sha256: "e2e" };
        await route.fulfill({ response: res, json: body });
      });
      await p.goto("/apps");
      const row = p.locator("li", { hasText: name });
      await row.getByRole("button", { name: "インストール" }).click();
      const field = p.getByTestId("install-submitters");
      await expect(field).toContainText(`このアプリのテーブル（「${table}」）`);
      await expect(p.getByTestId("install-identity-note")).toHaveText(
        "選んだ権限にかかわらず、使う人と契約を見分ける ID は、このアプリに渡ります。所属する部署・グループは、使う人の情報を読む権限を付けたときだけ渡ります。",
      );
      expect(await p.evaluate(() => document.documentElement.scrollWidth <= window.innerWidth + 1)).toBe(true);
      if (SHOTS) await p.screenshot({ path: `${SHOTS}/install-submitters-phone.png` });
    } finally {
      await phoneCtx.close();
    }

    // PC: 部署を探して選び、同意してインストールする。
    await page.goto("/apps");
    const row = page.locator("li", { hasText: name });
    await row.getByRole("button", { name: "インストール" }).click();
    const field = page.getByTestId("install-submitters");
    await expect(field).toContainText("レコードを追加するだけの部署・グループ");
    // 画面もサーバーの処理も無いアプリはトークンを受け取らないので、渡るものは言わない。
    await expect(page.getByTestId("install-identity-note")).toHaveCount(0);
    await expect(field).toContainText(
      `選んだ部署・グループの人は、このアプリのテーブル（「${table}」）にレコードを追加できます。見られるのは、自分が追加したレコードと、個別に共有されたレコードだけです。`,
    );
    // インストールの後も、カードの「使える部署・グループ」で足したり外したりできることを言う（GAP-68）。
    await expect(field.getByTestId("install-submitters-later")).toHaveText(
      "インストールの後も、アプリの「使える部署・グループ」で足したり外したりできます。",
    );
    await field.getByRole("combobox", { name: "レコードを追加するだけの部署・グループを探す" }).fill("営業");
    await page.getByRole("option", { name: /営業部/ }).first().click();
    await expect(field.getByRole("list", { name: "レコードを追加するだけの部署・グループ" })).toContainText("営業部");
    if (SHOTS) await page.screenshot({ path: `${SHOTS}/install-submitters-pc.png` });
    const sent = page.waitForResponse((r) => r.url().endsWith("/api/apps/installations") && r.request().method() === "POST");
    await page.getByRole("button", { name: "同意してインストール" }).click();
    const res = await sent;
    expect(res.status(), await res.text()).toBe(200);
    installed = true;
    const body = res.request().postDataJSON() as { submitter_roles: string[]; granted_scopes: string[] };
    expect(body.submitter_roles).toEqual(["sales"]);
    expect(((await res.json()) as { table_ids: string[] }).table_ids).toHaveLength(1);
  } finally {
    await cleanupAll(
      async () => {
        if (!installed) return;
        const r = await page.request.delete(`/api/apps/installations/${appId}`, { headers: { "x-csrf-token": await csrf(page) } });
        expect([200, 204, 404]).toContain(r.status());
      },
      async () => {
        if (!appId) return;
        // マニフェストのアーティファクトは消す。レジストリの登録（/apps の「インストールできるアプリ」）は消す口が
        // まだ無いので残る（手元で流すたびに 1 行増える。miniapp-b1 も同じ）。
        const r = await page.request.delete(`/api/artifacts/${appId}`, { headers: { "x-csrf-token": await csrf(page) } });
        expect([200, 204, 404], `アーティファクトの削除: ${r.status()}`).toContain(r.status());
      },
    );
  }
});
