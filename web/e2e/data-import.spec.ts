import { expect, test, type Page } from "@playwright/test";

import { loginViaKeycloak, uniqueName } from "./helpers";

/// 業務データの CSV 取込（監査 B3-6・一括取込の HTTP API）: テーブル画面から CSV を選んで取り込み、
/// 検証 → 適用の進捗と、取り込めなかった行の理由を見せる。合格した行は表に出る。

async function csrf(page: Page): Promise<string> {
  return (await page.context().cookies()).find((c) => c.name === "shiki_csrf")?.value ?? "";
}

async function createTable(page: Page): Promise<string> {
  const res = await page.request.post("/api/data/tables", {
    headers: { "X-CSRF-Token": await csrf(page) },
    data: {
      name: uniqueName("e2e-取込"),
      schema: {
        fields: [
          { key: "name", display_name: "件名", type: "text", indexed: true },
          { key: "amount", display_name: "金額", type: "number" },
          { key: "status", display_name: "状態", type: "select", options: ["申請中", "承認"] },
        ],
      },
    },
  });
  expect(res.status(), await res.text()).toBe(201);
  return ((await res.json()) as { id: string }).id;
}

test("CSV を取り込み、進捗と取り込めなかった行を見せる", async ({ page }) => {
  await loginViaKeycloak(page);
  const tableId = await createTable(page);
  await page.goto(`/data/${tableId}`);
  await expect(page.getByTestId("data-grid")).toBeVisible();

  await page.getByTestId("import-open").click();
  const dialog = page.getByTestId("import-dialog");
  await expect(dialog).toBeVisible();
  // ファイルを選ぶまで「取り込む」は押せない。
  await expect(page.getByTestId("import-submit")).toBeDisabled();

  // 見出しは表示名。2 行目は金額が数値でない・3 行目は選択肢に無い（どちらも行エラー）。
  const csv = "件名,金額,状態\n出張旅費,12000,申請中\n備品,abc,承認\n会議費,300,却下\n交通費,450,承認\n";
  await dialog.getByLabel("取り込む CSV ファイル").setInputFiles({
    name: "経費.csv",
    mimeType: "text/csv",
    buffer: Buffer.from(csv, "utf-8"),
  });
  await expect(dialog.getByText("経費.csv")).toBeVisible();
  const started = page.waitForResponse(
    (r) => r.url().includes(`/data/tables/${tableId}/imports/csv`) && r.request().method() === "POST",
  );
  await page.getByTestId("import-submit").click();
  expect((await started).status()).toBe(202);

  const progress = page.getByTestId("import-progress");
  await expect(progress).toHaveAttribute("data-status", "done", { timeout: 60_000 });
  await expect(progress.getByText("2 行を取り込みました。")).toBeVisible();
  const errors = page.getByTestId("import-row-errors");
  await expect(errors.locator("tbody tr")).toHaveCount(2);
  await expect(errors.locator("tbody tr").nth(0)).toContainText("2");
  await expect(errors.locator("tbody tr").nth(1)).toContainText("3");

  // 閉じると表は読み直されていて、合格した 2 行だけが見える。
  await page.getByTestId("import-close").click();
  await expect(dialog).toBeHidden();
  await expect(page.getByTestId("data-count")).toHaveText("2 件");

  // 開き直すと直前の取込の結果が出て、「別のファイルを取り込む」で選び直しに戻る。
  await page.getByTestId("import-open").click();
  await expect(page.getByTestId("import-progress")).toHaveAttribute("data-status", "done");
  await page.getByTestId("import-again").click();
  await expect(page.getByTestId("import-drop")).toBeVisible();
});

test("見出しに無い列は取込を始めずに理由を出す", async ({ page }) => {
  await loginViaKeycloak(page);
  const tableId = await createTable(page);
  await page.goto(`/data/${tableId}`);
  await page.getByTestId("import-open").click();
  const dialog = page.getByTestId("import-dialog");
  await dialog.getByLabel("取り込む CSV ファイル").setInputFiles({
    name: "unknown.csv",
    mimeType: "text/csv",
    buffer: Buffer.from("件名,存在しない列\nA,1\n", "utf-8"),
  });
  await page.getByTestId("import-submit").click();
  await expect(page.getByTestId("import-error")).toBeVisible();
  await expect(page.getByTestId("import-progress")).toHaveCount(0);
});
