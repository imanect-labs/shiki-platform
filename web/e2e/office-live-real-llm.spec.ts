import { copyFileSync } from "node:fs";
import { tmpdir } from "node:os";
import path from "node:path";

import { expect, test, type Page } from "@playwright/test";

import { loginViaKeycloak } from "./helpers";

/// **実 LLM（DeepSeek V4 Pro・opencode Go）** による本格タスクの動画撮影専用（AI_REAL=1）。
/// stub ではなく実モデルが自然言語の依頼から office.live_edit / document.edit を自分で
/// 選び、承認を経て実 Collabora / Yjs へライブ編集する様子を撮る。
/// 前提: api が SHIKI__LLM__BACKEND=openai + opencode Go で起動していること。
test.skip(process.env.AI_REAL !== "1", "実 LLM 動画撮影専用（AI_REAL=1）");
test.use({
  locale: "ja-JP",
  viewport: { width: 1280, height: 720 },
  video: { mode: "on", size: { width: 1280, height: 720 } },
});
// 実 LLM は思考（reasoning）込みで応答に数十秒かかる。全体を長めに取る。
test.setTimeout(420_000);

const beat = (p: Page, ms = 900) => p.waitForTimeout(ms);

const FIXTURE = path.join(__dirname, "fixtures", "office-live-demo.xlsx");

function uniqueFixture(prefix: string): { file: string; name: string } {
  const name = `${prefix}-${Date.now().toString(36)}.xlsx`;
  const file = path.join(tmpdir(), name);
  copyFileSync(FIXTURE, file);
  return { file, name };
}

/// Collabora の「What's New」を出さない（office-live-video.spec と同じ手当）。
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

async function typeIntoDocument(page: Page, text: string) {
  const inner = page.frameLocator('[data-testid="office-frame"]');
  await page.keyboard.press("Escape").catch(() => {});
  await inner
    .locator("#main-document-content, #document-container")
    .first()
    .click({ force: true, position: { x: 450, y: 220 } });
  await page.keyboard.type(text, { delay: 40 });
  await page.waitForTimeout(1200);
}

/// 依頼を送信する（office アシスタントパネル）。
async function sendRequest(page: Page, request: string) {
  const input = page.getByTestId("office-chat-panel").getByPlaceholder(/尋ねて|メッセージ|指示/);
  await input.click();
  await page.keyboard.type(request, { delay: 30 });
  await beat(page, 600);
  await input.press("Enter");
}

/// 実 LLM の思考→ツール発行→承認カードを待って承認する。モデルが ops を複数の
/// ツール呼び出しに分けることがあるため、続けて出る承認も拾う（最大 3 回）。
async function approveAll(page: Page, firstTimeoutMs = 180_000) {
  const approve = page.getByRole("button", { name: "承認して続行" }).first();
  await expect(approve).toBeVisible({ timeout: firstTimeoutMs });
  await beat(page, 2500); // 承認カード（実モデルの引数）を映す。
  await approve.click();
  for (let i = 0; i < 2; i++) {
    const again = page.getByRole("button", { name: "承認して続行" }).first();
    const appeared = await again
      .waitFor({ state: "visible", timeout: 30_000 })
      .then(() => true)
      .catch(() => false);
    if (!appeared) break;
    await beat(page, 1500);
    await again.click();
  }
}

/// ① Word: 雑なメモを丁寧なビジネス文へ書き直し → 続けて（同一会話で）末尾へ追記。
/// 実 LLM が selection から node_id を読み、office.live_edit を自分で選ぶ。
test("real-word-rewrite-then-append: 実LLMが書き直し→続けて追記", async ({ page }) => {
  await disableWelcome(page.context());
  await loginViaKeycloak(page);
  await openNewDocument(page);
  await typeIntoDocument(page, "明日の会議、資料まだ。急ぎで頼む。");
  await page.keyboard.press("Control+a");
  await page.waitForTimeout(1200);

  // 選択→AI（チップに選択本文＋node_id が入る）。
  await page.getByTestId("office-ask-ai").click();
  await expect(page.getByTestId("selection-chip")).toBeVisible({ timeout: 15_000 });
  await beat(page);
  await sendRequest(
    page,
    "選択した文を、取引先にそのまま送れる丁寧なビジネス文に書き直して、文書内で置き換えてください",
  );
  await approveAll(page);
  // AI が headless 参加して置換（実 CoolWSD）。反映を映す。
  await beat(page, 15000);

  // 同一会話で追加タスク（選択なし・モデルは会話履歴の node_id を使う）。
  await sendRequest(
    page,
    "ありがとう。同じ文書の末尾に「確認事項」という見出しと、確認すべきことを3点の箇条書きで追記してください",
  );
  await approveAll(page);
  await beat(page, 18000); // 追記が canvas に映る間。
});

