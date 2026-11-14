import { expect, test, type Browser, type Page } from "@playwright/test";

import {
  cleanupQueue,
  createFolderApi,
  loginAs,
  loginViaKeycloak,
  trackCreatedResources,
  trashFileApi,
  trashFolderApi,
  uniqueName,
} from "./helpers";

/// SQL の結果の「新規 CSV として保存」の保存先（GAP-66）。
///
/// 以前は、元の CSV がどのフォルダーにあっても、いつもドライブのトップに保存していた（CSV の画面が保存先に null を
/// 渡していた）。いまは元の CSV と同じフォルダーに保存し、保存先をダイアログに書く。そのフォルダーを見られない・
/// 書き込めない人（CSV だけを共有された人）は、ドライブのトップに保存する。
///
/// 作ったもの（フォルダー・元の CSV・保存した CSV）は、テストの後にゴミ箱へ移す（BUG-126）。

const SHOTS = process.env.E2E_SHOTS_DIR;
/// compose の bob（deploy/keycloak/shiki-realm.json・alice と同じテナント）。
const BOB_ID = "00000000-0000-0000-0000-000000000002";

const cleanup = cleanupQueue();
test.beforeEach(({ page }) => trackCreatedResources(page, cleanup));
test.afterEach(() => cleanup.run());

/// 画面と同じ API（ブラウザーから）でフォルダーに CSV を作る（片付けは trackCreatedResources が積む）。
async function createCsv(page: Page, parentId: string, name: string, csv: string): Promise<string> {
  return page.evaluate(
    async ({ parent, n, body }) => {
      const csrf = document.cookie.match(/(?:^|;\s*)shiki_csrf=([^;]+)/)?.[1] ?? "";
      const res = await fetch("/api/tabular/save", {
        method: "POST",
        credentials: "include",
        headers: { "Content-Type": "application/json", "X-CSRF-Token": csrf },
        body: JSON.stringify({ parent_id: parent, name: n, csv: body }),
      });
      if (!res.ok) throw new Error(`CSV 作成に失敗: ${res.status}`);
      return (await res.json()).node_id as string;
    },
    { parent: parentId, n: name, body: csv },
  );
}

/// bob にファイルかフォルダーを共有する。
async function shareWithBob(page: Page, nodeId: string, role: "viewer" | "editor") {
  const csrf = (await page.context().cookies()).find((c) => c.name === "shiki_csrf")?.value ?? "";
  const res = await page.request.put(`/api/nodes/${nodeId}/shares`, {
    headers: { "x-csrf-token": csrf, "content-type": "application/json" },
    data: { target: { type: "user", id: BOB_ID }, role },
  });
  expect(res.ok(), await res.text()).toBeTruthy();
}

/// CSV を開き、SQL の画面で集計を流して結果を出す（スマホは見出しのアイコンで SQL に切り替える）。
async function runQuery(page: Page, id: string, phone = false) {
  await page.goto(`/csv/${id}`);
  await page.getByTestId(phone ? "csv-tab-toggle" : "csv-tab-sql").click({ timeout: 30_000 });
  await expect(page.getByTestId("sql-console")).toBeVisible({ timeout: 20_000 });
  await page.getByTestId("sql-input").fill("SELECT id, score FROM data WHERE CAST(score AS INT) >= 20 ORDER BY id");
  await page.getByTestId("sql-run").click({ timeout: 20_000 });
  await expect(page.locator('[data-testid="sql-console"] table tbody tr')).toHaveCount(2, { timeout: 20_000 });
}

/// 保存のダイアログで名前を入れて「保存」を押し、保存した CSV の ID を返す。
async function saveAs(page: Page, name: string): Promise<string> {
  const dialog = page.getByTestId("sql-save-dialog");
  await dialog.getByLabel("ファイル名").fill(name);
  const saved = page.waitForResponse(
    (r) => r.request().method() === "POST" && new URL(r.url()).pathname === "/api/tabular/save" && r.ok(),
  );
  await dialog.getByRole("button", { name: "保存", exact: true }).click();
  const id = ((await (await saved).json()) as { node_id: string }).node_id;
  await expect(dialog).toBeHidden({ timeout: 20_000 });
  return id;
}

