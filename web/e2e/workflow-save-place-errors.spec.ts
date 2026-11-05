import { expect, test, type Page } from "@playwright/test";

import { cleanupAll, cleanupQueue, loginViaKeycloak, removeWorkflowApi, trashFolderApi, uniqueName } from "./helpers";

/// ブロックの外の保存時の誤り（GAP-35）の E2E。
/// - きっかけのフォルダーが消えた定義を保存すると、知らせが「きっかけの設定」に直すところがあると言い、
///   きっかけの設定の「対象フォルダ」の下に理由が出る。キャンバスのきっかけには赤い印、上には「直すところ」の一覧が出る。
/// - スマホ幅の一覧でも、きっかけのカードに「要修正」と「直すところ」の一覧が出て、横にはみ出さない。
/// 作ったもの（ワークフロー・フォルダー）は最後に消す。`E2E_SHOTS_DIR` を渡すとスクリーンショットを撮る。

const SHOTS = process.env.E2E_SHOTS_DIR;

const cleanup = cleanupQueue();
test.afterEach(() => cleanup.run());

async function csrfHeaders(page: Page): Promise<Record<string, string>> {
  const csrf = (await page.context().cookies()).find((c) => c.name === "shiki_csrf")?.value ?? "";
  return { "x-csrf-token": csrf, "content-type": "application/json" };
}

const MISSING_FOLDER = "選んだフォルダーが見つかりません。削除されたか、見る権限がありません。フォルダーを選び直してください。";

test("消えたフォルダーをきっかけにした定義は、きっかけの設定の欄の下に理由を出して保存を止める（GAP-35）", async ({
  page,
  browser,
}) => {
  test.setTimeout(180_000);
  await page.setViewportSize({ width: 1440, height: 900 });
  await loginViaKeycloak(page);
  const created = await page.request.post("/api/folders", {
    headers: await csrfHeaders(page),
    data: { name: uniqueName("e2e-受付"), parent_id: null },
  });
  expect(created.ok(), await created.text()).toBeTruthy();
  const folder = (await created.json()) as { id: string };
  cleanup.add(() => trashFolderApi(page, folder.id));

  const name = uniqueName("trigger-folder-gone");
  const res = await page.request.post("/api/workflows", {
    headers: await csrfHeaders(page),
    data: {
      ir: {
        ir_version: 1,
        name,
        display_name: name,
        declared_scopes: [],
        triggers: [{ kind: "event", source: "storage.write", scope: { folder: folder.id } }],
        nodes: [
          {
            id: "pick",
            type: "script.run",
            label: "受け取る",
            params: { source: { inline: "function main(input) { return { ok: true }; }" } },
          },
        ],
        edges: [],
      },
    },
  });
  expect(res.ok(), await res.text()).toBeTruthy();
  const id = ((await res.json()) as { id: string }).id;
  cleanup.add(() => removeWorkflowApi(page, id));

  // きっかけのフォルダーを消す（保存した後に消された・見られなくなった）。
  await trashFolderApi(page, folder.id);

  await page.goto(`/workflows/${id}`);
  const canvas = page.locator(".react-flow");
  await expect(canvas.getByText("受け取る").first()).toBeVisible({ timeout: 20_000 });
  await canvas.getByText("ファイルが保存されたとき").first().click();
  const triggerPanel = page.getByRole("complementary", { name: "きっかけの設定" });
  // 変更を入れて保存する（手動の実行のきっかけを足す）。
  await triggerPanel.getByRole("button", { name: "きっかけを追加" }).click();
  await page.getByRole("button", { name: "保存", exact: true }).click();
  await expect(page.getByText("保存できません", { exact: true }).first()).toBeVisible({ timeout: 15_000 });
  await expect(
    page.getByText("きっかけの設定に直すところがあります。上の「直すところ」から開けます。").first(),
  ).toBeVisible();

  // 理由はきっかけの設定の「対象フォルダ」の下に出る（内部の位置や code は出さない）。
  await canvas.getByText("ファイルが保存されたとき").first().click();
  await expect(triggerPanel.getByText(MISSING_FOLDER)).toBeVisible({ timeout: 10_000 });
  await expect(triggerPanel).not.toContainText("/triggers/");
  await expect(triggerPanel).not.toContainText("ir.unknown_folder");
  // 欄は消えたフォルダーの id を出さず、選び直しを促す。
  await expect(triggerPanel.getByPlaceholder("見つからないフォルダーです。選び直してください")).toHaveValue("");
  // キャンバスのきっかけに赤い印、上に「直すところ」の一覧。一覧から押すと、きっかけの設定を開く。
  await expect(canvas.getByRole("img", { name: "直すところ 1 件" })).toBeVisible();
  const summary = page.getByTestId("workflow-errors");
  await expect(summary).toHaveText("直すところ 1 件");
  if (SHOTS) await page.screenshot({ path: `${SHOTS}/workflow-place-errors-trigger-pc.png` });
  await page.getByRole("button", { name: "パネルを閉じる" }).click();
  await summary.click();
  const list = page.getByTestId("workflow-errors-list");
  await expect(list).toContainText("きっかけ（できごと）");
  await expect(list).toContainText(MISSING_FOLDER);
  // 場所は「きっかけ（できごと）」が言うので、文言の頭に「きっかけ: 」を重ねない。
  await expect(list).not.toContainText("きっかけ: ");
  if (SHOTS) {
    await page.waitForTimeout(400); // 開くときの動きが終わってから撮る。
    await page.screenshot({ path: `${SHOTS}/workflow-place-errors-summary-pc.png` });
  }
  await list.getByRole("button", { name: /きっかけ（できごと）/ }).click();
  await expect(triggerPanel.getByText(MISSING_FOLDER)).toBeVisible();

  // スマホ幅の一覧: きっかけのカードに「要修正」、上に「直すところ」。横にはみ出さない。
  const ctx = await browser.newContext({
    storageState: await page.context().storageState(),
    viewport: { width: 390, height: 844 },
    hasTouch: true,
    isMobile: true,
    deviceScaleFactor: 2,
    ignoreHTTPSErrors: true,
  });
  try {
    const phone = await ctx.newPage();
    await phone.goto(`/workflows/${id}`);
    const flow = phone.getByTestId("workflow-mobile-flow");
    await expect(flow).toBeVisible({ timeout: 20_000 });
    await flow.getByRole("button", { name: /できごと/ }).first().click();
    await phone.getByRole("button", { name: "きっかけを追加" }).click();
    await phone.getByRole("button", { name: "閉じる" }).click();
    // 変更を入れるとライブ検証が同じ誤りを返す（保存しなくても場所が分かる）。
    await expect(phone.getByTestId("workflow-errors")).toHaveText("直すところ 1 件", { timeout: 15_000 });
    await expect(flow.getByRole("button", { name: /できごと/ }).first()).toContainText("要修正 1");
    const overflow = await phone.evaluate(() => document.documentElement.scrollWidth - window.innerWidth);
    expect(overflow).toBeLessThanOrEqual(0);
    if (SHOTS) await phone.screenshot({ path: `${SHOTS}/workflow-place-errors-phone.png` });
    await flow.getByRole("button", { name: /できごと/ }).first().click();
    await expect(phone.getByText(MISSING_FOLDER)).toBeVisible({ timeout: 10_000 });
    if (SHOTS) await phone.screenshot({ path: `${SHOTS}/workflow-place-errors-sheet-phone.png` });
  } finally {
    await cleanupAll(() => ctx.close());
  }
});
