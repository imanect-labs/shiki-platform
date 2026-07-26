import { copyFileSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import path from "node:path";

import { expect, test, type Page } from "@playwright/test";

import { loginViaKeycloak } from "./helpers";

/// **フルツール・複雑タスク**のデモ動画（AI_FULL=1）。実 LLM＋実 SearXNG（web_search）＋
/// web_fetch（ホストネイティブ取得・#348）＋code_interpreter（gVisor・numpy/pandas 同梱）＋
/// csv.query（隔離 DuckDB）＋ドキュメント編集ツールを一つのタスクで横断させる。
/// 冪等 read（web_search/web_fetch/doc_search）は同一ステップ内で並列実行される（#349）。
/// 前提: `scripts/e2e-deep-host.env` の構成で api を起動（SearXNG :8099・sandbox :50000・
/// Langfuse :3002）。トレースは Langfuse UI（http://localhost:3002）で確認できる。
test.skip(process.env.AI_FULL !== "1", "フルツール複雑タスク動画（AI_FULL=1）");
test.use({
  locale: "ja-JP",
  viewport: { width: 1280, height: 720 },
  video: { mode: "on", size: { width: 1280, height: 720 } },
});
// 多段ツール（検索→取得→計算→執筆）＋実モデルの思考のため 25 分/テスト。
test.setTimeout(1_500_000);

const beat = (p: Page, ms = 900) => p.waitForTimeout(ms);

const XLSX_FIXTURE = path.join(__dirname, "fixtures", "office-live-demo.xlsx");

async function disableWelcome(context: import("@playwright/test").BrowserContext) {
  await context.addInitScript(() => {
    try {
      window.localStorage.setItem("WSDWelcomeDisabled", "true");
      window.localStorage.setItem("WSDWelcomeDisabledDate", new Date().toDateString());
    } catch {
      /* ignore */
    }
  });
}

/// ローカルファイルを Drive にアップロードする（実データの参照物を仕込む）。
async function uploadFile(page: Page, filePath: string, displayName: string) {
  await page.goto("/drive");
  await page
    .locator('input[type="file"]:not([webkitdirectory])')
    .first()
    .setInputFiles(filePath);
  await expect(page.getByText(displayName).first()).toBeVisible({ timeout: 60_000 });
  await page.waitForTimeout(1500);
}

/// コンポーザの「＋」→「ドライブから選択」で添付する。
async function attachFromDrive(page: Page, name: string) {
  await page.getByLabel("追加メニューを開く").first().click();
  await page.getByRole("menuitem", { name: "ドライブから選択" }).click();
  const dialog = page.getByRole("dialog");
  await expect(dialog).toBeVisible({ timeout: 10_000 });
  await dialog.getByRole("button", { name }).first().click();
  await page.keyboard.press("Escape").catch(() => {});
  await expect(page.getByText(name).first()).toBeVisible({ timeout: 10_000 });
  await beat(page, 400);
}

/// 承認カードが出るたびに承認する（多段タスクでは複数回出る）。
/// 最初の 1 枚は長めに待ち、以降は間隔をあけて拾い続ける。
async function approveLoop(page: Page, firstTimeoutMs: number, rounds = 8) {
  const approve = page.getByRole("button", { name: "承認して続行" }).first();
  await expect(approve).toBeVisible({ timeout: firstTimeoutMs });
  await beat(page, 2500);
  await approve.click();
  for (let i = 0; i < rounds; i++) {
    const again = page.getByRole("button", { name: "承認して続行" }).first();
    const appeared = await again
      .waitFor({ state: "visible", timeout: 180_000 })
      .then(() => true)
      .catch(() => false);
    if (!appeared) break;
    await beat(page, 1800);
    await again.click();
  }
}

/// ① 競合調査レポート: web_search（実 SearXNG）→ csv.query → code_interpreter（gVisor）→
/// 開いているノートへ document.edit でライブ執筆。参照物として社内の CSV 実績も渡す。
test("full-market-research: 検索→取得→SQL集計→ノートへレポート", async ({ page }) => {
  await loginViaKeycloak(page);

  // --- 参照物 1: 社内実績 CSV（少し量のある実データ）---
  const csvName = `internal-metrics-${Date.now().toString(36)}.csv`;
  const csvPath = path.join(tmpdir(), csvName);
  const rows = [
    "month,plan,mrr_jpy,new_customers,churned,nps",
    "2026-01,starter,1820000,34,3,41",
    "2026-01,business,4310000,12,1,52",
    "2026-01,enterprise,7600000,2,0,58",
    "2026-02,starter,1910000,29,5,39",
    "2026-02,business,4620000,15,1,54",
    "2026-02,enterprise,8100000,3,0,60",
    "2026-03,starter,1875000,31,7,36",
    "2026-03,business,5010000,18,2,55",
    "2026-03,enterprise,8950000,4,1,59",
    "2026-04,starter,1790000,25,9,33",
    "2026-04,business,5480000,21,2,57",
    "2026-04,enterprise,9700000,5,0,61",
    "2026-05,starter,1735000,22,11,31",
    "2026-05,business,5920000,24,3,58",
    "2026-05,enterprise,10450000,6,1,62",
    "2026-06,starter,1680000,19,12,29",
    "2026-06,business,6390000,27,3,59",
    "2026-06,enterprise,11200000,7,1,63",
  ].join("\n");
  writeFileSync(csvPath, `${rows}\n`, "utf8");
  await uploadFile(page, csvPath, csvName);

  // --- 成果物の器: レポートノートを作って開いたままにする ---
  await page.goto("/drive");
  await page.getByRole("button", { name: "新規作成" }).click();
  await page.getByTestId("new-note").click();
  await page.waitForURL(/\/notes\//, { timeout: 30_000 });
  await page.getByTestId("note-title-input").fill("SaaS 市場・自社実績クロス分析");
  await beat(page, 800);

  await page.getByTestId("note-ask-ai").click();
  await beat(page, 800);
  await attachFromDrive(page, csvName);

  const input = page.getByPlaceholder(/尋ねて|指示|メッセージ/).last();
  await input.click();
  await page.keyboard.type(
    "次の調査レポートを、いま開いているこのノートに執筆してください（新規作成ではなくこのノートを編集）。" +
      "手順: (1) web_search で「SaaS churn rate benchmark 2026」と「SaaS NPS benchmark B2B」を検索し、" +
      "(2) 有望な記事を web_fetch で 3〜4 本まとめて取得して業界水準の具体的な数値を読み取り、" +
      "(3) 添付の社内実績 CSV を csv.query で読み出し、" +
      "その結果を code_interpreter（pandas 利用可）で月次 MRR 成長率・プラン別チャーン率・NPS 推移まで計算し、" +
      "(4) 外部ベンチマークと自社実績を比較する「市場ベンチマーク比較」「自社実績の分析（計算結果の表）」" +
      "「プラン別の課題」「打ち手の提案5点」「出典リンク一覧」の構成でレポートを書いてください。" +
      "数値は必ず csv.query / code_interpreter の実行結果を使い、外部の主張は検索結果の出典 URL を併記してください。",
    { delay: 8 },
  );
  await beat(page, 800);
  await input.press("Enter");

  // web_search / csv.query / code_interpreter は承認不要、document.edit は承認が要る。
  await approveLoop(page, 900_000);

  // レポートがノートへライブ反映される（見出しの出現を確認して全体を流す）。
  const editor = page.locator(".tiptap").first();
  await expect(
    editor.getByRole("heading", { name: /ベンチマーク|自社実績|打ち手|分析/ }).first(),
  ).toBeVisible({ timeout: 420_000 });
  await beat(page, 4000);
  for (let i = 0; i < 6; i++) {
    await page.keyboard.press("PageDown");
    await beat(page, 1500);
  }
  await beat(page, 3000);
});

/// ② 財務ダッシュボード: CSV を csv.query＋code_interpreter で集計 → Excel へライブ書込
/// （複数 op）→ Word 下書きに経営サマリー。1 タスクで 3 種の成果物を作る。
test("full-financial-dashboard: SQL集計→Excel複数表→Word要約", async ({ page }) => {
  await disableWelcome(page.context());
  await loginViaKeycloak(page);

  // --- 参照物 1: 四半期別の取引明細 CSV（行数のある実データ）---
  const csvName = `deals-${Date.now().toString(36)}.csv`;
  const csvPath = path.join(tmpdir(), csvName);
  const header = "quarter,region,industry,deal_jpy,stage,days_to_close";
  const seed = [
    ["2026Q1", "east", "manufacturing", 12400000, "won", 62],
    ["2026Q1", "west", "retail", 3800000, "won", 41],
    ["2026Q1", "east", "finance", 22100000, "lost", 88],
    ["2026Q1", "west", "manufacturing", 7600000, "won", 55],
    ["2026Q2", "east", "retail", 5200000, "won", 37],
    ["2026Q2", "west", "finance", 18700000, "won", 74],
    ["2026Q2", "east", "manufacturing", 9900000, "lost", 91],
    ["2026Q2", "west", "retail", 4100000, "won", 33],
    ["2026Q3", "east", "finance", 26800000, "won", 80],
    ["2026Q3", "west", "manufacturing", 8300000, "won", 58],
    ["2026Q3", "east", "retail", 6100000, "lost", 45],
    ["2026Q3", "west", "finance", 15400000, "won", 69],
    ["2026Q4", "east", "manufacturing", 13900000, "won", 60],
    ["2026Q4", "west", "retail", 4800000, "won", 35],
    ["2026Q4", "east", "finance", 31200000, "won", 84],
    ["2026Q4", "west", "manufacturing", 10600000, "lost", 96],
  ];
  writeFileSync(
    csvPath,
    `${header}\n${seed.map((r) => r.join(",")).join("\n")}\n`,
    "utf8",
  );
  await uploadFile(page, csvPath, csvName);

  // --- 参照物 2: 書き込み先の xlsx ---
  const xlsxName = `fy2026-dashboard-${Date.now().toString(36)}.xlsx`;
  const xlsxPath = path.join(tmpdir(), xlsxName);
  copyFileSync(XLSX_FIXTURE, xlsxPath);
  await uploadFile(page, xlsxPath, xlsxName);

  // --- メインチャットで 2 つを添付して複合タスクを依頼 ---
  await page.goto("/");
  await attachFromDrive(page, csvName);
  await attachFromDrive(page, xlsxName);
  const input = page.getByLabel("メッセージを入力");
  await input.click();
  await page.keyboard.type(
    "添付の取引明細 CSV を csv.query（SQL）と code_interpreter（pandas）で集計し、次の3つを作ってください。" +
      "(1) 四半期別サマリー: 受注額合計・受注件数・勝率・平均クローズ日数を計算し、" +
      `添付の Excel（${xlsxName}）の A1 起点に表として書き込む。` +
      "(2) 同じ Excel の A8 起点に、地域×業種の受注額クロス集計表も書き込む。" +
      "(3) 経営会議向けの説明資料を Word 文書の下書きとして作成する" +
      "（ハイライト、四半期トレンドの考察、勝率が低いセグメントの特定と仮説、次四半期の重点3点）。" +
      "数値は必ず csv.query / code_interpreter の実行結果を使ってください。勝率は won/(won+lost) です。",
    { delay: 8 },
  );
  await beat(page, 800);
  await input.press("Enter");
  await page.waitForURL(/\/c\/[0-9a-f-]+/i, { timeout: 30_000 });

  await approveLoop(page, 900_000);

  // Word 下書きカード（成果物 3）を確認。
  await expect(page.getByTestId("document-draft-card").first()).toBeVisible({
    timeout: 420_000,
  });
  await beat(page, 4000);

  // Excel を開いて 2 つの表（成果物 1・2）を映す。
  await page.goto("/drive");
  await page.getByText(xlsxName).first().dblclick();
  await page.waitForURL(/\/office\//, { timeout: 30_000 });
  await expect(page.getByText("エディタを起動しています…")).toBeHidden({ timeout: 60_000 });
  await page.waitForTimeout(9000);
  await beat(page, 6000);
});