/// ファイルのある場所（親のフォルダーの ID・トップなら null）。
async function parentOf(page: Page, id: string): Promise<string | null> {
  const res = await page.request.get(`/api/files/${id}`);
  expect(res.ok(), await res.text()).toBeTruthy();
  return ((await res.json()) as { parent_id?: string | null }).parent_id ?? null;
}

/// bob で開く画面（片付けは bob の画面で送り、最後に閉じる）。
async function bobPage(browser: Browser): Promise<Page> {
  const ctx = await browser.newContext({ viewport: { width: 1440, height: 900 }, ignoreHTTPSErrors: true });
  cleanup.add(() => ctx.close());
  const bob = await ctx.newPage();
  await loginAs(bob, "bob");
  return bob;
}

test("フォルダーにある CSV の SQL の結果は、同じフォルダーに保存すると言い、そこに保存する（PC・スマホ）", async ({
  page,
  browser,
}) => {
  test.setTimeout(180_000);
  await page.setViewportSize({ width: 1440, height: 900 });
  await loginViaKeycloak(page);
  await page.goto("/drive");
  const folderName = uniqueName("売上の集計");
  const folderId = await createFolderApi(page, folderName);
  cleanup.add(() => trashFolderApi(page, folderId));
  const source = await createCsv(page, folderId, `${uniqueName("得点")}.csv`, "id,score\n1,10\n2,30\n3,20\n");
  const saveName = uniqueName("集計");

  await runQuery(page, source);
  await page.getByTestId("sql-save").click();
  const dialog = page.getByTestId("sql-save-dialog");
  await expect(dialog.getByTestId("sql-save-destination")).toHaveText(
    `結果を新しい CSV として、この CSV と同じ「${folderName}」フォルダーに保存します。`,
  );
  if (SHOTS) await page.screenshot({ path: `${SHOTS}/csv-sql-save-folder-pc.png` });
  const saved = await saveAs(page, saveName);
  await expect(page.getByText(`「${saveName}.csv」を「${folderName}」フォルダーに作成しました。`, { exact: true })).toBeVisible();
  expect(await parentOf(page, saved)).toBe(folderId);

  // 同じ名前は、そのフォルダーの中で番号を付ける（トップの同じ名前と取り違えない）。
  await page.getByTestId("sql-save").click();
  await dialog.getByLabel("ファイル名").fill(saveName);
  await dialog.getByRole("button", { name: "保存", exact: true }).click();
  await expect(dialog.getByTestId("sql-save-name-taken")).toBeVisible({ timeout: 20_000 });
  const numbered = page.waitForResponse(
    (r) => r.request().method() === "POST" && new URL(r.url()).pathname === "/api/tabular/save" && r.ok(),
  );
  await dialog.getByTestId("sql-save-numbered").click();
  const numberedId = ((await (await numbered).json()) as { node_id: string }).node_id;
  await expect(dialog).toBeHidden({ timeout: 20_000 });
  await expect(
    page.getByText(`「${saveName} (2).csv」を「${folderName}」フォルダーに作成しました。`, { exact: true }),
  ).toBeVisible();
  expect(await parentOf(page, numberedId)).toBe(folderId);

  // スマホ: 保存先の説明が横にはみ出さない。
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
    await runQuery(phone, source, true);
    await phone.getByTestId("sql-save").click({ timeout: 20_000 });
    const sheet = phone.getByTestId("sql-save-dialog");
    await expect(sheet.getByTestId("sql-save-destination")).toHaveText(
      `結果を新しい CSV として、この CSV と同じ「${folderName}」フォルダーに保存します。`,
      { timeout: 20_000 },
    );
    expect(await phone.evaluate(() => document.documentElement.scrollWidth)).toBeLessThanOrEqual(390);
    if (SHOTS) await phone.screenshot({ path: `${SHOTS}/csv-sql-save-folder-phone.png` });
    await sheet.getByRole("button", { name: "キャンセル" }).click({ timeout: 20_000 });
    await expect(sheet).toBeHidden();
  } finally {
    await phoneCtx.close();
  }
});

