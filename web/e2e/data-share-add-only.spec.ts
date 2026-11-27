import { expect, test, type Browser, type BrowserContext, type Page } from "@playwright/test";

import { cleanupAll, deleteTableApi, loginAs, loginViaKeycloak, uniqueName } from "./helpers";

/// テーブルの「追加だけ」の共有（D-113・GAP-49）: alice（持ち主）が bob に「追加だけ」を渡すと、bob の一覧に
/// 「追加だけ」として出る。bob は開くとレコードを追加でき、自分が追加したレコードだけが見え、編集・取込・共有・
/// 列の編集・保存ビューの操作は出ない。alice には両方のレコードが見える。
///
/// `E2E_SHOTS_DIR` があれば、PC（1440px）とスマホ（390px）の画面を撮る。

const SHOTS = process.env.E2E_SHOTS_DIR;

async function csrf(page: Page): Promise<string> {
  return (await page.context().cookies()).find((c) => c.name === "shiki_csrf")?.value ?? "";
}

async function seedTable(page: Page, name: string): Promise<string> {
  const headers = { "X-CSRF-Token": await csrf(page) };
  const res = await page.request.post("/api/data/tables", {
    headers,
    data: {
      name,
      schema: {
        fields: [
          { key: "title", display_name: "件名", type: "text", indexed: true },
          { key: "amount", display_name: "金額", type: "number" },
        ],
      },
    },
  });
  expect(res.status(), await res.text()).toBe(201);
  return ((await res.json()) as { id: string }).id;
}

async function addRecordApi(page: Page, tableId: string, title: string, amount: number): Promise<void> {
  const res = await page.request.post(`/api/data/tables/${tableId}/records`, {
    headers: { "X-CSRF-Token": await csrf(page) },
    data: { data: { title, amount } },
  });
  expect(res.status(), await res.text()).toBe(201);
}

async function titlesVia(page: Page, tableId: string): Promise<string[]> {
  const res = await page.request.post(`/api/data/tables/${tableId}/query`, {
    headers: { "X-CSRF-Token": await csrf(page) },
    data: {},
  });
  expect(res.ok(), await res.text()).toBeTruthy();
  const body = (await res.json()) as { items: { data: { title?: string } }[] };
  return body.items.map((i) => i.data.title ?? "").sort();
}

/// bob のセッションを引き継いだスマホ幅の画面。
async function phoneOf(browser: Browser, from: BrowserContext): Promise<BrowserContext> {
  return browser.newContext({
    storageState: await from.storageState(),
    viewport: { width: 390, height: 844 },
    deviceScaleFactor: 2,
    isMobile: true,
    hasTouch: true,
  });
}

