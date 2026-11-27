import { expect, test, type Page } from "@playwright/test";

import { cleanupQueue, createFolderApi, loginViaKeycloak, trashFileApi, trashFolderApi, uniqueName } from "./helpers";

/// 社内文書の検索への取り込み直しの e2e（D-126・GAP-51）:
/// - ドライブのファイルのメニュー（PC のドロップダウンとスマホの操作シート）に「検索に取り込み直す」がある。
///   押すと、社内文書の検索が使えない構成（CI の web-e2e と手元の検証のスタック）では、断る理由をそのまま出す。
///   使える構成（dev-up.sh --rag）では、受け付けた・取り込みの途中・取り込み済みのどれかを言う。
/// - 管理コンソールの「データ」の「社内文書の検索」の「取り込み」は、使えない構成では使えないことを言い、使える
///   構成では、取り込まれていないファイルをまとめて取り込むボタンを出す（押すとテナント全体に効くので、ここでは押さない）。
///
/// 取り込み直しが本文を索引に入れること・二重に積まないこと・編集できない人を断ることは、rag の結合テスト
/// reingest_it と api の結合テスト workflow_document_read_it が確かめる。

const SHOTS = process.env.E2E_SHOTS_DIR;
const DISABLED = "この環境では社内文書の検索が使えません。管理者に連絡してください。";

const cleanup = cleanupQueue();
test.afterEach(() => cleanup.run());

async function ragEnabled(page: Page): Promise<boolean> {
  const csrf = (await page.context().cookies()).find((c) => c.name === "shiki_csrf")?.value ?? "";
  const probe = await page.request.post("/api/search", {
    headers: { "x-csrf-token": csrf, "content-type": "application/json" },
    data: { query: "probe" },
  });
  return probe.status() !== 503;
}

/// `name` のファイルを `folderId` に画面から置き、id を返す。
async function uploadText(page: Page, folderId: string, name: string): Promise<string> {
  await page.goto(`/drive?folder=${folderId}`);
  await page
    .locator('input[type="file"][multiple]')
    .setInputFiles({ name, mimeType: "text/plain", buffer: Buffer.from("四半期の議事録\n", "utf8") });
  await expect(page.getByText(name, { exact: true })).toBeVisible({ timeout: 20_000 });
  let id = "";
  await expect
    .poll(
      async () => {
        const res = await page.request.get(`/api/nodes?parent_id=${folderId}`);
        const children = (await res.json()) as { items?: { id: string; name: string }[] };
        id = children.items?.find((c) => c.name === name)?.id ?? "";
        return id;
      },
      { timeout: 30_000 },
    )
    .not.toBe("");
  return id;
}

/// 押した後の通知（使えない構成では断る理由、使える構成では答え）。
async function expectAnswer(page: Page, rag: boolean, name: string) {
  if (rag) {
    await expect(
      page
        .getByText(new RegExp(`「${name}」の取り込み直しを受け付けました|取り込みの途中です|取り込み済みです`))
        .first(),
    ).toBeVisible({ timeout: 15_000 });
  } else {
    await expect(page.getByText("取り込み直せませんでした").first()).toBeVisible({ timeout: 15_000 });
    await expect(page.getByText(DISABLED).first()).toBeVisible();
  }
}

