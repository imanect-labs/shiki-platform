import { readFile } from "node:fs/promises";

import { expect, test } from "@playwright/test";

import { loginViaKeycloak, uniqueName } from "./helpers";

/// 業務データの CSV: CSV からテーブルを作る（列名・型の推定 → 作成 → 取込）と、テーブルを CSV に書き出す
/// （見出し行つき・取込と往復できる形）。

test("CSV からテーブルを作り、書き出した CSV が元の行を含む", async ({ page }) => {
  await loginViaKeycloak(page);
  await page.goto("/data");
  await page.getByTestId("create-table-csv").click();
  const dialog = page.getByTestId("create-from-csv-dialog");
  await expect(dialog).toBeVisible();
  // ファイルを選ぶまで作成できない。
  await expect(page.getByTestId("csv-create-submit")).toBeDisabled();

  // 日付は Excel が書く `2024/1/5` の形も日付として推定し、`YYYY-MM-DD` に直して取り込む。
  const csv = "件名,金額,日付,メモ\n出張旅費,12000,2024/1/5,\"東京, 大阪\"\n備品,300,2024-02-03,=SUM(A1)\n";
  await dialog.getByLabel("テーブルにする CSV ファイル").setInputFiles({
    name: "経費.csv",
    mimeType: "text/csv",
    buffer: Buffer.from(csv, "utf-8"),
  });
  const columns = page.getByTestId("csv-create-column");
  await expect(columns).toHaveCount(4);
  await expect(columns.nth(1).getByRole("combobox")).toHaveText("数値");
  await expect(columns.nth(2).getByRole("combobox")).toHaveText("日付");
  await expect(columns.nth(3).getByRole("combobox")).toHaveText("テキスト");
  // テーブル名はファイル名から。列名は直せる。
  const name = uniqueName("e2e-CSV作成");
  await dialog.getByLabel("テーブル名").fill(name);
  await columns.nth(3).getByLabel("4 列目の列名").fill("備考");

  await page.getByTestId("csv-create-submit").click();
  await page.waitForURL(/\/data\/[0-9a-f-]{36}$/);
  await expect(page.getByTestId("data-grid")).toBeVisible();
  await expect(page.getByTestId("data-count")).toHaveText("2 件", { timeout: 60_000 });

  // 書き出し: 見出しは表示名、値は取込と往復できる形。数式に見える文字列は `'` を付けて無害にする。
  const download = page.waitForEvent("download");
  await page.getByTestId("export-csv").click();
  const file = await download;
  // ファイル名はテーブル名（`<名前>.csv`）。版の合わない headless shell は ASCII 以外のファイル名を
  // `download` に置き換えるので、それも許す。
  expect([`${name}.csv`, "download"]).toContain(file.suggestedFilename());
  const text = (await readFile((await file.path())!, "utf-8")).replace(/^﻿/, "");
  const lines = text.trimEnd().split("\r\n");
  expect(lines[0]).toBe("件名,金額,日付,備考");
  expect(lines.slice(1).sort()).toEqual(
    ['出張旅費,12000,2024-01-05,"東京, 大阪"', "備品,300,2024-02-03,'=SUM(A1)"].sort(),
  );
});

test("見出しと列の数が合わない CSV は作成に進まず理由を出す", async ({ page }) => {
  await loginViaKeycloak(page);
  await page.goto("/data");
  await page.getByTestId("create-table-csv").click();
  const dialog = page.getByTestId("create-from-csv-dialog");
  await dialog.getByLabel("テーブルにする CSV ファイル").setInputFiles({
    name: "bad.csv",
    mimeType: "text/csv",
    buffer: Buffer.from("a,b\n1,2\n3\n", "utf-8"),
  });
  await expect(page.getByTestId("csv-create-error")).toContainText("3 行目");
  await expect(page.getByTestId("csv-create-submit")).toBeDisabled();
});