test("テーブルを「追加だけ」で共有すると、相手は追加でき、自分のレコードだけ見え、編集できない", async ({ page, browser }) => {
  test.setTimeout(120_000);
  await page.setViewportSize({ width: 1440, height: 900 });
  // bob を先に一度ログインさせる（ディレクトリに載せて検索に出す）。
  const bobCtx = await browser.newContext({ viewport: { width: 1440, height: 900 } });
  const bob = await bobCtx.newPage();
  await loginAs(bob, "bob");
  await loginViaKeycloak(page); // alice
  const name = uniqueName("e2e-申請");
  let tableId = "";
  let phone: BrowserContext | null = null;
  try {
    tableId = await seedTable(page, name);
    await addRecordApi(page, tableId, "alice の申請", 98_765);

    // alice: 共有のダイアログで bob を「追加だけ」にする。
    await page.goto(`/data/${tableId}`);
    await expect(page.getByTestId("data-grid")).toHaveAttribute("data-loaded", "1");
    await page.getByRole("button", { name: "テーブルを共有" }).click();
    const share = page.getByTestId("table-share-dialog");
    await share.getByPlaceholder("名前・メールで検索").fill("bob");
    const bobRow = share.locator("li", { hasText: "bob@" });
    await expect(bobRow).toBeVisible({ timeout: 10_000 });
    await bobRow.getByRole("combobox", { name: /と共有する権限$/ }).click();
    // 一覧の中にだけ補足（見られるのは自分が追加したレコードと個別に共有されたレコード）が出る。
    const option = page.getByTestId("share-role-submitter");
    await expect(option).toContainText("見られるのは、自分が追加したレコードと、個別に共有されたレコードです。");
    if (SHOTS) await page.screenshot({ path: `${SHOTS}/add-only-share-menu-pc.png` });
    await option.click();
    await bobRow.getByRole("button", { name: "共有", exact: true }).click();
    const entry = share.getByTestId("share-entry").filter({ hasText: "bob@" });
    await expect(entry).toBeVisible({ timeout: 10_000 });
    await expect(entry.getByRole("combobox", { name: /の権限$/ })).toHaveText("追加だけ");
    if (SHOTS) await page.screenshot({ path: `${SHOTS}/add-only-share-dialog-pc.png` });
    await page.keyboard.press("Escape");

    // bob: 一覧に「追加だけ」として出る。
    await bob.goto("/data");
    const row = bob.getByTestId("data-table-list").locator("li", { hasText: name });
    await expect(row).toBeVisible({ timeout: 15_000 });
    await expect(row.getByTestId("data-table-add-only")).toHaveText("追加だけ");
    if (SHOTS) await bob.screenshot({ path: `${SHOTS}/add-only-list-pc.png` });
    await row.getByRole("button").first().click();

    // bob: 開くと、追加の操作と案内だけが出る（alice のレコードは見えない＝まだ自分のレコードは無い）。
    const grid = bob.getByTestId("data-grid");
    await expect(grid).toHaveAttribute("data-loaded", "0", { timeout: 15_000 });
    await expect(grid).toHaveAttribute("data-editable", "0");
    await expect(bob.getByTestId("data-add-only-note")).toBeVisible();
    await expect(bob.getByTestId("data-grid-empty")).toContainText("まだ自分が追加したレコードはありません");
    await expect(bob.getByText("alice の申請")).toHaveCount(0);
    for (const hidden of ["import-open", "schema-editor-open", "table-share-open"]) {
      await expect(bob.getByTestId(hidden)).toHaveCount(0);
    }
    await expect(bob.getByRole("button", { name: /ビュー/ })).toHaveCount(0);
    if (SHOTS) await bob.screenshot({ path: `${SHOTS}/add-only-empty-pc.png` });

    // bob: レコードを追加すると、自分のレコードだけが 1 件見える。
    await bob.getByTestId("add-record").click();
    const dialog = bob.getByTestId("record-dialog");
    await dialog.getByLabel("件名").fill("bob の申請");
    await dialog.getByLabel("金額").fill("1200");
    await bob.getByTestId("record-dialog-submit").click();
    await expect(dialog).toBeHidden();
    await expect(bob.getByTestId("data-count")).toHaveText("1 件", { timeout: 15_000 });
    await expect(grid).toHaveAttribute("data-loaded", "1");
    if (SHOTS) await bob.screenshot({ path: `${SHOTS}/add-only-table-pc.png` });

    // 裏取り: bob に見えるのは自分の 1 件だけ、alice には 2 件。bob は書き換えられない（403）。
    expect(await titlesVia(bob, tableId)).toEqual(["bob の申請"]);
    expect(await titlesVia(page, tableId)).toEqual(["alice の申請", "bob の申請"]);
    const mine = await bob.request.post(`/api/data/tables/${tableId}/query`, {
      headers: { "X-CSRF-Token": await csrf(bob) },
      data: {},
    });
    const [rec] = ((await mine.json()) as { items: { id: string; rev: number }[] }).items;
    const patch = await bob.request.patch(`/api/data/tables/${tableId}/records/${rec.id}`, {
      headers: { "X-CSRF-Token": await csrf(bob) },
      data: { patch: { title: "書き換え" }, expected_rev: rec.rev },
    });
    expect(patch.status()).toBe(403);

    // スマホ（390px）: 同じ画面が崩れずに出る。
    phone = await phoneOf(browser, bobCtx);
    const p = await phone.newPage();
    await p.goto("/data");
    await expect(p.getByTestId("data-table-list").locator("li", { hasText: name })).toBeVisible({ timeout: 15_000 });
    if (SHOTS) await p.screenshot({ path: `${SHOTS}/add-only-list-mobile.png` });
    await p.goto(`/data/${tableId}`);
    await expect(p.getByTestId("data-grid")).toHaveAttribute("data-loaded", "1", { timeout: 15_000 });
    await expect(p.getByTestId("data-add-only-note")).toBeVisible();
    await expect(p.getByTestId("add-record")).toBeVisible();
    const overflow = await p.evaluate(() => document.documentElement.scrollWidth - window.innerWidth);
    expect(overflow, "横にはみ出さない").toBeLessThanOrEqual(0);
    if (SHOTS) await p.screenshot({ path: `${SHOTS}/add-only-table-mobile.png` });
    await p.getByTestId("add-record").click();
    const sheet = p.getByTestId("record-dialog");
    await expect(sheet).toBeVisible();
    // 下から出るシートの動きが終わってから撮る。
    await sheet.evaluate((el) => Promise.all(el.getAnimations({ subtree: true }).map((a) => a.finished)));
    if (SHOTS) await p.screenshot({ path: `${SHOTS}/add-only-add-dialog-mobile.png` });
    await p.keyboard.press("Escape");

    // alice のスマホ幅の共有のダイアログ（権限の欄に「追加だけ」）。
    await page.setViewportSize({ width: 390, height: 844 });
    await page.goto(`/data/${tableId}`);
    await expect(page.getByTestId("data-grid")).toHaveAttribute("data-loaded", "2", { timeout: 15_000 });
    await page.getByRole("button", { name: "テーブルを共有" }).click();
    await expect(page.getByTestId("table-share-dialog").getByTestId("share-entry").filter({ hasText: "bob@" })).toBeVisible({
      timeout: 10_000,
    });
    if (SHOTS) await page.screenshot({ path: `${SHOTS}/add-only-share-dialog-mobile.png` });
  } finally {
    await cleanupAll(
      () => (tableId ? deleteTableApi(page, tableId) : Promise.resolve()),
      () => (phone ? phone.close() : Promise.resolve()),
      () => bobCtx.close(),
    );
  }
});
