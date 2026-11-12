import { expect, test, type Page } from "@playwright/test";

import { cleanupQueue, createFolderApi, loginAs, loginViaKeycloak, trashFolderApi, uniqueName } from "./helpers";

/// ゴミ箱の完全削除と「ゴミ箱を空にする」（GAP-108b・D-201）:
/// - フォルダの持ち主（alice）は、中のほかの人（bob）のファイルもまとめて完全に削除できる。確かめのダイアログに
///   中のファイルの数と、ほかの人が作った数が出る。キーボードで開け、Esc で閉じると開いたボタンへ戻る。
/// - 削除に失敗したときの文言（中に削除の後で作られたファイルがある 409・権限の 403）はダイアログの中に出る
///   （応答は route で差し替える）。
/// - 持ち主でも管理者でもない人（bob）には、共有されたフォルダの「完全に削除」が出ない。「空にする」は自分の
///   ものだけを消し、残した数を言う。
/// 作ったフォルダは最後にゴミ箱へ移す（完全に削除したものは 404 で何もしない）。`E2E_SHOTS_DIR` があれば撮る。

const SHOTS = process.env.E2E_SHOTS_DIR;
const cleanup = cleanupQueue();
test.afterEach(() => cleanup.run());

async function csrf(page: Page): Promise<Record<string, string>> {
  const token = (await page.context().cookies()).find((c) => c.name === "shiki_csrf")?.value ?? "";
  return { "x-csrf-token": token, "content-type": "application/json" };
}

async function myId(page: Page): Promise<string> {
  return ((await (await page.request.get("/api/me")).json()) as { id: string }).id;
}

/// `folderId` を `userId` に編集者として共有する。
async function shareEditor(page: Page, folderId: string, userId: string): Promise<void> {
  const res = await page.request.put(`/api/nodes/${folderId}/shares`, {
    headers: await csrf(page),
    data: { target: { type: "user", id: userId }, role: "editor" },
  });
  expect(res.status(), await res.text()).toBe(204);
}