test("ドライブ: ファイルのメニューから検索に取り込み直す（PC とスマホ・D-126）", async ({ browser, page }) => {
  test.setTimeout(180_000);
  await page.setViewportSize({ width: 1440, height: 900 });
  await loginViaKeycloak(page);
  const rag = await ragEnabled(page);
  test.info().annotations.push({
    type: "社内文書の検索",
    description: rag ? "有効: 取り込み直しの答えを確かめる" : "無効: 断る理由を確かめる",
  });
  const folderId = await createFolderApi(page, uniqueName("e2e-取り込み直し"));
  cleanup.add(() => trashFolderApi(page, folderId));
  const name = `${uniqueName("議事録")}.txt`;
  const fileId = await uploadText(page, folderId, name);
  cleanup.add(() => trashFileApi(page, fileId));

  // PC: 行の「…」のドロップダウンに「検索に取り込み直す」がある。
  await page.getByRole("button", { name: `「${name}」の操作` }).first().click();
  const item = page.getByRole("menuitem", { name: "検索に取り込み直す" });
  await expect(item).toBeVisible();
  if (SHOTS) await page.screenshot({ path: `${SHOTS}/drive-reindex-menu-pc.png` });
  const answered = page.waitForResponse(
    (r) => r.url().includes(`/api/files/${fileId}/reindex`) && r.request().method() === "POST",
  );
  await item.click();
  const res = await answered;
  if (rag) {
    expect([200, 202], await res.text()).toContain(res.status());
  } else {
    expect(res.status(), await res.text()).toBe(503);
    expect(await res.json()).toMatchObject({ code: "search_disabled", message: DISABLED });
  }
  await expectAnswer(page, rag, name);
  if (SHOTS) await page.screenshot({ path: `${SHOTS}/drive-reindex-answer-pc.png` });

  const phoneCtx = await browser.newContext({
    storageState: await page.context().storageState(),
    viewport: { width: 390, height: 844 },
    hasTouch: true,
    isMobile: true,
    deviceScaleFactor: 3,
    ignoreHTTPSErrors: true,
  });
  try {
    const phone = await phoneCtx.newPage();
    await phone.goto(`/drive?folder=${folderId}`);
    await expect(phone.getByText(name, { exact: true })).toBeVisible({ timeout: 20_000 });
    // スマホ: 「…」で開く操作シートの行に「検索に取り込み直す」がある。
    await phone.getByRole("button", { name: `「${name}」の操作` }).first().click();
    const sheet = phone.getByTestId("node-action-sheet");
    await expect(sheet).toBeVisible();
    const row = sheet.getByRole("button", { name: "検索に取り込み直す" });
    await expect(row).toBeVisible();
    expect(await sheet.evaluate((el) => el.scrollWidth <= el.clientWidth + 1)).toBe(true);
    if (SHOTS) await phone.screenshot({ path: `${SHOTS}/drive-reindex-menu-phone.png` });
    await row.click();
    await expectAnswer(phone, rag, name);
    expect(await phone.evaluate(() => document.documentElement.scrollWidth)).toBeLessThanOrEqual(390);
    if (SHOTS) await phone.screenshot({ path: `${SHOTS}/drive-reindex-answer-phone.png` });
  } finally {
    await phoneCtx.close();
  }
});

test("管理コンソール: 社内文書の検索の取り込み（PC とスマホ・D-126）", async ({ browser, page }) => {
  test.setTimeout(120_000);
  await page.setViewportSize({ width: 1440, height: 900 });
  // 前提: dev seed で alice が組織管理者。
  await loginViaKeycloak(page);
  const rag = await ragEnabled(page);
  await page.goto("/admin/data/settings");
  // 「データ」のメニューに「社内文書の検索」の「取り込み」がある。
  await page.getByRole("link", { name: "取り込み", exact: true }).click();
  await page.waitForURL(/\/admin\/data\/search$/);
  await expect(page.getByRole("heading", { name: "社内文書の検索" })).toBeVisible();
  if (rag) {
    await expect(page.getByText("取り込まれていないファイル", { exact: true })).toBeVisible();
    await expect(page.getByTestId("search-backfill-start")).toBeVisible();
    await expect(page.getByText(/取り込みに失敗したファイルは対象外です。/)).toBeVisible();
  } else {
    const empty = page.getByTestId("search-ingest-disabled");
    await expect(empty).toContainText("この環境では社内文書の検索が使えません");
    await expect(empty).toContainText("rag.enabled");
    // 使えない構成では、まとめての取り込みも理由を言って断る（画面はボタンを出さない）。
    const csrf = (await page.context().cookies()).find((c) => c.name === "shiki_csrf")?.value ?? "";
    const res = await page.request.post("/api/org-admin/search/backfill", { headers: { "x-csrf-token": csrf } });
    expect(res.status()).toBe(503);
    expect(await res.json()).toMatchObject({ code: "search_disabled", message: DISABLED });
  }
  if (SHOTS) await page.screenshot({ path: `${SHOTS}/admin-search-ingest-pc.png` });

  const phoneCtx = await browser.newContext({
    storageState: await page.context().storageState(),
    viewport: { width: 390, height: 844 },
    hasTouch: true,
    isMobile: true,
    deviceScaleFactor: 3,
    ignoreHTTPSErrors: true,
  });
  try {
    const phone = await phoneCtx.newPage();
    await phone.goto("/admin/data/search");
    await expect(phone.getByRole("heading", { name: "社内文書の検索" })).toBeVisible({ timeout: 20_000 });
    await expect(
      rag ? phone.getByTestId("search-backfill-start") : phone.getByTestId("search-ingest-disabled"),
    ).toBeVisible();
    expect(await phone.evaluate(() => document.documentElement.scrollWidth)).toBeLessThanOrEqual(390);
    if (SHOTS) await phone.screenshot({ path: `${SHOTS}/admin-search-ingest-phone.png` });
  } finally {
    await phoneCtx.close();
  }
});
