import { expect, test, type Page } from "@playwright/test";

import { loginViaKeycloak } from "./helpers";

/// チャットの図を、開いている Excel・Word へグラフとして入れる（#731）。
/// 実 Collabora（26.04 以上・Office エンジン）とスタブ LLM（`yui:bar` で棒グラフを返す）が要るため
/// OFFICE_E2E=1 でのみ実行する。保存されたファイルの中身（zip の中のグラフ部品）で裏取りする。
test.skip(process.env.OFFICE_E2E !== "1", "OFFICE_E2E=1 のときのみ実行（Collabora が必要）");
test.use({ locale: "ja-JP", viewport: { width: 1280, height: 800 } });
// 文書を開く・回答を待つ・挿入して保存を確かめる、まで 1 本で通すので長めにとる。
test.setTimeout(180_000);

async function openNew(page: Page, testId: "new-document" | "new-sheet") {
  await page.goto("/drive");
  for (let i = 0; i < 3 && !/\/office\//.test(page.url()); i++) {
    await page.getByRole("button", { name: "新規作成" }).click();
    await page.getByTestId(testId).click();
    await page.waitForURL(/\/office\//, { timeout: 30_000 }).catch(() => {});
  }
  expect(page.url()).toMatch(/\/office\//);
  await expect(page.getByText("エディタを起動しています…")).toBeHidden({ timeout: 60_000 });
  await page.waitForTimeout(6000);
  return page.url().match(/\/office\/([^/?#]+)/)?.[1] ?? "";
}

/// アシスタントに棒グラフの回答を出させ、その図の挿入ボタンを返す。
async function askForBarChart(page: Page) {
  await page.getByTestId("office-ask-ai").click();
  const panel = page.getByTestId("office-chat-panel");
  const input = panel.getByPlaceholder(/尋ねて|メッセージ|指示/);
  await expect(input).toBeVisible({ timeout: 20_000 });
  await input.fill("yui:bar");
  await input.press("Enter");
  const figure = panel.getByTestId("ui-figure").first();
  await expect(figure).toBeVisible({ timeout: 30_000 });
  return figure.getByTestId("ui-figure-insert");
}

/// トーストの文言（画面読み上げ用の要素にも同じ文が入るので、完全一致の最初の 1 つ）。
function toast(page: Page, text: string) {
  return page.getByText(text, { exact: true }).first();
}

/// 保存された最新版の中身（zip）に、グラフの部品があるか。zip の中の名前は圧縮されないので
/// バイト列にそのまま現れる。
async function savedFileHas(page: Page, fileId: string, part: string) {
  const res = await page.request.get(`/api/files/${fileId}/download-url`);
  expect(res.ok()).toBeTruthy();
  const { url } = (await res.json()) as { url: string };
  const bytes = await (await page.request.get(url)).body();
  return bytes.includes(Buffer.from(part));
}

test("Word: 回答の棒グラフを、文書の末尾に Word のグラフとして入れる", async ({ page }) => {
  await loginViaKeycloak(page);
  const fileId = await openNew(page, "new-document");
  const insert = await askForBarChart(page);
  await expect(insert).toHaveText("Word に挿入");
  await insert.click();
  await expect(toast(page, "グラフを入れました")).toBeVisible({ timeout: 60_000 });
  await expect(toast(page, "文書の末尾にグラフを入れました")).toBeVisible();
  await page.screenshot({ path: "test-results/office-chart-insert-word.png" });
  await expect.poll(() => savedFileHas(page, fileId, "word/charts/chart"), { timeout: 30_000 }).toBe(true);
});

test("Excel: 回答の棒グラフを、新しいシートに表とグラフとして入れる", async ({ page }) => {
  await loginViaKeycloak(page);
  const fileId = await openNew(page, "new-sheet");
  const insert = await askForBarChart(page);
  await expect(insert).toHaveText("Excel に挿入");
  await insert.click();
  await expect(toast(page, "シート「四半期ごとの売上と利益」に表とグラフを置きました")).toBeVisible({ timeout: 60_000 });
  // 人の画面は元のシートのまま。足したシートのタブを開いて、表とグラフを目で確かめられるようにする。
  await page
    .frameLocator('[data-testid="office-frame"]')
    .getByText("四半期ごとの売上と利益", { exact: true })
    // シートのタブは Collabora の描画で揺れ続け「stable」にならないことがあるので force で押す。
    .click({ force: true });
  await page.waitForTimeout(3000);
  await page.screenshot({ path: "test-results/office-chart-insert-excel.png" });
  await expect.poll(() => savedFileHas(page, fileId, "xl/charts/chart"), { timeout: 30_000 }).toBe(true);
  // 同じ図をもう一度入れると、空いている名前のシートになる（既存のシートを上書きしない）。
  await insert.click();
  await expect(toast(page, "シート「四半期ごとの売上と利益 (2)」に表とグラフを置きました")).toBeVisible({
    timeout: 60_000,
  });
});

test("通常のチャット: ドライブから選んだ Word へグラフを入れる", async ({ page }) => {
  await loginViaKeycloak(page);
  // 入れる先の Word を作っておく（ドライブの最上位に「無題のドキュメント (N)」ができる）。
  const fileId = await openNew(page, "new-document");
  const { name } = (await (await page.request.get(`/api/files/${fileId}`)).json()) as { name: string };
  await page.goto("/");
  const input = page.getByPlaceholder(/尋ねて|メッセージ|指示/).first();
  await input.fill("yui:bar");
  await input.press("Enter");
  const figure = page.getByTestId("ui-figure").first();
  await expect(figure).toBeVisible({ timeout: 30_000 });
  const insert = figure.getByTestId("ui-figure-insert");
  await expect(insert).toHaveText("Excel / Word に挿入…");
  await insert.click();
  const dialog = page.getByRole("dialog", { name: "グラフを入れる文書" });
  await expect(dialog).toBeVisible();
  // Excel・Word だけが並ぶ（ノートなどは出ない）。
  await expect(dialog.getByRole("button", { name: /\.md$/ })).toHaveCount(0);
  await dialog.getByRole("button", { name, exact: true }).click();
  await expect(toast(page, `「${name}」にグラフを入れました`)).toBeVisible({ timeout: 60_000 });
  await expect(page.getByRole("link", { name: "Word を開く" })).toHaveAttribute("href", `/office/${fileId}`);
  await page.screenshot({ path: "test-results/office-chart-insert-chat.png" });
  await expect.poll(() => savedFileHas(page, fileId, "word/charts/chart"), { timeout: 30_000 }).toBe(true);
});