/// `name` のテキストファイルを `folderId` に画面から置き、id を返す。
async function uploadText(page: Page, folderId: string, name: string): Promise<string> {
  await page.goto(`/drive?folder=${folderId}`);
  await page
    .locator('input[type="file"][multiple]')
    .setInputFiles({ name, mimeType: "text/plain", buffer: Buffer.from(`${name} の本文\n`, "utf8") });
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

/// 画面のアニメーション（ダイアログのフェード・下からのシート）が終わるまで待つ（撮る前に）。
async function settle(page: Page): Promise<void> {
  // 回り続けるもの（読み込みの印）は待たない。
  await page.waitForFunction(() =>
    document
      .getAnimations()
      .every((a) => a.playState !== "running" || a.effect?.getTiming().iterations === Infinity),
  );
}

/// ゴミ箱の一覧で `name` の行を出す（いま削除したものは新しい順の 1 ページ目にある）。
async function trashRow(page: Page, name: string) {
  await page.goto("/drive/trash");
  const row = page.getByRole("listitem").filter({ has: page.getByText(name, { exact: true }) });
  await expect(row).toBeVisible({ timeout: 15_000 });
  return row;
}

test("ゴミ箱: フォルダの持ち主は中のほかの人のファイルもまとめて完全に削除できる（キーボード・失敗の表示・スマホ）", async ({
  page,
  browser,
}) => {
  test.setTimeout(180_000);
  await page.setViewportSize({ width: 1440, height: 900 });
  await loginViaKeycloak(page); // alice（フォルダの持ち主・組織の管理者）
  const bobCtx = await browser.newContext();
  cleanup.add(() => bobCtx.close());
  const bob = await bobCtx.newPage();
  await loginAs(bob, "bob");

  const folderName = uniqueName("e2e-完全に削除");
  const folder = await createFolderApi(page, folderName);
  cleanup.add(() => trashFolderApi(page, folder));
  await shareEditor(page, folder, await myId(bob));
  const alicesFile = await uploadText(page, folder, `${uniqueName("alice")}.txt`);
  await uploadText(bob, folder, `${uniqueName("bob")}.txt`);
  await trashFolderApi(page, folder);

  const row = await trashRow(page, folderName);
  const purge = row.getByRole("button", { name: `「${folderName}」を完全に削除` });

  // キーボードで開く: フォーカスは「キャンセル」に来る。中の数とほかの人の数が出る。Esc で閉じると開いたボタンへ戻る。
  await purge.focus();
  await page.keyboard.press("Enter");
  const dialog = page.getByRole("dialog", { name: "完全に削除しますか？" });
  await expect(dialog).toBeVisible();
  await expect(dialog.getByRole("button", { name: "キャンセル" })).toBeFocused();
  await expect(dialog).toContainText(`フォルダ「${folderName}」と、中のファイル 2 件を完全に削除します。元に戻せません。`);
  await expect(dialog).toContainText("このうち 1 件は、ほかの人が作ったファイルです。");
  await settle(page);
  if (SHOTS) await page.screenshot({ path: `${SHOTS}/gap108b-purge-dialog-pc.png` });
  await page.keyboard.press("Escape");
  await expect(dialog).toBeHidden();
  await expect(purge).toBeFocused();

  // 失敗の表示（応答を差し替える）: 中に削除の後で作られたファイルがある 409、権限の 403。行は残る。
  const refusals: { status: number; body: unknown; text: string }[] = [
    {
      status: 409,
      body: { status: 409, title: "x", code: "live_descendants" },
      text: "このフォルダの中に、削除した後に作られたファイルがあります。フォルダを元に戻して中を確かめてください。",
    },
    { status: 403, body: { status: 403, title: "Forbidden" }, text: "持ち主か管理者だけが完全に削除できます。" },
  ];
  for (const r of refusals) {
    await page.route(`**/api/trash/${folder}`, (route) =>
      route.request().method() === "DELETE"
        ? route.fulfill({ status: r.status, contentType: "application/json", body: JSON.stringify(r.body) })
        : route.fallback(),
    );
    await purge.click();
    await dialog.getByRole("button", { name: "完全に削除" }).click();
    await expect(dialog.getByRole("alert")).toHaveText(r.text);
    await settle(page);
    if (SHOTS && r.status === 409) await page.screenshot({ path: `${SHOTS}/gap108b-purge-409-pc.png` });
    await dialog.getByRole("button", { name: "キャンセル" }).click();
    await expect(dialog).toBeHidden();
    await page.unroute(`**/api/trash/${folder}`);
  }
  await expect(row).toBeVisible();

  // スマホの幅: 行の「完全に削除」は印だけ（名前は読み上げに残る）、ダイアログは下からのシート。
  await page.setViewportSize({ width: 390, height: 844 });
  await expect(purge).toBeVisible();
  if (SHOTS) await page.screenshot({ path: `${SHOTS}/gap108b-trash-row-mobile.png` });
  await purge.click();
  await expect(dialog).toBeVisible();
  await settle(page);
  // 下からのシートでも「キャンセル」まで画面に収まる。
  await expect(dialog.getByRole("button", { name: "キャンセル" })).toBeInViewport();
  if (SHOTS) await page.screenshot({ path: `${SHOTS}/gap108b-purge-dialog-mobile.png` });

  // 本当に消す: 行が消え、知らせが出る。消したファイルは見つからない（404）。
  await dialog.getByRole("button", { name: "完全に削除" }).click();
  await expect(dialog).toBeHidden();
  await expect(page.getByText(`「${folderName}」を完全に削除しました`, { exact: true })).toBeVisible();
  await expect(page.getByText(folderName, { exact: true })).toHaveCount(0);
  expect((await page.request.get(`/api/files/${alicesFile}`)).status()).toBe(404);
  expect((await bob.request.get(`/api/files/${alicesFile}`)).status()).toBe(404);
  // 読み直しても戻らない。
  await page.reload();
  await expect(page.getByRole("heading", { name: "ゴミ箱" })).toBeVisible();
  await expect(page.getByText(folderName, { exact: true })).toHaveCount(0);
});

test("ゴミ箱: 持ち主でも管理者でもない人には「完全に削除」が出ず、空にすると自分のものだけが消える", async ({
  page,
  browser,
}) => {
  test.setTimeout(180_000);
  await page.setViewportSize({ width: 1440, height: 900 });
  await loginViaKeycloak(page); // alice
  const bobCtx = await browser.newContext();
  cleanup.add(() => bobCtx.close());
  const bob = await bobCtx.newPage();
  await bob.setViewportSize({ width: 1440, height: 900 });
  await loginAs(bob, "bob"); // 組織の管理者ではない

  // alice のフォルダを bob に編集者として共有し、bob がゴミ箱へ移す（bob の一覧に出るが、bob は消せない）。
  const sharedName = uniqueName("e2e-共有されたフォルダ");
  const shared = await createFolderApi(page, sharedName);
  cleanup.add(() => trashFolderApi(page, shared));
  await shareEditor(page, shared, await myId(bob));
  // bob 自身のフォルダ（bob が消せる）。
  const ownName = uniqueName("e2e-bob のフォルダ");
  const own = await createFolderApi(bob, ownName);
  cleanup.add(() => trashFolderApi(bob, own));
  await trashFolderApi(bob, shared);
  await trashFolderApi(bob, own);

  const sharedRow = await trashRow(bob, sharedName);
  await expect(sharedRow.getByRole("button", { name: `「${sharedName}」を復元` })).toBeVisible();
  await expect(sharedRow.getByRole("button", { name: `「${sharedName}」を完全に削除` })).toHaveCount(0);
  const ownRow = bob.getByRole("listitem").filter({ has: bob.getByText(ownName, { exact: true }) });
  await expect(ownRow.getByRole("button", { name: `「${ownName}」を完全に削除` })).toBeVisible();

  // 空にする: 確かめてから消す。終わったら消した数と残した数を言う。共有されたフォルダは残る。
  const emptyButton = bob.getByRole("button", { name: "ゴミ箱を空にする" });
  await emptyButton.click();
  const dialog = bob.getByRole("dialog", { name: "ゴミ箱を空にしますか？" });
  await expect(dialog).toContainText("持ち主か管理者だけが削除できるものは残ります。");
  await settle(bob);
  if (SHOTS) await bob.screenshot({ path: `${SHOTS}/gap108b-empty-dialog-pc.png` });
  await dialog.getByRole("button", { name: "空にする" }).click();
  await expect(dialog).toBeHidden({ timeout: 60_000 });
  const status = bob.getByRole("group", { name: "ゴミ箱の操作" }).getByRole("status");
  await expect(status).toHaveText(/^\d+ 件を削除しました。[1-9]\d* 件は持ち主か管理者だけが削除できるため残しました。$/);
  await expect(emptyButton).toBeFocused();
  await expect(bob.getByText(ownName, { exact: true })).toHaveCount(0);
  await expect(bob.getByText(sharedName, { exact: true })).toBeVisible();
  await settle(bob);
  if (SHOTS) await bob.screenshot({ path: `${SHOTS}/gap108b-empty-result-pc.png` });

  // 持ち主の alice の一覧には残っていて、alice は消せる。
  const aliceRow = await trashRow(page, sharedName);
  await expect(aliceRow.getByRole("button", { name: `「${sharedName}」を完全に削除` })).toBeVisible();
});
