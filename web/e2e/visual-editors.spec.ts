import { expect, test } from "@playwright/test";

import { loginViaKeycloak, uniqueName } from "./helpers";

/// ノート／CSV エディタの視覚確認専用キャプチャ（SHOTS_DIR 設定時のみ）。
/// 例: SHOTS_DIR=/tmp/shots pnpm exec playwright test visual-editors
const SHOTS = process.env.SHOTS_DIR;

test.describe.configure({ timeout: 120_000 });

test("ノートエディタのスクショ", async ({ page }) => {
  test.skip(!SHOTS, "SHOTS_DIR 未設定");
  await loginViaKeycloak(page);
  await page.goto("/drive");
  await page.waitForLoadState("networkidle");

  // 新規作成 > ノート。
  await page.getByRole("button", { name: "新規作成" }).click();
  await page.getByRole("menuitem", { name: "ノート" }).click();
  await page.waitForURL(/\/notes\/[0-9a-f-]+/i, { timeout: 60_000 });
  await page.getByTestId("note-editor").waitFor({ timeout: 60_000 });

  // メタデータ（アイコン・タイトル・タグ・プロパティ）。
  await page.getByLabel("アイコン").fill("🚀");
  await page.getByLabel("タイトル").fill("週次プロダクトレビュー");
  const tag = page.getByLabel("タグを追加");
  await tag.fill("プロダクト");
  await tag.press("Enter");
  await tag.fill("2026Q3");
  await tag.press("Enter");
  await page.getByRole("button", { name: "プロパティを追加" }).click();
  await page.getByLabel("プロパティ名").fill("担当");
  await page.getByLabel("プロパティ値").fill("あなた");
  await page.getByLabel("プロパティ値").press("Enter");

  // 本文（markdown 入力規則で見出し/リスト/引用/コードを作る）。
  const editor = page.getByTestId("note-editor");
  await editor.click();
  const type = async (text: string) => {
    await page.keyboard.type(text, { delay: 6 });
  };
  await type("四半期の目標に対する進捗と次アクションをまとめます。");
  await page.keyboard.press("Enter");
  await type("## 今週のハイライト");
  await page.keyboard.press("Enter");
  await type("- 新規サインアップが前週比 +18% で着地");
  await page.keyboard.press("Enter");
  await type("CSV エディタのベータを社内公開");
  await page.keyboard.press("Enter");
  await type("ノートの共同編集がプレビュー開始");
  await page.keyboard.press("Enter");
  await page.keyboard.press("Enter"); // リストを抜ける
  await type("## 意思決定");
  await page.keyboard.press("Enter");
  await type("> オンボーディング改善を最優先とする（承認済み）");
  await page.keyboard.press("Enter");
  await page.keyboard.press("Enter");
  await type("## 次アクション");
  await page.keyboard.press("Enter");
  await type("1. 招待フローの計測を追加");
  await page.keyboard.press("Enter");
  await type("SQL コンソールのテンプレを用意");
  await page.keyboard.press("Enter");
  await page.keyboard.press("Enter");
  // コードブロック（``` + 空白で入力規則が発火する）。
  await type("``` ");
  await type("SELECT plan, count(*) FROM signups GROUP BY 1;");
  await page.waitForTimeout(500);
  await page.screenshot({ path: `${SHOTS}/note-01-editor.png`, fullPage: true });

  // スラッシュメニューを開いたところ。
  await editor.click();
  await page.keyboard.press("End");
  await page.keyboard.press("Enter");
  await page.keyboard.type("/");
  await page.waitForTimeout(600);
  await page.screenshot({ path: `${SHOTS}/note-02-slash.png` });
  await page.keyboard.press("Escape");

  // チャットパネルを開いたところ。
  await page.getByTestId("note-ask-ai").click();
  await page.waitForTimeout(800);
  await page.screenshot({ path: `${SHOTS}/note-03-chat.png`, fullPage: true });
});

