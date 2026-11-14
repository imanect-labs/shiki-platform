import { expect, test, type Page } from "@playwright/test";

import {
  cleanupQueue,
  createFolderApi,
  loginViaKeycloak,
  trackCreatedResources,
  trashFolderApi,
  uniqueName,
} from "./helpers";

/// アップロードの前の同じ名前の確かめ（BUG-199）:
/// - 見えるものが多い人では、名前の検索（`GET /nodes?q=`）の先頭のページが 0 件で続きの位置だけを返す。以前の画面は
///   その先頭のページで同じ名前を探したので、同じ名前があっても見つけられず、送り切ってから確定の 409 で気づき、
///   「新しいバージョンとして保存」が出なかった。名前の検索の応答をその形に差し替えても、ドライブのアップロードは
///   送る前に同じ名前を見つけ、「新しいバージョンとして保存」を出す（名前の完全一致の引き当て `GET /nodes/by-name`）。
/// - チャットの添付（同じ名前なら番号を付けて置く）も、同じ名前を送ってから断られて送り直さない。
/// 作ったもの（作業用のフォルダー・添付で置いたファイル）は最後に片付ける（失敗したテストでも走る）。
/// `E2E_SHOTS_DIR` があれば、PC（1440px）とスマホ（390px）の画面を撮る。

const SHOTS = process.env.E2E_SHOTS_DIR;
const cleanup = cleanupQueue();
test.afterEach(() => cleanup.run());

/// 名前の検索の応答を、見えるものが多い人の先頭のページ（0 件・続きの位置だけ）に差し替える（差し替えた回数を返す）。
async function emptySearchPages(page: Page): Promise<() => number> {
  let hits = 0;
  await page.route(/\/api\/nodes\?(.*&)?q=/, async (route) => {
    hits += 1;
    await route.fulfill({ json: { items: [], next_cursor: "opaque-next" } });
  });
  return () => hits;
}

/// 確定（finalize）が同じ名前で断られた回数（送ってから気づいた回数）。
function rejectedFinalizes(page: Page): () => number {
  let n = 0;
  page.on("response", (res) => {
    if (/\/api\/files\/[^/]+\/finalize$/.test(new URL(res.url()).pathname) && res.status() === 409) n += 1;
  });
  return () => n;
}

test("ドライブ: 名前の検索の先頭のページに出なくても、送る前に同じ名前を見つけて新しいバージョンとして保存できる（BUG-199）", async ({
  page,
}) => {
  test.setTimeout(120_000);
  await page.setViewportSize({ width: 1440, height: 900 });
  await loginViaKeycloak(page);
  const work = await createFolderApi(page, uniqueName("e2e-same-name"));
  cleanup.add(() => trashFolderApi(page, work));
  await page.goto(`/drive?folder=${work}`);
  const fileName = `${uniqueName("報告書")}.txt`;
  const input = page.locator('input[type="file"][multiple]');
  // 送り切るまで待つ（送っている間も一覧の進み具合に名前が出るので、名前では待たない）。
  const finalized = page.waitForResponse(
    (res) => /\/api\/files\/[^/]+\/finalize$/.test(new URL(res.url()).pathname) && res.ok(),
  );
  await input.setInputFiles({ name: fileName, mimeType: "text/plain", buffer: Buffer.from("version one\n") });
  await finalized;
  await expect(page.getByText("アップロードが完了しました").first()).toBeVisible({ timeout: 20_000 });

  const searched = await emptySearchPages(page);
  const rejected = rejectedFinalizes(page);
  const lookups: string[] = [];
  page.on("request", (req) => {
    if (new URL(req.url()).pathname === "/api/nodes/by-name") lookups.push(req.url());
  });

  // PC: 同じ名前をもう一度アップロードすると、送る前に問い合わせが出て、新しいバージョンとして保存できる。
  await input.setInputFiles({ name: fileName, mimeType: "text/plain", buffer: Buffer.from("version two\n") });
  const dialog = page.getByTestId("upload-conflict");
  await expect(dialog).toBeVisible({ timeout: 20_000 });
  await expect(dialog.getByText(/^既存: /)).toBeVisible();
  await expect(dialog.getByTestId("conflict-version")).toBeVisible();
  expect(rejected(), "送ってから断られていない").toBe(0);
  expect(searched(), "名前の検索の先頭のページで探さない").toBe(0);
  expect(lookups.length, "名前の完全一致で引く").toBeGreaterThan(0);
  if (SHOTS) await page.screenshot({ path: `${SHOTS}/drive-same-name-pc.png` });
  await dialog.getByTestId("conflict-version").click();
  await expect(dialog).toBeHidden();
  await expect(page.getByText("保存が完了しました").first()).toBeVisible({ timeout: 20_000 });
  const res = await page.request.get(
    `/api/nodes/by-name?parent_id=${work}&name=${encodeURIComponent(fileName)}`,
  );
  expect(res.ok(), await res.text()).toBeTruthy();
  expect(((await res.json()) as { node: { version: number } | null }).node?.version).toBe(2);

  // スマホ: 下からのシートで同じ問い合わせが出る（スキップで閉じる）。
  await page.setViewportSize({ width: 390, height: 844 });
  await page.goto(`/drive?folder=${work}`);
  await expect(page.getByText(fileName, { exact: true })).toBeVisible({ timeout: 20_000 });
  await page.locator('input[type="file"][multiple]').setInputFiles({
    name: fileName,
    mimeType: "text/plain",
    buffer: Buffer.from("version three\n"),
  });
  await expect(dialog).toBeVisible({ timeout: 20_000 });
  await expect(dialog.getByTestId("conflict-version")).toBeVisible();
  expect(rejected()).toBe(0);
  expect(await page.evaluate(() => document.documentElement.scrollWidth)).toBeLessThanOrEqual(390);
  if (SHOTS) await page.screenshot({ path: `${SHOTS}/drive-same-name-phone.png` });
  await dialog.getByTestId("conflict-skip").click();
  await expect(dialog).toBeHidden();
});

test("チャットの添付: 名前の検索の先頭のページに出なくても、同じ名前を送らずに番号を付けて置く（BUG-199）", async ({
  page,
}) => {
  test.setTimeout(120_000);
  trackCreatedResources(page, cleanup);
  await loginViaKeycloak(page);
  await page.goto("/");
  const name = `${uniqueName("attach")}.txt`;
  const numbered = name.replace(/\.txt$/, " (1).txt");
  const input = page.locator('input[type="file"]:not([multiple])').first();
  const chips = page.getByRole("button", { name: "添付を外す" }).locator("..");
  await input.setInputFiles({ name, mimeType: "text/plain", buffer: Buffer.from("first\n") });
  await expect(chips.getByText(name, { exact: true })).toBeVisible({ timeout: 20_000 });

  const searched = await emptySearchPages(page);
  const rejected = rejectedFinalizes(page);
  await input.setInputFiles({ name, mimeType: "text/plain", buffer: Buffer.from("second\n") });
  await expect(chips.getByText(numbered, { exact: true })).toBeVisible({ timeout: 20_000 });
  expect(rejected(), "同じ名前を送ってから断られていない").toBe(0);
  expect(searched(), "名前の検索の先頭のページで探さない").toBe(0);
  await expect(page.getByText(`${name} のアップロードに失敗しました`)).toHaveCount(0);
});
