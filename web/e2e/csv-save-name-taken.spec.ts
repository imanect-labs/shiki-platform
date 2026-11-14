import { expect, test, type Page } from "@playwright/test";

import { cleanupQueue, loginViaKeycloak, trackCreatedResources, uniqueName } from "./helpers";

/// 新しい CSV を保存するときに、保存先に同じ名前のファイルがある（BUG-150）。
///
/// 以前は、SQL の結果の「新規 CSV として保存」で同じ名前を入れると「CSV が他の編集で更新されました」と出て（名前の
/// 衝突の 409 を版の競合と取り違えていた）、CSV の下書きの保存は「保存に失敗しました。」とだけ出ていた。いまは
/// `POST /tabular/save` が code の name_taken を返し、画面は名前の欄の下で同じ名前があると言い、「番号を付けて保存」で
/// 「名前 (2).csv」に保存できる。名前を変えると知らせは消える。
///
/// 作った CSV（元の CSV・保存した CSV）は、テストの後にゴミ箱へ移す（BUG-126）。

const SHOTS = process.env.E2E_SHOTS_DIR;

const cleanup = cleanupQueue();
test.beforeEach(({ page }) => trackCreatedResources(page, cleanup));
test.afterEach(() => cleanup.run());

/// CSV を画面と同じ API（ブラウザーから）で作る（片付けは trackCreatedResources が積む）。
async function createCsv(page: Page, name: string, csv: string): Promise<string> {
  return page.evaluate(
    async ({ n, body }) => {
      const csrf = document.cookie.match(/(?:^|;\s*)shiki_csrf=([^;]+)/)?.[1] ?? "";
      const res = await fetch("/api/tabular/save", {
        method: "POST",
        credentials: "include",
        headers: { "Content-Type": "application/json", "X-CSRF-Token": csrf },
        body: JSON.stringify({ parent_id: null, name: n, csv: body }),
      });
      if (!res.ok) throw new Error(`CSV 作成に失敗: ${res.status}`);
      return (await res.json()).node_id as string;
    },
    { n: name, body: csv },
  );
}

/// CSV を開き、SQL の画面で集計を流して結果を出す（スマホは見出しのアイコンで SQL に切り替える）。
async function runQuery(page: Page, id: string, phone = false) {
  await page.goto(`/csv/${id}`);
  await page.getByTestId(phone ? "csv-tab-toggle" : "csv-tab-sql").click({ timeout: 30_000 });
  await expect(page.getByTestId("sql-console")).toBeVisible({ timeout: 20_000 });
  await page.getByTestId("sql-input").fill("SELECT id, score FROM data WHERE CAST(score AS INT) >= 20 ORDER BY id");
  await page.getByTestId("sql-run").click({ timeout: 20_000 });
  await expect(page.locator('[data-testid="sql-console"] table tbody tr')).toHaveCount(2, { timeout: 20_000 });
}