/// ② Excel: 自然言語の依頼から実 LLM が set_cells を組み立てて表を作る。
test("real-excel-build-table: 実LLMがシートに売上表を作る", async ({ page }) => {
  await disableWelcome(page.context());
  await loginViaKeycloak(page);
  const { file, name } = uniqueFixture("real-excel");
  await page.goto("/drive");
  await page
    .locator('input[type="file"]:not([webkitdirectory])')
    .first()
    .setInputFiles(file);
  const row = page.getByText(name).first();
  await expect(row).toBeVisible({ timeout: 30_000 });
  await page.waitForTimeout(1500);
  await row.dblclick();
  await page.waitForURL(/\/office\//, { timeout: 30_000 });
  await expect(page.getByText("エディタを起動しています…")).toBeHidden({ timeout: 60_000 });
  await page.waitForTimeout(8000);

  // 全選択で選択チップ（node_id）を作ってから依頼する。
  const inner = page.frameLocator('[data-testid="office-frame"]');
  await inner
    .locator("#main-document-content, #document-container")
    .first()
    .click({ force: true, position: { x: 450, y: 220 } });
  await page.keyboard.press("Control+a");
  await page.waitForTimeout(1200);
  await page.getByTestId("office-ask-ai").click();
  await expect(page.getByTestId("selection-chip")).toBeVisible({ timeout: 15_000 });
  await beat(page);

  await sendRequest(
    page,
    "このシートの A1 を起点に、架空のカフェメニュー売上表を作ってください。列は「メニュー」「杯数」「売上(円)」で、3 商品と合計の行をお願いします",
  );
  await approveAll(page);
  await beat(page, 18000); // 表が canvas に映る間。
});

/// ③ ノート（Yjs ネイティブ）: 実 LLM が document.edit でノートをライブ編集。
/// Collabora と同じ「AI が共同編集エンジンに載る」思想のネイティブ側バリエーション。
test("real-note-organize: 実LLMがノートを見出し付きで整理", async ({ page }) => {
  await loginViaKeycloak(page);
  await page.goto("/drive");
  await page.getByRole("button", { name: "新規作成" }).click();
  await page.getByTestId("new-note").click();
  await page.waitForURL(/\/notes\//, { timeout: 30_000 });
  const editor = page.locator(".tiptap").first();
  await editor.click();
  await page.keyboard.type(
    "# 障害ふりかえりメモ\n\nデプロイ後にAPIが遅くなった。DBのインデックスが効いてなかったらしい。監視も気づくの遅かった。\n",
    { delay: 18 },
  );
  await beat(page, 1200);

  await page.getByTestId("note-ask-ai").click();
  await beat(page, 800);
  await editor.getByText("DBのインデックス", { exact: false }).click({ clickCount: 3 });
  await beat(page, 800);
  await expect(page.getByTestId("selection-chip")).toBeVisible({ timeout: 10_000 });

  const input = page.getByPlaceholder(/尋ねて|指示|メッセージ/).last();
  await input.click();
  await page.keyboard.type(
    "このメモを、原因・影響・対策の見出しで整理して、ノートに追記してください",
    { delay: 30 },
  );
  await beat(page, 600);
  await input.press("Enter");
  await approveAll(page);

  // document.edit → Yjs → 本文がライブで書き換わる（DOM なので見出しの出現を待てる）。
  await expect(editor.getByRole("heading", { name: /原因|対策/ }).first()).toBeVisible({
    timeout: 60_000,
  });
  await beat(page, 5000);
});
