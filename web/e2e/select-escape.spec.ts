import { expect, test, type Locator, type Page } from "@playwright/test";

import { loginViaKeycloak, uniqueName } from "./helpers";

/// ダイアログ・ポップオーバーの中の選択肢（Radix Select）で Esc を押したとき（A-1）:
/// 選択肢だけが閉じ、外側のダイアログは開いたまま・入力は残る。外側を閉じたあと画面が操作できる
/// （`body{pointer-events:none}` が残らない）。Radix の DismissableLayer が 2 版混在すると、層の重なりを
/// 別々に数えて Esc が外側まで閉じ、片方の層の後始末が残って画面全体が押せなくなっていた。

async function csrf(page: Page): Promise<string> {
  return (await page.context().cookies()).find((c) => c.name === "shiki_csrf")?.value ?? "";
}

async function bodyPointerEvents(page: Page): Promise<string> {
  return page.evaluate(() => getComputedStyle(document.body).pointerEvents);
}

/// 選択肢を開いて Esc → 選択肢だけ閉じる → もう一度 Esc で外側が閉じ、画面が操作できる。
async function escapeClosesOnlySelect(page: Page, trigger: Locator, outer: Locator, closeOuter?: () => Promise<void>) {
  await trigger.click();
  const listbox = page.getByRole("listbox");
  await expect(listbox).toBeVisible();
  await page.keyboard.press("Escape");
  await expect(listbox).toBeHidden();
  await expect(outer).toBeVisible();
  // 外側を閉じる（既定は Esc）。
  if (closeOuter) await closeOuter();
  else await page.keyboard.press("Escape");
  await expect(outer).toBeHidden();
  await expect.poll(() => bodyPointerEvents(page)).not.toBe("none");
}

async function seedTable(page: Page): Promise<string> {
  const res = await page.request.post("/api/data/tables", {
    headers: { "X-CSRF-Token": await csrf(page) },
    data: {
      name: uniqueName("e2e-esc"),
      schema: {
        fields: [
          { key: "name", display_name: "件名", type: "text", indexed: true },
          { key: "status", display_name: "状態", type: "select", options: ["申請中", "承認"] },
        ],
      },
    },
  });
  expect(res.status(), await res.text()).toBe(201);
  return ((await res.json()) as { id: string }).id;
}

test("/data: スキーマ編集・レコード追加・絞り込みの選択肢で Esc を押しても外側は閉じず、画面は操作できる", async ({ page }) => {
  await loginViaKeycloak(page);
  const tableId = await seedTable(page);
  await page.goto(`/data/${tableId}`);
  await expect(page.getByTestId("data-grid")).toBeVisible();

  // 1. レコードの追加（ダイアログ）の単一選択。
  await page.getByTestId("add-record").click();
  const recordDialog = page.getByTestId("record-dialog");
  await recordDialog.getByLabel("件名").fill("入力は残る");
  await escapeClosesOnlySelect(page, recordDialog.getByRole("combobox").first(), recordDialog, async () => {
    await expect(recordDialog.getByLabel("件名")).toHaveValue("入力は残る");
    await page.keyboard.press("Escape");
  });

  // 2. 絞り込み（ポップオーバー）の列の選択。
  await page.getByRole("button", { name: /絞り込み/ }).first().click();
  const addCond = page.getByRole("button", { name: /条件を追加/ });
  if (await addCond.isVisible()) await addCond.click();
  const popover = page.getByRole("dialog").filter({ has: page.getByRole("combobox") }).last();
  await escapeClosesOnlySelect(page, popover.getByRole("combobox").first(), popover);

  // 3. 列の編集（ダイアログ）の型の選択。
  await page.getByTestId("schema-editor-open").click();
  const editor = page.getByTestId("schema-editor");
  await page.getByTestId("schema-add-field").click();
  await escapeClosesOnlySelect(page, page.getByLabel("新しい列の型"), editor);

  // 4. 画面が操作できる（行の追加ダイアログが開く）。
  await page.getByTestId("add-record").click();
  await expect(recordDialog).toBeVisible();
});

test("ドライブ: 共有ダイアログの権限の選択肢で Esc を押しても共有ダイアログは閉じない", async ({ page }) => {
  await loginViaKeycloak(page);
  await page.goto("/drive");
  const folder = uniqueName("esc-共有");
  await page.getByRole("button", { name: "新規フォルダ" }).click();
  const dialog = page.getByRole("dialog");
  await dialog.getByRole("textbox").fill(folder);
  await dialog.getByRole("button", { name: "作成" }).click();
  await expect(page.getByText(folder, { exact: true })).toBeVisible();
  await page.getByRole("button", { name: `「${folder}」の操作` }).click();
  await page.getByRole("menuitem", { name: "共有" }).click();
  const share = page.getByRole("dialog");
  await expect(share.getByText(`「${folder}」を共有`)).toBeVisible();
  await escapeClosesOnlySelect(page, share.getByRole("combobox").first(), share);
  // 画面が操作できる（もう一度メニューを開ける）。
  await page.getByRole("button", { name: `「${folder}」の操作` }).click();
  await expect(page.getByRole("menuitem", { name: "共有" })).toBeVisible();
});

test("ワークフロー: 有効化ダイアログのテーブルの選択肢で Esc を押しても有効化ダイアログは閉じない", async ({ page }) => {
  await loginViaKeycloak(page);
  await seedTable(page);
  const name = uniqueName("esc-wf");
  const res = await page.request.post("/api/workflows", {
    headers: { "x-csrf-token": await csrf(page) },
    data: {
      ir: {
        ir_version: 1,
        name,
        display_name: name,
        declared_scopes: ["data.read"],
        triggers: [{ kind: "schedule", cron: "0 9 * * *", tz: "Asia/Tokyo", catchup: "skip" }],
        nodes: [{ id: "q", type: "data.query", label: "調べる", params: { table: { $from: "input", path: "/table" }, query: { limit: 5 } } }],
        edges: [],
      },
    },
  });
  expect(res.ok(), await res.text()).toBeTruthy();
  const wfId = ((await res.json()) as { id: string }).id;
  await page.goto(`/workflows/${wfId}`);
  await page.getByRole("button", { name: /自動実行を有効化/ }).click();
  const dialog = page.locator('[role="dialog"]');
  await expect(dialog.getByText("自動実行の設定")).toBeVisible();
  const pick = dialog.getByRole("combobox").first();
  await escapeClosesOnlySelect(page, pick, dialog);
});
