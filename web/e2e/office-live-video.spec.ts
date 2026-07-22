import { copyFileSync } from "node:fs";
import { tmpdir } from "node:os";
import path from "node:path";

import { expect, test, type Page } from "@playwright/test";

import { loginViaKeycloak } from "./helpers";

const FIXTURE = path.join(__dirname, "fixtures", "office-live-demo.xlsx");

/// fixture を一意名でコピーして返す（同一 Drive に同名を作らず重複行を避ける）。
function uniqueFixture(prefix: string): { file: string; name: string } {
  const name = `${prefix}-${Date.now().toString(36)}.xlsx`;
  const file = path.join(tmpdir(), name);
  copyFileSync(FIXTURE, file);
  return { file, name };
}

/// Drive に xlsx をアップロードして開き、fileId を返す。
async function uploadAndOpenXlsx(page: Page, prefix: string): Promise<string> {
  const { file, name } = uniqueFixture(prefix);
  await page.goto("/drive");
  await page
    .locator('input[type="file"]:not([webkitdirectory])')
    .first()
    .setInputFiles(file);
  const row = page.getByText(name).first();
  await expect(row).toBeVisible({ timeout: 30_000 });
  await page.waitForTimeout(1500); // アップロード完了（行が確定）を待つ。
  await row.dblclick();
  await page.waitForURL(/\/office\//, { timeout: 30_000 });
  const fileId = page.url().match(/\/office\/([^/?#]+)/)?.[1];
  if (!fileId) throw new Error("fileId が取れませんでした");
  await expect(page.getByText("エディタを起動しています…")).toBeHidden({ timeout: 60_000 });
  await page.waitForTimeout(6000);
  return fileId;
}

/// AI headless 参加（office.live_edit・issue #352）のデモ動画撮影専用（AI_LIVE=1）。
/// 実 Collabora＋実パイプライン（stub LLM → 承認 → CoolWSD headless 接続 → 検索/paste →
/// WOPI PutFile 保存）で「AI が共同編集参加者として働く様子」を撮る。
/// 動画は test-results/<テスト名>/video.webm に保存される。
test.skip(process.env.AI_LIVE !== "1", "動画撮影専用（AI_LIVE=1）");
// viewport は office-assistant.spec（実証済みの選択→編集フロー）と同じ既定 1280x720 に揃える
// （広い viewport だと Collabora の Navigation パネル等がレイアウトを変え、typeIntoDocument の
// クリックが検索欄に逸れることがあるため）。
test.use({
  locale: "ja-JP",
  viewport: { width: 1280, height: 720 },
  video: { mode: "on", size: { width: 1280, height: 720 } },
});
// Collabora 起動・kit 初期化・保存反映など実物の待ちが多いため全体を長めに取る。
test.setTimeout(240_000);

const beat = (p: Page, ms = 900) => p.waitForTimeout(ms);

/// Collabora の「What's New」ダイアログを閉じて綺麗な編集結果を映す。タイピングは
/// ダイアログ表示中でも文書に届くため、選択・依頼が済んだ**承認後**に閉じる
/// （home_mode.enable は Navigation パネルが焦点を奪うため使わない）。出なければ無視。
async function dismissWelcome(page: Page) {
  // What's New は Collabora フレーム内の**ネスト iframe（iframe.iframe-welcome）**にあり、閉じる X は
  // その中の #welcome-close（診断で確認）。isVisible ガードは false 判定で空振りするため、
  // 直接 click（timeout 付き）して存在しなければ catch する。
  const close = page
    .frameLocator('[data-testid="office-frame"]')
    .frameLocator("iframe.iframe-welcome")
    .locator("#welcome-close");
  await close.click({ timeout: 5000 }).catch(() => {});
  await page.waitForTimeout(800);
}

/// Drive から新規ドキュメント（docx）を作成して Collabora が落ち着くまで待つ。
async function openNewDocument(page: Page) {
  await page.goto("/drive");
  for (let i = 0; i < 3 && !/\/office\//.test(page.url()); i++) {
    await page.getByRole("button", { name: "新規作成" }).click();
    await page.getByTestId("new-document").click();
    await page.waitForURL(/\/office\//, { timeout: 30_000 }).catch(() => {});
  }
  expect(page.url()).toMatch(/\/office\//);
  await expect(page.getByText("エディタを起動しています…")).toBeHidden({ timeout: 60_000 });
  await page.waitForTimeout(8000);
}

/// Collabora の編集領域へフォーカスして本文を打つ。
async function typeIntoDocument(page: Page, text: string) {
  const inner = page.frameLocator('[data-testid="office-frame"]');
  await inner
    .locator("#main-document-content, #document-container")
    .first()
    .click({ force: true, position: { x: 60, y: 40 } });
  await page.keyboard.type(text, { delay: 45 });
  await page.waitForTimeout(1200);
}

/// 選択→依頼→承認カード→承認まで（動画の主役シーン）。
async function askAndApprove(page: Page, request: string) {
  await page.getByTestId("office-ask-ai").click();
  await expect(page.getByTestId("selection-chip")).toBeVisible({ timeout: 15_000 });
  await beat(page);
  const input = page.getByTestId("office-chat-panel").getByPlaceholder(/尋ねて|メッセージ|指示/);
  await input.click();
  await page.keyboard.type(request, { delay: 40 });
  await beat(page, 600);
  await input.press("Enter");
  const approve = page.getByRole("button", { name: "承認して続行" });
  await expect(approve).toBeVisible({ timeout: 30_000 });
  await beat(page, 2000); // 承認カード（ツール名・引数）が映る間。
  await approve.click();
}

/// ① Word: 選択→AI→承認→「Shiki AI」が参加者として本文をライブ置換する。
test("word-live-replace: AI が参加者として選択箇所を書き換える", async ({ page }) => {
  await loginViaKeycloak(page);
  await openNewDocument(page);
  await typeIntoDocument(page, "差し替え対象の本文");
  await page.keyboard.press("Control+a");
  await page.waitForTimeout(1200);

  await askAndApprove(page, "この選択範囲を、丁寧な文章に書き直して");

  // AI が headless 参加 → 検索・照合 → paste → 保存（実 CoolWSD・PutFile）。承認後に What's New を
  // 閉じ、編集後の綺麗な本文が canvas に映るまで待つ（正しさは office-assistant.spec の chip で担保）。
  await beat(page, 4000);
  await dismissWelcome(page);
  await beat(page, 10000);
});

/// ② コワーク（2 画面）: 別ウィンドウで同じ文書を開いている参加者の画面にも
/// AI 編集がライブで届く（Collabora の view 同期）。両ウィンドウの動画が撮れる。
test("word-cowork-two-windows: もう一人の画面にもライブ反映される", async ({ browser }) => {
  const ctxA = await browser.newContext({
    locale: "ja-JP",
    viewport: { width: 1280, height: 720 },
    recordVideo: { dir: "test-results/cowork-editor-A", size: { width: 1280, height: 720 } },
  });
  const ctxB = await browser.newContext({
    locale: "ja-JP",
    viewport: { width: 1280, height: 720 },
    recordVideo: { dir: "test-results/cowork-editor-B", size: { width: 1280, height: 720 } },
  });
  try {
    const pageA = await ctxA.newPage();
    await loginViaKeycloak(pageA);
    await openNewDocument(pageA);
    await typeIntoDocument(pageA, "四半期レビューの下書きです。西日本エリアの記述");
    const docUrl = pageA.url();

    // A 側で先に全選択→「AI に依頼」まで済ませ、選択チップを確定させる（B 参加で A の
    // 編集フォーカスが移る前に選択を取り込む）。
    await pageA.keyboard.press("Control+a");
    await pageA.waitForTimeout(1200);
    await pageA.getByTestId("office-ask-ai").click();
    await expect(pageA.getByTestId("selection-chip")).toBeVisible({ timeout: 15_000 });

    // B（同一ユーザーの別ウィンドウ）が同じ文書を開いて共同編集セッションに参加する。
    const pageB = await ctxB.newPage();
    await loginViaKeycloak(pageB);
    await pageB.goto(docUrl);
    await expect(pageB.getByText("エディタを起動しています…")).toBeHidden({ timeout: 60_000 });
    await pageB.waitForTimeout(8000);

    // A 側で依頼を送信→承認（選択チップは確定済み）。
    const input = pageA
      .getByTestId("office-chat-panel")
      .getByPlaceholder(/尋ねて|メッセージ|指示/);
    await input.click();
    await pageA.keyboard.type("この部分を、要点が伝わる文章に書き直して", { delay: 40 });
    await pageA.waitForTimeout(600);
    await input.press("Enter");
    const approveA = pageA.getByRole("button", { name: "承認して続行" });
    await expect(approveA).toBeVisible({ timeout: 30_000 });
    await pageA.waitForTimeout(2000);
    await approveA.click();

    // B 側は何も操作していないのに本文が変わる（Collabora の view 同期・+「Shiki AI」参加が映る）。
    // 両ウィンドウの What's New を閉じ、canvas に反映が現れるまで待つ（動画が主証拠）。
    await pageA.waitForTimeout(4000);
    await dismissWelcome(pageA);
    await dismissWelcome(pageB);
    await pageA.waitForTimeout(14000);
    await pageB.waitForTimeout(2000);
  } finally {
    await ctxA.close();
    await ctxB.close();
  }
});

/// ③ Excel: アップロードした xlsx を開き、AI がセル矩形（set_cells）をライブで貼り込む。
test("excel-set-cells-live: 開いているシートへ AI がセルを書き込む", async ({ page }) => {
  await loginViaKeycloak(page);
  const fileId = await uploadAndOpenXlsx(page, "excel-live");
  await page.waitForTimeout(8000);

  // officecells: プレフィックスで stub が set_cells を発行 → 承認 → ライブ貼り込み。
  await page.getByTestId("office-ask-ai").click();
  const input = page.getByTestId("office-chat-panel").getByPlaceholder(/尋ねて|メッセージ|指示/);
  await input.click();
  await input.fill(`officecells:${fileId}`);
  await beat(page, 600);
  await input.press("Enter");
  const approve = page.getByRole("button", { name: "承認して続行" });
  await expect(approve).toBeVisible({ timeout: 30_000 });
  await beat(page, 2000);
  await approve.click();

  // AI が headless 参加して A1 起点にセル矩形を貼り込む。What's New を閉じ、貼り込まれた表が
  // canvas に現れるまで待つ（動画が主証拠）。
  await beat(page, 4000);
  await dismissWelcome(page);
  await beat(page, 12000);
});

/// ④ 閉じた文書への編集: 文書を開いていなくても AI が単独セッションを立てて編集・保存し、
/// 開き直すと新バージョンに反映されている。
test("excel-closed-doc-edit: 閉じた文書も AI が編集して新バージョン保存", async ({ page }) => {
  await loginViaKeycloak(page);
  // アップロード→一度開いて fileId を取り、Drive へ戻って閉じる（編集セッション無しの状態を作る）。
  const fileId = await uploadAndOpenXlsx(page, "excel-closed");
  await page.waitForTimeout(3000);
  await page.goto("/drive");
  await beat(page, 1500);

  // 通常チャットから officecells: → 承認（文書は誰も開いていない）。
  await page.goto("/");
  const chat = page.getByLabel("メッセージを入力");
  await chat.click();
  await chat.fill(`officecells:${fileId}`);
  await beat(page, 600);
  await page.getByRole("button", { name: "送信" }).click();
  await page.waitForURL(/\/c\/[0-9a-f-]+/i, { timeout: 20_000 });
  const approve = page.getByRole("button", { name: "承認して続行" });
  await expect(approve).toBeVisible({ timeout: 30_000 });
  await beat(page, 2000);
  await approve.click();
  // AI 単独セッション（kit 起動込み）→ 編集 → 保存（誰も開いていない文書を AI が編集して新版化）。
  await beat(page, 18000);

  // 開き直すと AI の書き込みが反映されている（新バージョンの canvas に表が現れる）。
  await page.goto(`/office/${fileId}`);
  await expect(page.getByText("エディタを起動しています…")).toBeHidden({ timeout: 60_000 });
  await page.waitForTimeout(6000);
  await beat(page, 6000); // 貼り込まれた表が映る間。
});
