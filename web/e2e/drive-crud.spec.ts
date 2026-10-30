import { expect, test, type Locator } from "@playwright/test";

import { cleanupAll, createFolderApi, loginViaKeycloak, trashFolderApi, uniqueName } from "./helpers";

/// Drive の CRUD（フォルダ作成→アップロード→リネーム→移動→削除→ゴミ箱復元）を
/// UI から一気通貫で検証する（issue #20 受け入れ条件: CRUD が UI から完結）。
///
/// ドライブのトップに作業用のフォルダーを 1 つ作り、その中で操作する。トップに別のファイルやフォルダーがたまって
/// いても（名前の順の最初の 50 件に入らなくても）左右されない。作業用のフォルダーは最後にゴミ箱へ移す（BUG-125）。

/// 移動先の一覧で、名前のフォルダーへ入る（一覧に無ければ「もっと読み込む」で続きを読む）。
async function openInMoveDialog(dialog: Locator, name: string): Promise<void> {
  const folder = dialog.getByRole("button", { name, exact: true });
  const more = dialog.getByRole("button", { name: "もっと読み込む" });
  await expect(folder.or(more).first()).toBeVisible({ timeout: 15_000 });
  for (let i = 0; i < 50 && !(await folder.isVisible()); i += 1) {
    await more.click();
    await expect(folder.or(more).first()).toBeVisible({ timeout: 15_000 });
  }
  await folder.click();
}

test("ドライブの CRUD がUIから完結する", async ({ page }) => {
  test.setTimeout(120_000);
  await loginViaKeycloak(page);
  const workName = uniqueName("e2e-drive-crud");
  const work = await createFolderApi(page, workName);
  try {
    await page.goto(`/drive?folder=${work}`);

    const folder = uniqueName("フォルダ");
    const fileName = `${uniqueName("メモ")}.txt`;
    const renamed = `${uniqueName("改名")}.txt`;

    // --- フォルダ作成 ---
    await page.getByRole("button", { name: "新規フォルダ" }).click();
    const newFolderDialog = page.getByRole("dialog");
    await newFolderDialog.getByRole("textbox").fill(folder);
    await newFolderDialog.getByRole("button", { name: "作成" }).click();
    await expect(page.getByText(folder, { exact: true })).toBeVisible();

    // --- アップロード（作業用のフォルダーの直下） ---
    await page.locator('input[type="file"][multiple]').setInputFiles({
      name: fileName,
      mimeType: "text/plain",
      buffer: Buffer.from("hello shiki drive e2e\n"),
    });
    await expect(page.getByText(fileName, { exact: true })).toBeVisible({ timeout: 20_000 });

    // --- リネーム ---
    await page.getByRole("button", { name: `「${fileName}」の操作` }).click();
    await page.getByRole("menuitem", { name: "名前を変更" }).click();
    const renameDialog = page.getByRole("dialog");
    await renameDialog.getByRole("textbox").fill(renamed);
    await renameDialog.getByRole("button", { name: "変更" }).click();
    await expect(page.getByText(renamed, { exact: true })).toBeVisible();

    // --- 移動（作成したフォルダへ） ---
    await page.getByRole("button", { name: `「${renamed}」の操作` }).click();
    await page.getByRole("menuitem", { name: "移動", exact: true }).click();
    const moveDialog = page.getByRole("dialog");
    // 移動先は、ファイルがいまある作業用のフォルダーから選ぶ（GAP-48）。作成したフォルダへ入り、「ここへ移動」で確定する。
    await expect(moveDialog.getByRole("navigation", { name: "移動先の階層" })).toContainText(workName);
    await openInMoveDialog(moveDialog, folder);
    await moveDialog.getByRole("button", { name: "ここへ移動" }).click();
    await expect(moveDialog).toBeHidden();
    // 作業用のフォルダーの直下からは消える。
    await expect(page.getByText(renamed, { exact: true })).toHaveCount(0);

    // フォルダを開くと中に移動済みのファイルがある。
    await page.getByText(folder, { exact: true }).click();
    await expect(page.getByText(renamed, { exact: true })).toBeVisible();

    // --- フォルダ削除（パンくずで作業用のフォルダーへ戻ってから） ---
    await page.getByRole("navigation", { name: "パンくず" }).getByRole("button", { name: workName }).click();
    await page.getByRole("button", { name: `「${folder}」の操作` }).click();
    await page.getByRole("menuitem", { name: "ゴミ箱へ移動" }).click();
    await page.getByRole("dialog").getByRole("button", { name: "ゴミ箱へ移動" }).click();
    await expect(page.getByText(folder, { exact: true })).toHaveCount(0);

    // --- ゴミ箱から復元（ゴミ箱にほかのものがあっても、このフォルダーの行の「復元」を押す） ---
    await page.goto("/drive/trash");
    await expect(page.getByText(folder, { exact: true })).toBeVisible();
    await page.getByRole("button", { name: `「${folder}」を復元` }).click();
    await expect(page.getByText(folder, { exact: true })).toHaveCount(0);

    // 作業用のフォルダーに戻ると復元されている。
    await page.goto(`/drive?folder=${work}`);
    await expect(page.getByText(folder, { exact: true })).toBeVisible();
  } finally {
    await cleanupAll(() => trashFolderApi(page, work));
  }
});

