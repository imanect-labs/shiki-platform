import { expect, test } from "@playwright/test";

import { loginViaKeycloak } from "./helpers";

/// スライドの「選択→AI 依頼」で slide.edit（要確認・破壊系）が human-in-the-loop の承認カードを
/// 出し、承認すると本物のパイプライン（CollabHub::apply_ai_slide_edit・Yjs broadcast）で表示中
/// スライドが GrapesJS キャンバス上にライブで書き換わり、別ブラウザにも収束することを検証する
/// （前提: LLM=stub・エディタバンドルは app-gateway /builtin に配備済み）。
///
/// 回帰対象（#328・ノート同等のライブ AI 共同編集をスライドへ）:
/// - slide_selection の locator（slide_id）が選択チップ経由でサーバへ渡る。
/// - slide.edit が承認ゲートを通り、AI 編集が observeDeep → slide:load → setComponents で
///   編集中キャンバスへ echo される（AI 編集と人間編集が同一 Yjs 経路で収束）。
///
/// stub 駆動: slide_selection の node_id ＋ locator.slide_id ＋編集キーワードで
/// slide.edit（replace_slide・MOCK_SLIDE_EDIT_HTML）を呼ぶ。

const BUILTIN_URL = process.env.E2E_B1_ORIGIN
  ? `${process.env.E2E_B1_ORIGIN}/builtin/slide-editor`
  : "http://localhost:8091/builtin/slide-editor";

test.beforeAll(async () => {
  const res = await fetch(BUILTIN_URL).catch(() => null);
  test.skip(!res?.ok, `エディタバンドル未配備のためスキップ（${BUILTIN_URL}）`);
});

test("選択→AI: slide.edit が承認ゲートを経てキャンバスをライブ編集し、別ブラウザへ収束する", async ({
  browser,
}) => {
  // ユーザー A がスライドを作成してエディタを開く。
  const ctxA = await browser.newContext();
  const pageA = await ctxA.newPage();
  await loginViaKeycloak(pageA);
  await pageA.goto("/drive");
  await pageA.getByRole("button", { name: "新規作成" }).click();
  await pageA.getByTestId("new-slide").click();
  await pageA.waitForURL(/\/slides\//, { timeout: 20_000 });
  const slideUrl = pageA.url();
  await expect(pageA.getByTestId("slide-editor-frame")).toBeVisible({ timeout: 20_000 });

  // ユーザー B を **AI 編集の前に** 同じスライドへ接続しておく（既接続クライアントへの Yjs
  // broadcast が届くことを検証するため。編集後に開くと初期ロードしか検証できない）。
  const ctxB = await browser.newContext();
  const pageB = await ctxB.newPage();
  await loginViaKeycloak(pageB);
  await pageB.goto(slideUrl);
  await expect(pageB.getByTestId("slide-filmstrip")).toBeVisible({ timeout: 20_000 });

  // アシスタントを開く（開いていると選択が自動でチャットへ挿入される）。
  await pageA.getByTestId("slide-ask-ai").click();

  // キャンバスの見出しを単一クリックで選択 → 選択要素がチャットへ挿入される（slide_id 付き）。
  const canvas = pageA
    .frameLocator('[data-testid="slide-editor-frame"]')
    .frameLocator("iframe.gjs-frame");
  await canvas.locator("h1").first().click();
  await expect(pageA.getByTestId("selection-chip")).toBeVisible({ timeout: 15_000 });

  // 編集キーワードを含む依頼を送る → stub が slide.edit（replace_slide）を呼ぶ。
  const input = pageA.getByTestId("note-chat-panel").getByLabel("メッセージを入力");
  await input.fill("このスライドを、結論を先頭にして要点を整理して書き直して");
  await input.press("Enter");

  // 破壊系なので承認カードが出る（human-in-the-loop）。承認して実行させる。
  const approve = pageA.getByRole("button", { name: "承認して続行" });
  await expect(approve).toBeVisible({ timeout: 20_000 });
  await approve.click();

  // AI 編集が Yjs 経由でキャンバスへライブ反映される（差し替え後の見出しが現れる）。
  await expect(canvas.getByRole("heading", { name: "AI が改訂したスライド" })).toBeVisible({
    timeout: 25_000,
  });

  // 既に接続済みのユーザー B のフィルムストリップにも、AI 編集が live broadcast で収束する。
  await expect(
    pageB.frameLocator('[data-testid="slide-frame"] iframe').first().getByText("AI が改訂したスライド"),
  ).toBeVisible({ timeout: 20_000 });

  await ctxA.close();
  await ctxB.close();
});
