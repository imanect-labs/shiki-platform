import { expect, test, type Page } from "@playwright/test";

import { cleanupAll, loginViaKeycloak, uniqueName } from "./helpers";

/// コードアプリの同意インストールで、アプリのテーブルに「追加だけ」を渡す部署・グループを選べる（GAP-53・
/// インストールの `submitter_roles`）。テーブルを持つアプリだけに欄が出て、選んだ部署がインストールの要求に載る。
/// 画面かサーバーの処理を持つアプリには、アプリのトークンで何が渡るかを言う（使う人と契約の ID。所属グループは
/// 載らない・D-130）。
/// 2 本目は、公開の取り下げ（GAP-69b）: 持ち主に「取り下げ」が出て、確かめのダイアログ・失敗の表示・キーボードで
/// 閉じる・開くができ、取り下げると一覧から消えて新しくはインストールできない。入れてあるアプリのカードからも取り下げ
/// られ、取り下げた後もアプリはカードに残り、取り下げたことを出す。
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

/// アプリを publish する（マニフェストの id = アプリの id・`table` があればテーブルを 1 つ持つ）。返すのはアプリの id と、
/// レジストリの登録の id（取り下げに使う）。
async function publishApp(page: Page, name: string, table?: string): Promise<{ appId: string; entryId: string }> {
  const manifest = {
    name,
    version: "1.0.0",
    description: "申請を受け付けるアプリ",
    requested_scopes: ["data.read", "data.write"],
    tools: [],
    tables: table ? [{ name: table, schema: { fields: [{ key: "title", display_name: "件名", type: "text" }] } }] : [],
    workflows: [],
    budget: {},
    frontend: null,
    server: null,
    trust_tier: "in_house",
  };
  const created = (await apiJson(page, "post", "/apps/manifests", { manifest })) as { id: string };
  const entry = (await apiJson(page, "post", `/apps/manifests/${created.id}/publish`, {})) as { id: string };
  return { appId: created.id, entryId: entry.id };
}

/// 公開を取り下げる（もう取り下げていても 204）・アプリを消す後片付け（BUG-126）。
async function yankAndDelete(page: Page, app: { appId: string; entryId: string } | null): Promise<void> {
  if (!app) return;
  const headers = { "x-csrf-token": await csrf(page) };
  const yanked = await page.request.post(`/api/apps/registry/${app.entryId}/yank`, { headers });
  expect([204, 404], `公開の取り下げ: ${yanked.status()}`).toContain(yanked.status());
  const r = await page.request.delete(`/api/artifacts/${app.appId}`, { headers });
  expect([200, 204, 404], `アーティファクトの削除: ${r.status()}`).toContain(r.status());
}

test("コードアプリの同意インストールで、テーブルに「追加だけ」を渡す部署を選べる（GAP-53）", async ({ page, browser }) => {
  test.setTimeout(180_000);
  await page.setViewportSize({ width: 1440, height: 900 });
  await loginViaKeycloak(page);
  const name = uniqueName("e2e-申請アプリ");
  const table = uniqueName("e2e-申請表");
  let app: { appId: string; entryId: string } | null = null;
  let installed = false;
  try {
    app = await publishApp(page, name, table);
    const appId = app.appId;

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
      let asksIdentity = false;
      await p.route(`**/api/apps/manifests/${appId}*`, async (route) => {
        const res = await route.fetch();
        const body = (await res.json()) as { manifest: Record<string, unknown> };
        body.manifest.frontend = { bundle_key: "e2e", sha256: "e2e" };
        if (asksIdentity) {
          body.manifest.requested_scopes = [...(body.manifest.requested_scopes as string[]), "identity.read"];
        }
        await route.fulfill({ response: res, json: body });
      });
      await p.goto("/apps");
      const row = p.locator("li", { hasText: name });
      await row.getByRole("button", { name: "インストール" }).click();
      const field = p.getByTestId("install-submitters");
      await expect(field).toContainText(`このアプリのテーブル（「${table}」）`);
      const passed = "選んだ権限にかかわらず、使う人を見分ける ID と、契約全体を見分ける ID は、このアプリに渡ります。";
      // 所属を読む権限を求めないアプリには、所属は渡らない。
      await expect(p.getByTestId("install-identity-note")).toHaveText(`${passed}所属する部署・グループは渡りません。`);
      await p.getByRole("button", { name: "キャンセル" }).click();
      // 求めるアプリでは、一覧に出ている名前（identity.read）で、付けたときだけ渡ると言う。
      asksIdentity = true;
      await row.getByRole("button", { name: "インストール" }).click();
      await expect(p.getByRole("dialog").getByText("identity.read", { exact: true })).toBeVisible();
      await expect(p.getByTestId("install-identity-note")).toHaveText(
        `${passed}所属する部署・グループは、identity.read を付けたときだけ渡ります。`,
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
        if (!installed || !app) return;
        const r = await page.request.delete(`/api/apps/installations/${app.appId}`, {
          headers: { "x-csrf-token": await csrf(page) },
        });
        expect([200, 204, 404]).toContain(r.status());
      },
      // 公開を取り下げ（組織の「インストールできるアプリ」に残さない）、マニフェストのアーティファクトを消す。
      () => yankAndDelete(page, app),
    );
  }
});