/// 作ったフォルダー・アップロードしたファイル・名前を変えたファイルは、一覧の最初のページ（名前の順に 50 件）の外でも、
/// 作った直後に見える（続きを読み込み、その行へ移る・BUG-125）。以前は、スクロールして続きを読むまで現れなかった。
test("ドライブ: 作ったものは一覧の最初のページの外でも、作った直後に見える（BUG-125）", async ({ page }) => {
  test.setTimeout(180_000);
  await page.setViewportSize({ width: 1440, height: 900 });
  await loginViaKeycloak(page);
  const work = await createFolderApi(page, uniqueName("e2e-drive-reveal"));
  try {
    // 名前の順で先に並ぶフォルダーを、最初のページ（50 件）より多く置く。
    for (let i = 1; i <= 55; i += 1) await createFolderApi(page, `a-${String(i).padStart(3, "0")}`, work);
    await page.goto(`/drive?folder=${work}`);
    await expect(page.getByText("a-001", { exact: true })).toBeVisible({ timeout: 15_000 });

    // 名前の順で最後に並ぶフォルダーを作る → 続きを読み込んで、その行が見える（画面の中にある）。
    const folder = uniqueName("z-フォルダ");
    await page.getByRole("button", { name: "新規フォルダ" }).click();
    const dialog = page.getByRole("dialog");
    await dialog.getByRole("textbox").fill(folder);
    await dialog.getByRole("button", { name: "作成" }).click();
    const folderRow = page.getByText(folder, { exact: true });
    await expect(folderRow).toBeInViewport({ timeout: 15_000 });

    // アップロードしたファイルも、名前の順で最後なら同じように見える。
    await page.evaluate(() => window.scrollTo(0, 0));
    const file = `${uniqueName("zz-メモ")}.txt`;
    await page.locator('input[type="file"][multiple]').setInputFiles({
      name: file,
      mimeType: "text/plain",
      buffer: Buffer.from("reveal\n"),
    });
    await expect(page.getByText(file, { exact: true })).toBeInViewport({ timeout: 20_000 });

    // 名前を変えて最初のページの外へ移っても、変えた直後に見える。
    await page.goto(`/drive?folder=${work}`);
    await expect(page.getByText("a-001", { exact: true })).toBeVisible({ timeout: 15_000 });
    await page.getByRole("button", { name: "「a-001」の操作" }).click();
    await page.getByRole("menuitem", { name: "名前を変更" }).click();
    const renamed = uniqueName("zzz-改名");
    const renameDialog = page.getByRole("dialog");
    await renameDialog.getByRole("textbox").fill(renamed);
    await renameDialog.getByRole("button", { name: "変更" }).click();
    await expect(page.getByText(renamed, { exact: true })).toBeInViewport({ timeout: 15_000 });
    if (process.env.E2E_SHOTS_DIR) await page.screenshot({ path: `${process.env.E2E_SHOTS_DIR}/bug125-drive-reveal-pc.png` });

    // 名前の検索中に、検索に合わない名前で作ったものは一覧に来ないので、続きを読み込んで探さない（読み直すだけ）。
    await page.goto(`/drive?folder=${work}&q=a-0`);
    await expect(page.getByText("a-002", { exact: true })).toBeVisible({ timeout: 15_000 });
    const pagesAfter: string[] = [];
    const watch = (req: { url: () => string }) => {
      const url = new URL(req.url());
      if (url.pathname === "/api/nodes" && url.searchParams.has("cursor")) pagesAfter.push(url.search);
    };
    await page.getByRole("button", { name: "新規フォルダ" }).click();
    const searchDialog = page.getByRole("dialog");
    await searchDialog.getByRole("textbox").fill(uniqueName("y-検索に合わない"));
    page.on("request", watch);
    const reloaded = page.waitForResponse((r) => {
      const url = new URL(r.url());
      return url.pathname === "/api/nodes" && url.searchParams.get("q") === "a-0" && !url.searchParams.has("cursor");
    });
    await searchDialog.getByRole("button", { name: "作成" }).click();
    await reloaded;
    await page.waitForTimeout(2_000);
    page.off("request", watch);
    expect(pagesAfter, "検索に合わないものを探して続きを読み込まない").toEqual([]);
  } finally {
    await cleanupAll(() => trashFolderApi(page, work));
  }
});
