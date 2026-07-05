import { expect, test } from "@playwright/test";

import { loginViaKeycloak } from "./helpers";

/// 検索動線のスモーク（Task 2.10）。
/// 日常動線: ⌘K（入力欄外では "/" も可）のフローティング検索（最上段「"q"で検索↗︎」→ ドライブの
/// 検索結果画面、下にフォルダ/ファイルをスコア順）と、ドライブの検索バー
/// （名前一致＋内容一致の統合一覧）。引用・段階ファネルのデバッグは /search
/// 詳細ページ。実インジェスト E2E は worker/Qdrant を要するため compose 検証で
/// 行い、CI では結線のみを見る（RAG は 503 で内容一致セクションが出ない）。
test("フローティング検索: 「で検索」からドライブの検索結果画面へ遷移する", async ({
  page,
}) => {
  await loginViaKeycloak(page);

  // ⌘K/Ctrl+K でパレットが開く（"/" は入力欄フォーカス中は無効のため、
  // チャット入力に自動フォーカスされるホームでは Ctrl+K を使う）。
  await page.keyboard.press("ControlOrMeta+k");
  const input = page.getByRole("textbox", { name: "文書・チャットを検索" });
  await expect(input).toBeVisible();
  await input.fill("経費");
  // 最上段に「"経費"で検索」アクションが出る（RAG 無効環境でも表示される）。
  const searchAction = page.getByRole("button", { name: "「経費」で検索" });
  await expect(searchAction).toBeVisible();
  await searchAction.click();
  // ドライブの検索結果画面（?q=）へ遷移し、検索バーへクエリが引き継がれる。
  await page.waitForURL(/\/drive\?q=/);
  await expect(page.getByRole("searchbox", { name: "ドライブを検索" })).toHaveValue("経費");
  await expect(page.getByText("「経費」の検索結果")).toBeVisible();
});

test("フローティング検索: Escape で閉じ、未入力時は新しいチャットが先頭", async ({ page }) => {
  await loginViaKeycloak(page);

  await page.keyboard.press("ControlOrMeta+k");
  const input = page.getByRole("textbox", { name: "文書・チャットを検索" });
  await expect(input).toBeVisible();
  await expect(page.getByRole("button", { name: "新しいチャット" })).toBeVisible();
  await page.keyboard.press("Escape");
  await expect(input).not.toBeVisible();
});

test("詳細検索ページ（デバッグ）が表示され、検索 UI が結線されている", async ({ page }) => {
  await loginViaKeycloak(page);

  await page.goto("/search");
  await expect(page.getByRole("heading", { name: "文書検索" })).toBeVisible();
  await expect(page.getByRole("textbox", { name: "検索クエリ" })).toBeVisible();
  await expect(page.getByRole("radio", { name: "ハイブリッド" })).toBeVisible();
  // クエリ未入力では検索ボタンは押せない（exact: 他の「検索」ボタンと区別）。
  await expect(page.getByRole("button", { name: "検索", exact: true })).toBeDisabled();
});