test("コードアプリの公開を取り下げると、一覧から消えて新しくはインストールできず、入れたアプリは動き続ける（GAP-69b）", async ({ page }) => {
  test.setTimeout(180_000);
  await page.setViewportSize({ width: 1440, height: 900 });
  await loginViaKeycloak(page);
  const listed = uniqueName("e2e-取り下げ");
  const running = uniqueName("e2e-入れたアプリ");
  let a: { appId: string; entryId: string } | null = null;
  let b: { appId: string; entryId: string } | null = null;
  let installed = false;
  try {
    a = await publishApp(page, listed);
    b = await publishApp(page, running);
    await apiJson(page, "post", "/apps/installations", { name: running, version: "1.0.0", granted_scopes: ["data.read"] });
    installed = true;

    await page.goto("/apps");
    // 一覧の行で探す（取り下げのトーストも同じ名前を含む li になる）。
    const registryRows = page.getByRole("list", { name: "インストールできるアプリ" }).getByRole("listitem");
    const row = registryRows.filter({ hasText: listed });
    const yankA = row.getByRole("button", { name: `「${listed}」v1.0.0 の公開を取り下げる` });
    await expect(yankA).toBeVisible();
    const dialog = page.getByRole("dialog");

    // キーボードで開いて閉じる。開いたら、取り消せない操作なので先に「キャンセル」へフォーカスが来る。
    // （閉じた後に開いたボタンへフォーカスが戻らないのは、共有の確認ダイアログ〔アンインストールと同じ〕の既存の挙動。）
    await yankA.focus();
    await page.keyboard.press("Enter");
    await expect(dialog.getByRole("heading", { name: "公開を取り下げますか？" })).toBeVisible();
    await expect(dialog).toContainText(
      `「${listed}」v1.0.0 の公開を取り下げます。新しくインストールできなくなり、一覧から消えます。入れてあるアプリは動き続けます。`,
    );
    await expect(dialog.getByRole("button", { name: "キャンセル" })).toBeFocused();
    if (SHOTS) await page.screenshot({ path: `${SHOTS}/yank-confirm-pc.png` });
    await page.keyboard.press("Escape");
    await expect(dialog).toBeHidden();
    await expect(row).toBeVisible();

    // スマホ: 行の「取り下げ」と「インストール」が横にはみ出さない。
    await page.setViewportSize({ width: 390, height: 844 });
    await expect(yankA).toBeVisible();
    expect(await page.evaluate(() => document.documentElement.scrollWidth <= window.innerWidth + 1)).toBe(true);
    if (SHOTS) {
      await row.scrollIntoViewIfNeeded();
      await page.screenshot({ path: `${SHOTS}/yank-row-phone.png` });
    }
    await page.setViewportSize({ width: 1440, height: 900 });

    // 失敗はダイアログの中に出し、閉じない（ここでは 403 を返させる）。
    await page.route(`**/api/apps/registry/${a.entryId}/yank`, (route) =>
      route.fulfill({ status: 403, json: { error: "forbidden" } }),
    );
    await yankA.click();
    await dialog.getByRole("button", { name: "取り下げる" }).click();
    await expect(dialog).toContainText("取り下げられるのは、アプリの持ち主と組織の管理者だけです。");
    await expect(dialog.getByRole("button", { name: "取り下げる" })).toBeEnabled();
    if (SHOTS) await page.screenshot({ path: `${SHOTS}/yank-failed-pc.png` });
    await page.unroute(`**/api/apps/registry/${a.entryId}/yank`);

    // 取り下げると一覧から消え、その版は新しくはインストールできない。
    await dialog.getByRole("button", { name: "取り下げる" }).click();
    await expect(dialog).toBeHidden();
    await expect(page.getByText(`「${listed}」v1.0.0 の公開を取り下げました`).first()).toBeVisible();
    await expect(registryRows.filter({ hasText: listed })).toHaveCount(0);
    const refused = await page.request.post("/api/apps/installations", {
      headers: { "content-type": "application/json", "x-csrf-token": await csrf(page) },
      data: JSON.stringify({ name: listed, version: "1.0.0", granted_scopes: ["data.read"] }),
    });
    expect(refused.status(), await refused.text()).toBe(409);

    // 入れてあるアプリのカードからも取り下げられる。取り下げた後もカードは残り、そう出す。
    const card = page.getByRole("list", { name: "インストール済みのアプリ" }).getByRole("listitem").filter({ hasText: running });
    await expect(card.getByTestId("app-yanked")).toHaveCount(0);
    await card.getByRole("button", { name: `「${running}」v1.0.0 の公開を取り下げる` }).click();
    await dialog.getByRole("button", { name: "取り下げる" }).click();
    await expect(dialog).toBeHidden();
    const note = card.getByTestId("app-yanked");
    await expect(note).toContainText("この版は公開を取り下げました");
    await expect(note).toContainText("このまま使えますが、新しくインストールすることはできません。");
    await expect(card.getByRole("button", { name: /公開を取り下げる/ })).toHaveCount(0);
    if (SHOTS) await page.screenshot({ path: `${SHOTS}/yank-installed-pc.png`, fullPage: true });

    await page.setViewportSize({ width: 390, height: 844 });
    await expect(note).toBeVisible();
    expect(await page.evaluate(() => document.documentElement.scrollWidth <= window.innerWidth + 1)).toBe(true);
    if (SHOTS) await page.screenshot({ path: `${SHOTS}/yank-installed-phone.png`, fullPage: true });
  } finally {
    await cleanupAll(
      async () => {
        if (!installed || !b) return;
        const r = await page.request.delete(`/api/apps/installations/${b.appId}`, {
          headers: { "x-csrf-token": await csrf(page) },
        });
        expect([200, 204, 404]).toContain(r.status());
      },
      () => yankAndDelete(page, a),
      () => yankAndDelete(page, b),
    );
  }
});
