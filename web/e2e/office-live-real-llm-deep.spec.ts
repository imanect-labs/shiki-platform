import { copyFileSync } from "node:fs";
import { tmpdir } from "node:os";
import path from "node:path";

import { expect, test, type Page } from "@playwright/test";

import { loginViaKeycloak } from "./helpers";

/// **実 LLM の本格タスク**（AI_DEEP=1・動画撮影専用）。複数の参照文書を読み込み、
/// ボリュームのある成果物（経営会議レポート・集計表＋分析レポート）を作らせる。
/// 前提: api が実 LLM（openai 互換）で起動していること。
test.skip(process.env.AI_DEEP !== "1", "実 LLM 本格タスク動画（AI_DEEP=1）");
test.use({
  locale: "ja-JP",
  viewport: { width: 1280, height: 720 },
  video: { mode: "on", size: { width: 1280, height: 720 } },
});
// 参照読込（複数回のツール往復）＋長文生成のため 15 分/テストまで許容。
test.setTimeout(900_000);

const beat = (p: Page, ms = 900) => p.waitForTimeout(ms);

const FIXTURE = path.join(__dirname, "fixtures", "office-live-demo.xlsx");

/// Collabora の What's New を出さない（office-live-video.spec と同じ手当）。
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

/// タイトル付きノートを作成し本文を打ち、**ドライブ上のファイル名も**タイトルへ揃える
/// （note-title-input はメタデータであり drive のファイル名は 無題のノート (N).md のまま。
/// 添付ピッカーはファイル名で選ぶため、行メニュー「名前を変更」で付け替える）。
async function createNote(page: Page, title: string, content: string) {
  await page.goto("/drive");
  await page.getByRole("button", { name: "新規作成" }).click();
  await page.getByTestId("new-note").click();
  await page.waitForURL(/\/notes\//, { timeout: 30_000 });
  await page.getByTestId("note-title-input").fill(title);
  const editor = page.locator(".tiptap").first();
  await editor.click();
  await page.keyboard.type(content, { delay: 6 });
  // Yjs の保存が走るのを待ってから離脱する。
  await page.waitForTimeout(1500);
  // 直前に作った未リネームのノート（無題のノート*.md は常に 1 件だけ残っている）を改名。
  await page.goto("/drive");
  await page
    .getByLabel(/「無題のノート.*」の操作/)
    .first()
    .click({ force: true });
  await page.getByRole("menuitem", { name: "名前を変更" }).click();
  await page.getByLabel("新しい名前").fill(`${title}.md`);
  await page.getByRole("button", { name: "変更" }).click();
  await expect(page.getByText(`${title}.md`).first()).toBeVisible({ timeout: 10_000 });
}

/// コンポーザの「＋」→「ドライブから選択」でファイルを添付する。
async function attachFromDrive(page: Page, name: string) {
  await page.getByLabel("追加メニューを開く").first().click();
  await page.getByRole("menuitem", { name: "ドライブから選択" }).click();
  const dialog = page.getByRole("dialog");
  await expect(dialog).toBeVisible({ timeout: 10_000 });
  await dialog.getByRole("button", { name }).first().click();
  // ダイアログが残る実装でも先へ進めるよう Escape で閉じる（閉じ済みなら no-op）。
  await page.keyboard.press("Escape").catch(() => {});
  await expect(page.getByText(name).first()).toBeVisible({ timeout: 10_000 });
  await beat(page, 400);
}

/// 実 LLM の承認カードを待って承認する（複数ツール呼び出しに分かれても拾う）。
async function approveAll(page: Page, firstTimeoutMs = 300_000, extra = 3) {
  const approve = page.getByRole("button", { name: "承認して続行" }).first();
  await expect(approve).toBeVisible({ timeout: firstTimeoutMs });
  await beat(page, 2500);
  await approve.click();
  for (let i = 0; i < extra; i++) {
    const again = page.getByRole("button", { name: "承認して続行" }).first();
    const appeared = await again
      .waitFor({ state: "visible", timeout: 60_000 })
      .then(() => true)
      .catch(() => false);
    if (!appeared) break;
    await beat(page, 1500);
    await again.click();
  }
}

/// ① 3 つの部門月次報告（参照ノート）を読み込ませ、開いているノートに
/// ボリュームのある「経営会議レポート」を執筆させる。
test("deep-monthly-exec-report: 3つの参照ノート→経営会議レポートを執筆", async ({ page }) => {
  await loginViaKeycloak(page);

  // --- 参照資料 3 本を仕込む ---
  await createNote(
    page,
    "営業部 10月月次報告",
    "新規受注は24件（前月比+20%）。大型案件はアクメ商事1,200万円、ベータ工業800万円。解約は2件。" +
      "西日本エリアの商談化率が12%から9%に低下しており、要因はフィールド人員の不足による追客遅延。" +
      "パイプライン総額は前月比+8%の2.4億円。",
  );
  await createNote(
    page,
    "開発部 10月月次報告",
    "10月15日に新検索機能をリリースし、利用率は想定を上回る勢い。障害は2件発生: " +
      "10月8日にDBインデックス欠落によるAPI遅延（影響2時間）、10月22日に夜間バッチ失敗（30分）。" +
      "技術的負債の返済は計画の60%で遅延気味。バックエンドエンジニア1名の採用が内定。",
  );
  await createNote(
    page,
    "サポート部 10月月次報告",
    "問い合わせは342件（前月比+15%）。平均初回応答は4.2時間で目標の2時間を未達。" +
      "解約予兆のある顧客が3社。新検索機能への好意的なフィードバックが多数。" +
      "FAQ整備により自己解決率は38%から45%へ改善。",
  );

  // --- 成果物の器（レポートノート）を作って開いたまま、AI に執筆させる ---
  await page.goto("/drive");
  await page.getByRole("button", { name: "新規作成" }).click();
  await page.getByTestId("new-note").click();
  await page.waitForURL(/\/notes\//, { timeout: 30_000 });
  await page.getByTestId("note-title-input").fill("10月 経営会議レポート");
  await beat(page, 800);

  await page.getByTestId("note-ask-ai").click();
  await beat(page, 800);
  // 参照 3 本を添付（node_id がモデルへ渡り document.read の対象になる）。
  await attachFromDrive(page, "営業部 10月月次報告.md");
  await attachFromDrive(page, "開発部 10月月次報告.md");
  await attachFromDrive(page, "サポート部 10月月次報告.md");

  const input = page.getByPlaceholder(/尋ねて|指示|メッセージ/).last();
  await input.click();
  await page.keyboard.type(
    "添付した3つの部門月次報告を、1つずつ順番にすべて読み込んでから、この開いているノートに「10月 経営会議レポート」を執筆してください。" +
      "（node_id は添付情報のものを正確にコピーして使ってください）" +
      "構成は: エグゼクティブサマリー（3行）、部門別ハイライト（営業・開発・サポート、各3〜5点）、" +
      "部門横断の重要リスク3点（根拠つき）、来月の重点アクション5点（担当部門を明記）、" +
      "最後に主要KPIのまとめ表。事実は報告書の数字に忠実に。",
    { delay: 12 },
  );
  await beat(page, 800);
  await input.press("Enter");

  // 3 本の read（承認不要）→ 執筆の document.edit（承認）まで待って承認する。
  await approveAll(page, 420_000);

  // 大きなレポートがライブでノートに書き込まれる。見出しの出現を確認して全体をスクロール。
  const editor = page.locator(".tiptap").first();
  await expect(
    editor.getByRole("heading", { name: /エグゼクティブサマリー|部門別|リスク/ }).first(),
  ).toBeVisible({ timeout: 240_000 });
  await beat(page, 4000);
  for (let i = 0; i < 5; i++) {
    await page.keyboard.press("PageDown");
    await beat(page, 1500);
  }
  await beat(page, 3000);
});

/// ② データノート（参照）＋ xlsx を渡し、集計表を Excel に書き込ませ、
/// さらに分析レポートを Word 下書きとして作らせる（2 つの成果物）。
test("deep-sales-analysis: データノート→Excel集計＋Word分析レポート", async ({ page }) => {
  await disableWelcome(page.context());
  await loginViaKeycloak(page);

  // --- 参照: 週次売上データのノート ---
  await createNote(
    page,
    "11月 週次売上データ",
    "第1週: アイスカフェラテ 120杯 60000円 / 抹茶フラペチーノ 80杯 52000円 / フルーツティー 60杯 30000円\n" +
      "第2週: アイスカフェラテ 135杯 67500円 / 抹茶フラペチーノ 95杯 61750円 / フルーツティー 75杯 37500円\n" +
      "第3週: アイスカフェラテ 110杯 55000円 / 抹茶フラペチーノ 120杯 78000円 / フルーツティー 90杯 45000円\n" +
      "第4週: アイスカフェラテ 150杯 75000円 / 抹茶フラペチーノ 140杯 91000円 / フルーツティー 85杯 42500円\n" +
      "備考: 第3週から抹茶フラペチーノのSNSキャンペーンを実施。",
  );

  // --- 書き込み先の xlsx をアップロード ---
  const xlsxName = `sales-summary-${Date.now().toString(36)}.xlsx`;
  const xlsxPath = path.join(tmpdir(), xlsxName);
  copyFileSync(FIXTURE, xlsxPath);
  await page.goto("/drive");
  await page
    .locator('input[type="file"]:not([webkitdirectory])')
    .first()
    .setInputFiles(xlsxPath);
  await expect(page.getByText(xlsxName).first()).toBeVisible({ timeout: 30_000 });
  await beat(page, 1200);

  // --- メインチャットで両方を添付して依頼 ---
  await page.goto("/");
  await attachFromDrive(page, "11月 週次売上データ.md");
  await attachFromDrive(page, xlsxName);
  const input = page.getByLabel("メッセージを入力");
  await input.click();
  await page.keyboard.type(
    "添付の「11月 週次売上データ」ノートを読み込んで集計し、次の2つを作ってください。" +
      `①添付のExcel（${xlsxName}）のA1を起点に「商品別サマリー」表を書き込む` +
      "（列: 商品/杯数合計/売上合計(円)/売上構成比。最終行に総合計）。" +
      "②経営向けの分析レポートをWord文書の下書きとして作成する" +
      "（週次トレンド、商品別の考察、キャンペーン効果の評価、来月の提案3点）。",
    { delay: 12 },
  );
  await beat(page, 800);
  await input.press("Enter");
  await page.waitForURL(/\/c\/[0-9a-f-]+/i, { timeout: 20_000 });

  // read（承認不要）→ office.live_edit set_cells（承認）→ save_document（下書き）。
  await approveAll(page, 420_000);

  // Word 下書きカードが現れる（大きな成果物その1）。
  await expect(page.getByTestId("document-draft-card").first()).toBeVisible({
    timeout: 300_000,
  });
  await beat(page, 4000);

  // Excel を開いて集計表（成果物その2）を映す。
  await page.goto("/drive");
  await page.getByText(xlsxName).first().dblclick();
  await page.waitForURL(/\/office\//, { timeout: 30_000 });
  await expect(page.getByText("エディタを起動しています…")).toBeHidden({ timeout: 60_000 });
  await page.waitForTimeout(8000);
  await beat(page, 6000);
});