test("CSV だけを共有された人は、元のフォルダーを見られないので、ドライブのトップに保存する", async ({ page, browser }) => {
  test.setTimeout(180_000);
  await page.setViewportSize({ width: 1440, height: 900 });
  await loginViaKeycloak(page);
  await page.goto("/drive");
  const folderName = uniqueName("非公開の集計");
  const folderId = await createFolderApi(page, folderName);
  cleanup.add(() => trashFolderApi(page, folderId));
  const source = await createCsv(page, folderId, `${uniqueName("得点")}.csv`, "id,score\n1,10\n2,30\n3,20\n");
  await shareWithBob(page, source, "editor");

  const bob = await bobPage(browser);
  await runQuery(bob, source);
  await bob.getByTestId("sql-save").click();
  const dialog = bob.getByTestId("sql-save-dialog");
  // 見られないフォルダーの名前は出さない。
  await expect(dialog.getByTestId("sql-save-destination")).toHaveText(
    "この CSV のフォルダーには保存できないため、結果を新しい CSV として、ドライブのトップに保存します。",
  );
  await expect(dialog.getByText(folderName)).toHaveCount(0);
  if (SHOTS) await bob.screenshot({ path: `${SHOTS}/csv-sql-save-shared-top-pc.png` });
  const saveName = uniqueName("bob の集計");
  const saved = await saveAs(bob, saveName);
  cleanup.add(() => trashFileApi(bob, saved));
  await expect(bob.getByText(`「${saveName}.csv」を作成しました。`, { exact: true })).toBeVisible();
  expect(await parentOf(bob, saved)).toBeNull();
});

test("フォルダーを見られても書き込めない人は、保存するときにドライブのトップへ保存し直し、そう言う", async ({
  page,
  browser,
}) => {
  test.setTimeout(180_000);
  await page.setViewportSize({ width: 1440, height: 900 });
  await loginViaKeycloak(page);
  await page.goto("/drive");
  const folderName = uniqueName("閲覧だけの集計");
  const folderId = await createFolderApi(page, folderName);
  cleanup.add(() => trashFolderApi(page, folderId));
  const source = await createCsv(page, folderId, `${uniqueName("得点")}.csv`, "id,score\n1,10\n2,30\n3,20\n");
  // フォルダーは閲覧だけ、CSV は編集できる（フォルダーに書き込めるかは、保存して初めて分かる）。
  await shareWithBob(page, folderId, "viewer");
  await shareWithBob(page, source, "editor");

  const bob = await bobPage(browser);
  await runQuery(bob, source);
  await bob.getByTestId("sql-save").click();
  const dialog = bob.getByTestId("sql-save-dialog");
  await expect(dialog.getByTestId("sql-save-destination")).toHaveText(
    `結果を新しい CSV として、この CSV と同じ「${folderName}」フォルダーに保存します。`,
  );
  const saveName = uniqueName("bob の集計");
  const saved = await saveAs(bob, saveName);
  cleanup.add(() => trashFileApi(bob, saved));
  await expect(
    bob.getByText(`「${folderName}」フォルダーには保存できないため、ドライブのトップに「${saveName}.csv」を作成しました。`, {
      exact: true,
    }),
  ).toBeVisible();
  if (SHOTS) await bob.screenshot({ path: `${SHOTS}/csv-sql-save-fallback-pc.png` });
  expect(await parentOf(bob, saved)).toBeNull();

  // 次に開いたダイアログは、はじめからドライブのトップと言う。
  await bob.getByTestId("sql-save").click();
  await expect(dialog.getByTestId("sql-save-destination")).toHaveText(
    "この CSV のフォルダーには保存できないため、結果を新しい CSV として、ドライブのトップに保存します。",
  );
  await dialog.getByRole("button", { name: "キャンセル" }).click();
  await expect(dialog).toBeHidden();
});