test("ドライブ一覧＋版履歴のスクショ（更新者/作者）", async ({ page }) => {
  test.skip(!SHOTS, "SHOTS_DIR 未設定");
  await loginViaKeycloak(page);
  await page.goto("/drive");
  await page.waitForLoadState("networkidle");
  await page.waitForTimeout(800);
  await page.screenshot({ path: `${SHOTS}/drive-01-list.png`, fullPage: true });

  // 版履歴ダイアログ（作者表示・11P.10）。txt を 2 版アップロードして開く。
  const fileName = `${uniqueName("版")}.txt`;
  await page.locator('input[type="file"][multiple]').setInputFiles({
    name: fileName,
    mimeType: "text/plain",
    buffer: Buffer.from("v1\n"),
  });
  await expect(page.getByText(fileName, { exact: true })).toBeVisible({ timeout: 20_000 });
  await page.getByRole("button", { name: `「${fileName}」の操作` }).click();
  const [chooser] = await Promise.all([
    page.waitForEvent("filechooser"),
    page.getByRole("menuitem", { name: "新しいバージョン" }).click(),
  ]);
  await chooser.setFiles({ name: fileName, mimeType: "text/plain", buffer: Buffer.from("v2 longer\n") });
  await expect(page.getByText("新しいバージョンをアップロードしました").first()).toBeVisible({
    timeout: 20_000,
  });
  await page.getByRole("button", { name: `「${fileName}」の操作` }).click();
  await page.getByRole("menuitem", { name: "版履歴" }).click();
  await expect(page.getByRole("dialog").getByText("バージョン 2")).toBeVisible({ timeout: 10_000 });
  await page.waitForTimeout(400);
  await page.screenshot({ path: `${SHOTS}/drive-02-versions.png` });
});

test("CSVエディタのスクショ", async ({ page }) => {
  test.skip(!SHOTS, "SHOTS_DIR 未設定");
  await loginViaKeycloak(page);
  await page.goto("/drive");
  await page.waitForLoadState("networkidle");

  // データ入り CSV をアップロード（グリッドに中身を出すため）。
  const fileName = `${uniqueName("売上")}.csv`;
  const rows = [
    "地域,担当,四半期,売上,達成率",
    "東京,佐藤,Q1,1250000,112",
    "大阪,鈴木,Q1,980000,98",
    "名古屋,高橋,Q1,760000,88",
    "福岡,田中,Q1,540000,105",
    "札幌,伊藤,Q1,430000,76",
    "東京,佐藤,Q2,1380000,121",
    "大阪,鈴木,Q2,1010000,101",
    "名古屋,高橋,Q2,820000,94",
    "福岡,田中,Q2,610000,118",
    "札幌,伊藤,Q2,470000,83",
  ].join("\n");
  await page.locator('input[type="file"][multiple]').setInputFiles({
    name: fileName,
    mimeType: "text/csv",
    buffer: Buffer.from(rows + "\n"),
  });
  await expect(page.getByText(fileName, { exact: true })).toBeVisible({ timeout: 20_000 });

  // 開く → CSV エディタ（行の主ボタンをクリック。トーストのテキストと衝突させない）。
  await page.getByRole("button", { name: fileName, exact: true }).click();
  await page.waitForURL(/\/csv\/[0-9a-f-]+/i, { timeout: 60_000 });
  await page.waitForTimeout(1500);
  await page.screenshot({ path: `${SHOTS}/csv-01-grid.png`, fullPage: true });

  // SQL コンソール。
  await page.getByTestId("csv-tab-sql").click();
  await page.waitForTimeout(500);
  const sql = page.locator("textarea").first();
  await sql.click();
  await sql.fill("SELECT 地域, sum(売上) AS 合計 FROM data GROUP BY 地域 ORDER BY 合計 DESC");
  // 実行ボタン（Cmd/Ctrl+Enter か 実行ボタン）。
  await page.keyboard.press("Control+Enter");
  await page.waitForTimeout(1500);
  await page.screenshot({ path: `${SHOTS}/csv-02-sql.png`, fullPage: true });
});
