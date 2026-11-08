import { expect, test, type Page } from "@playwright/test";

import { cleanupAll, loginViaKeycloak, removeWorkflowApi, trashFileApi, uniqueName } from "./helpers";

/// 「ドライブから選ぶ」のピッカー（DrivePicker）を閉じたときのフォーカスの e2e（BUG-261）:
/// 開くボタンが Radix の Trigger でない画面でも、閉じると開いたときにフォーカスがあった要素へ戻る（ページの先頭へ
/// 落ちない）。キーボードで開いて Esc で閉じたときも、ファイルを選んで閉じたときも同じ。
/// - ワークフローのファイルの欄（「読むファイル」の「変更」）
/// - チャットの添付（「+」のメニューの「ドライブから選択」。戻る先は「+」）
/// - CSV の SQL の画面（「JOIN する CSV を追加」）

const SHOTS = process.env.E2E_SHOTS_DIR;

async function csrf(page: Page): Promise<string> {
  return (await page.context().cookies()).find((c) => c.name === "shiki_csrf")?.value ?? "";
}

/// ドライブの先頭に CSV を作って id を返す（ピッカーは名前順なので、先頭近くに並ぶ名前にする）。
async function createCsv(page: Page, name: string): Promise<string> {
  const res = await page.request.post("/api/tabular/save", {
    headers: { "x-csrf-token": await csrf(page) },
    data: { parent_id: null, name, csv: "item,amount\n交通費,1200\n" },
  });
  expect(res.ok(), await res.text()).toBeTruthy();
  return ((await res.json()) as { node_id: string }).node_id;
}

test("ドライブから選ぶ: ワークフローのファイルの欄は、Esc で閉じても選んで閉じても「変更」へフォーカスが戻る（BUG-261）", async ({
  page,
}) => {
  test.setTimeout(120_000);
  await page.setViewportSize({ width: 1440, height: 900 });
  await loginViaKeycloak(page);
  const fileName = `0focus-${uniqueName("e2e-台帳")}.csv`;
  const fileId = await createCsv(page, fileName);
  let id = "";
  try {
    const name = uniqueName("picker-focus");
    const res = await page.request.post("/api/workflows", {
      headers: { "x-csrf-token": await csrf(page), "content-type": "application/json" },
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
    id = ((await res.json()) as { id: string }).id;

    await page.goto(`/workflows/${id}`);
    await page.getByTestId("rf__node-read").click();
    const panel = page.getByRole("complementary", { name: "ブロックの設定" });
    const change = panel.getByRole("button", { name: "読むファイルを変更" });
    await expect(change).toBeVisible({ timeout: 20_000 });

    // キーボードで開き、Esc で閉じる。
    await change.focus();
    await page.keyboard.press("Enter");
    const picker = page.getByRole("dialog", { name: "ファイルを選ぶ" });
    await expect(picker).toBeVisible();
    await page.keyboard.press("Escape");
    await expect(picker).toHaveCount(0);
    await expect(change).toBeFocused();

    // もう一度キーボードで開き、ファイルを選んで閉じる。
    await page.keyboard.press("Enter");
    await expect(picker).toBeVisible();
    const file = picker.getByRole("button", { name: fileName });
    await expect(file).toBeVisible({ timeout: 15_000 });
    if (SHOTS) await page.screenshot({ path: `${SHOTS}/drive-picker-focus-open-pc.png` });
    await file.focus();
    await page.keyboard.press("Enter");
    await expect(picker).toHaveCount(0);
    await expect(change).toBeFocused();
    if (SHOTS) await page.screenshot({ path: `${SHOTS}/drive-picker-focus-closed-pc.png` });
  } finally {
    await cleanupAll(
      async () => {
        if (id) await removeWorkflowApi(page, id);
      },
      () => trashFileApi(page, fileId),
    );
  }
});

test("ドライブから選ぶ: チャットの添付は、閉じると「+」へフォーカスが戻る（BUG-261）", async ({ page }) => {
  test.setTimeout(90_000);
  await page.setViewportSize({ width: 1440, height: 900 });
  await loginViaKeycloak(page);
  await page.goto("/");
  const plus = page.getByRole("button", { name: "追加メニューを開く" }).first();
  // 画面が動き出す前に押してもメニューは開かない（BUG-159）。PC の幅のメニューのボタンになるのを待つ。
  await expect(plus).toHaveAttribute("aria-haspopup", "menu", { timeout: 30_000 });

  await plus.focus();
  await page.keyboard.press("Enter");
  const item = page.getByRole("menuitem", { name: "ドライブから選択" });
  await expect(item).toBeVisible();
  await item.focus();
  await page.keyboard.press("Enter");
  const picker = page.getByRole("dialog", { name: "ドライブから選択" });
  await expect(picker).toBeVisible();
  await expect(page.getByRole("menu")).toHaveCount(0);
  await page.keyboard.press("Escape");
  await expect(picker).toHaveCount(0);
  await expect(plus).toBeFocused();
});

test("ドライブから選ぶ: CSV の SQL の画面は、閉じると「JOIN する CSV を追加」へフォーカスが戻る（BUG-261）", async ({
  page,
}) => {
  test.setTimeout(90_000);
  await page.setViewportSize({ width: 1440, height: 900 });
  await loginViaKeycloak(page);
  const fileId = await createCsv(page, `${uniqueName("e2e-注文")}.csv`);
  try {
    await page.goto(`/csv/${fileId}`);
    await expect(page.getByTestId("csv-grid")).toBeVisible({ timeout: 20_000 });
    await page.getByTestId("csv-tab-sql").click();
    await expect(page.getByTestId("sql-console")).toBeVisible();

    const add = page.getByTestId("sql-add-table");
    await add.focus();
    await page.keyboard.press("Enter");
    const picker = page.getByRole("dialog", { name: "JOIN する CSV を選択" });
    await expect(picker).toBeVisible();
    await page.keyboard.press("Escape");
    await expect(picker).toHaveCount(0);
    await expect(add).toBeFocused();
  } finally {
    await cleanupAll(() => trashFileApi(page, fileId));
  }
});