test("SQL の結果を同じ名前で保存すると、同じ名前があると言い、番号を付けて保存できる（PC・スマホ）", async ({
  page,
  browser,
}) => {
  test.setTimeout(180_000);
  await page.setViewportSize({ width: 1440, height: 900 });
  await loginViaKeycloak(page);
  await page.goto("/drive");
  const source = await createCsv(page, `${uniqueName("得点")}.csv`, "id,score\n1,10\n2,30\n3,20\n");
  const saveName = uniqueName("集計");

  await runQuery(page, source);
  const dialog = page.getByTestId("sql-save-dialog");
  // 1 回目: その名前で保存できる（保存先はドライブのトップと言う）。
  await page.getByTestId("sql-save").click();
  await expect(dialog.getByText("結果を新しい CSV として、ドライブのトップに保存します。")).toBeVisible();
  await dialog.getByLabel("ファイル名").fill(saveName);
  await dialog.getByRole("button", { name: "保存", exact: true }).click();
  await expect(dialog).toBeHidden({ timeout: 20_000 });
  await expect(page.getByText(`「${saveName}.csv」を作成しました。`, { exact: true })).toBeVisible();

  // 2 回目: 同じ名前は、編集の重なりと取り違えずに同じ名前があると言う。
  await page.getByTestId("sql-save").click();
  await dialog.getByLabel("ファイル名").fill(saveName);
  await dialog.getByRole("button", { name: "保存", exact: true }).click();
  const taken = dialog.getByTestId("sql-save-name-taken");
  await expect(taken).toHaveText(`保存先に「${saveName}.csv」がすでにあります。名前を変えるか、番号を付けて保存してください。`, {
    timeout: 20_000,
  });
  await expect(page.getByText("CSV が他の編集で更新されました")).toHaveCount(0);
  await expect(dialog.getByRole("button", { name: "保存", exact: true })).toHaveCount(0);
  await expect(dialog.getByLabel("ファイル名")).toHaveAttribute("aria-invalid", "true");
  if (SHOTS) await page.screenshot({ path: `${SHOTS}/csv-save-name-taken-pc.png` });

  // 名前を変えると知らせは消え、「保存」に戻る。元の名前に戻すと、また知らせが出る。
  await dialog.getByLabel("ファイル名").fill(`${saveName}x`);
  await expect(taken).toHaveCount(0);
  await expect(dialog.getByRole("button", { name: "保存", exact: true })).toBeEnabled();
  await dialog.getByLabel("ファイル名").fill(saveName);
  await expect(taken).toBeVisible();

  // 番号を付けて保存すると「名前 (2).csv」になる。
  await dialog.getByTestId("sql-save-numbered").click();
  await expect(dialog).toBeHidden({ timeout: 20_000 });
  await expect(page.getByText(`「${saveName} (2).csv」を作成しました。`, { exact: true })).toBeVisible();

  // スマホ: 同じ知らせが名前の欄の下に出て、横にはみ出さない。
  const phoneCtx = await browser.newContext({
    storageState: await page.context().storageState(),
    viewport: { width: 390, height: 844 },
    hasTouch: true,
    isMobile: true,
    deviceScaleFactor: 3,
    ignoreHTTPSErrors: true,
  });
  try {
    const phone = await phoneCtx.newPage();
    await runQuery(phone, source, true);
    await phone.getByTestId("sql-save").click({ timeout: 20_000 });
    const sheet = phone.getByTestId("sql-save-dialog");
    await sheet.getByLabel("ファイル名").fill(saveName, { timeout: 20_000 });
    await sheet.getByRole("button", { name: "保存", exact: true }).click({ timeout: 20_000 });
    await expect(sheet.getByTestId("sql-save-name-taken")).toBeVisible({ timeout: 20_000 });
    await expect(sheet.getByTestId("sql-save-numbered")).toBeVisible();
    expect(await phone.evaluate(() => document.documentElement.scrollWidth)).toBeLessThanOrEqual(390);
    if (SHOTS) await phone.screenshot({ path: `${SHOTS}/csv-save-name-taken-phone.png` });
    await sheet.getByRole("button", { name: "キャンセル" }).click({ timeout: 20_000 });
    await expect(sheet).toBeHidden();
  } finally {
    await phoneCtx.close();
  }
});

test("CSV の下書きを同じ名前で保存すると、同じ名前があると言い、番号を付けて保存できる", async ({ page }) => {
  test.setTimeout(180_000);
  await page.setViewportSize({ width: 1440, height: 900 });
  await loginViaKeycloak(page);
  await page.goto("/");
  const name = uniqueName("集計の下書き");
  // ドライブのトップに、下書きと同じ名前の CSV を先に置く。
  await createCsv(page, `${name}.csv`, "列1,列2\n1,2\n");

  // チャットで下書きを作る（stub: `savecsv:<name>`）。
  const input = page.getByLabel("メッセージを入力");
  await input.fill(`savecsv:${name}`);
  await page.getByRole("button", { name: "送信" }).click();
  await page.waitForURL(/\/csv\/draft/, { timeout: 25_000 });
  await expect(page.getByTestId("csv-draft-grid")).toBeVisible({ timeout: 20_000 });

  await page.getByTestId("draft-save-button").click();
  await expect(page.getByTestId("save-draft-name")).toHaveValue(name);
  await page.getByTestId("save-draft-confirm").click();
  const taken = page.getByTestId("save-draft-name-taken");
  await expect(taken).toHaveText(`保存先に「${name}.csv」がすでにあります。名前を変えるか、番号を付けて保存してください。`, {
    timeout: 20_000,
  });
  await expect(page.getByText("保存に失敗しました。")).toHaveCount(0);
  await expect(page.getByTestId("save-draft-confirm")).toHaveCount(0);
  if (SHOTS) await page.screenshot({ path: `${SHOTS}/csv-draft-name-taken-pc.png` });

  // 名前を変えると知らせは消え、戻すとまた出る。「番号を付けて保存」を押すと「名前 (2).csv」で開く。
  await page.getByTestId("save-draft-name").fill(`${name}x`);
  await expect(taken).toHaveCount(0);
  await expect(page.getByTestId("save-draft-confirm")).toBeEnabled();
  await page.getByTestId("save-draft-name").fill(name);
  await expect(taken).toBeVisible();
  await page.getByTestId("save-draft-numbered").click();
  await page.waitForURL(/\/csv\/[0-9a-f-]{36}/i, { timeout: 25_000 });
  const id = page.url().match(/\/csv\/([0-9a-f-]{36})/i)![1];
  const file = (await (await page.request.get(`/api/files/${id}`)).json()) as { name: string };
  expect(file.name).toBe(`${name} (2).csv`);
});
