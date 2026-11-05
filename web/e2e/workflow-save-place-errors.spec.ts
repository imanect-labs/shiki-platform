import { expect, test, type Page } from "@playwright/test";

import {
  cleanupAll,
  cleanupQueue,
  loginViaKeycloak,
  removeWorkflowApi,
  trashFileApi,
  trashFolderApi,
  uniqueName,
} from "./helpers";

/// ブロックの外の保存時の誤り（GAP-35）の E2E。
/// - きっかけのフォルダーが消えた定義を保存すると、知らせが「きっかけの設定」に直すところがあると言い、
///   きっかけの設定の「対象フォルダ」の下に理由が出る。キャンバスのきっかけには赤い印、上には「直すところ」の一覧が出る。
/// - スマホ幅の一覧でも、きっかけのカードに「要修正」と「直すところ」の一覧が出て、横にはみ出さない。
/// - ドライブから選んだファイルを消した定義を保存すると、ファイルの欄の下に理由が出て保存が止まる（GAP-91）。
/// 作ったもの（ワークフロー・フォルダー・ファイル）は最後に消す。`E2E_SHOTS_DIR` を渡すとスクリーンショットを撮る。

const SHOTS = process.env.E2E_SHOTS_DIR;

const cleanup = cleanupQueue();
test.afterEach(() => cleanup.run());

async function csrfHeaders(page: Page): Promise<Record<string, string>> {
  const csrf = (await page.context().cookies()).find((c) => c.name === "shiki_csrf")?.value ?? "";
  return { "x-csrf-token": csrf, "content-type": "application/json" };
}

const MISSING_FOLDER = "選んだフォルダーが見つかりません。削除されたか、見る権限がありません。フォルダーを選び直してください。";
const MISSING_FILE = "選んだファイルが見つかりません。削除されたか、見る権限がありません。ファイルを選び直してください。";

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

test("ドライブから選んだファイルを消した定義は、ファイルの欄の下に理由を出して保存を止める（GAP-91）", async ({ page }) => {
  test.setTimeout(180_000);
  await page.setViewportSize({ width: 1440, height: 900 });
  await loginViaKeycloak(page);
  const file = await page.request.post("/api/tabular/save", {
    headers: { "x-csrf-token": (await csrfHeaders(page))["x-csrf-token"] },
    data: { parent_id: null, name: `${uniqueName("e2e-台帳")}.csv`, csv: "item,amount\n交通費,1200\n" },
  });
  expect(file.ok(), await file.text()).toBeTruthy();
  const fileId = ((await file.json()) as { node_id: string }).node_id;
  cleanup.add(() => trashFileApi(page, fileId));

  // 選んだファイルが見える間は保存できる。
  const name = uniqueName("read-file-gone");
  const res = await page.request.post("/api/workflows", {
    headers: await csrfHeaders(page),
    data: {
      ir: {
        ir_version: 1,
        name,
        display_name: name,
        triggers: [{ kind: "interactive" }],
        nodes: [{ id: "read", type: "storage.read", label: "台帳を読む", params: { file: fileId } }],
        edges: [],
      },
    },
  });
  expect(res.ok(), await res.text()).toBeTruthy();
  const id = ((await res.json()) as { id: string }).id;
  cleanup.add(() => removeWorkflowApi(page, id));

  // 選んだファイルを消す（保存した後に消された・見られなくなった）。保存済みの版はそのまま残る。
  await trashFileApi(page, fileId);

  await page.goto(`/workflows/${id}`);
  const canvas = page.locator(".react-flow");
  await canvas.getByText("台帳を読む").first().click();
  const panel = page.getByRole("complementary", { name: "ブロックの設定" });
  // 変更を入れると、ライブの確かめがファイルの欄の下に理由を出す（内部の位置や code は出さない）。
  await panel.getByLabel("ブロック名").fill("台帳を読む（月次）");
  await expect(panel.getByText(MISSING_FILE)).toBeVisible({ timeout: 15_000 });
  await expect(panel).not.toContainText("/params/file");
  await expect(panel).not.toContainText("ir.unknown_file");
  await expect(canvas.locator('[aria-label="検証エラー 1 件"]')).toBeVisible();
  if (SHOTS) await page.screenshot({ path: `${SHOTS}/workflow-file-gone-pc.png` });

  // 保存は止まる（保存済みの版は変わらない）。
  const saved = page.waitForResponse((r) => r.request().method() === "PUT" && r.url().endsWith(`/workflows/${id}`));
  await page.getByRole("button", { name: "保存", exact: true }).click();
  expect((await saved).status()).toBe(400);
  await expect(page.getByText("保存できません", { exact: true }).first()).toBeVisible({ timeout: 15_000 });
  const wf = (await (await page.request.get(`/api/workflows/${id}`)).json()) as {
    version: number;
    ir: { nodes: { label?: string }[] };
  };
  expect(wf.ir.nodes[0].label).toBe("台帳を読む");
});
