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

test("/data: 参照の候補一覧（レコードの追加・絞り込みのユーザー）で Esc を押すと一覧だけ閉じ、他の欄の入力は残る（2-A-P2-2・3-A-11）", async ({ page }) => {
  await loginViaKeycloak(page);
  const res = await page.request.post("/api/data/tables", {
    headers: { "X-CSRF-Token": await csrf(page) },
    data: {
      name: uniqueName("e2e-esc-ref"),
      schema: {
        fields: [
          { key: "name", display_name: "件名", type: "text", indexed: true },
          { key: "who", display_name: "申請者", type: "user_ref", indexed: true },
        ],
      },
    },
  });
  expect(res.status(), await res.text()).toBe(201);
  const tableId = ((await res.json()) as { id: string }).id;
  await page.goto(`/data/${tableId}`);
  await expect(page.getByTestId("data-grid")).toBeVisible();

  // 1. レコードの追加の参照の候補（自前の listbox）。
  await page.getByTestId("add-record").click();
  const dialog = page.getByTestId("record-dialog");
  await dialog.getByLabel("件名").fill("入力は残る");
  await dialog.locator("#rec-who").fill("bo");
  const list = dialog.getByRole("listbox", { name: "候補" });
  await expect(list).toBeVisible();
  await page.keyboard.press("Escape");
  await expect(list).toBeHidden();
  await expect(dialog).toBeVisible();
  await expect(dialog.getByLabel("件名")).toHaveValue("入力は残る");
  // 選ばずにやめた探す語は消える（値に入ったように見せて、送るときに黙って捨てない・3-A-11）。
  await expect(dialog.locator("#rec-who")).toHaveValue("");
  // 打ったまま別の欄へ移ると、まだ選ばれていないことを知らせる。
  await dialog.locator("#rec-who").fill("bo");
  await dialog.getByLabel("件名").click();
  await expect(dialog.getByTestId("ref-picker-unpicked")).toContainText("「bo」はまだ選ばれていません");
  await page.keyboard.press("Escape");
  await expect(dialog).toBeHidden();
  await expect.poll(() => bodyPointerEvents(page)).not.toBe("none");

  // 2. 絞り込み（ポップオーバー）のユーザーの値の候補。
  await page.getByTestId("data-filter-button").click();
  await page.getByRole("button", { name: /条件を追加/ }).click();
  await page.getByLabel("条件 1 の列").click();
  await page.getByRole("option", { name: "申請者" }).click();
  const value = page.getByRole("combobox", { name: "条件 1 の値" });
  await value.fill("bo");
  const candidates = page.getByRole("listbox", { name: "候補" });
  await expect(candidates).toBeVisible();
  await page.keyboard.press("Escape");
  await expect(candidates).toBeHidden();
  await expect(page.getByTestId("data-filter-apply")).toBeVisible();
});
