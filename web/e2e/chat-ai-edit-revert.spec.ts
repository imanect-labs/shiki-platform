import { expect, test, type Page } from "@playwright/test";

import { loginAs, loginViaKeycloak, uniqueName } from "./helpers";

/// AI による編集をチャットから取り消す（#1049）の E2E（前提: LLM=stub）。
///
/// - AI が csv.patch で CSV を編集 → 会話のカードの「元に戻す」で編集前に戻る（新しい版として）
/// - 「やり直す」で AI の編集が戻ってくる
/// - 編集の後に別の更新があると、確認ダイアログを出してから戻す
///
/// stub 駆動: `calltool:<ツール名> <入力 JSON>` でツールを決定的に呼ぶ。

async function csrf(page: Page): Promise<string> {
  return (await page.context().cookies()).find((c) => c.name === "shiki_csrf")?.value ?? "";
}

/// CSV を作って (id, version) を返す。
async function createCsv(page: Page, name: string, body: string) {
  const res = await page.request.post("/api/tabular/save", {
    headers: { "X-CSRF-Token": await csrf(page) },
    data: { parent_id: null, name, csv: body },
  });
  expect(res.status(), await res.text()).toBe(200);
  return (await res.json()) as { node_id: string; version: number };
}

/// 先頭データ行の 2 列目（value 列）。
async function firstValue(page: Page, id: string): Promise<string | null> {
  const res = await page.request.get(`/api/files/${id}/tabular/rows?offset=0`);
  expect(res.status()).toBe(200);
  const body = (await res.json()) as { rows: (string | null)[][] };
  return body.rows[0]?.[1] ?? null;
}

async function currentVersion(page: Page, id: string): Promise<number> {
  const res = await page.request.get(`/api/files/${id}`);
  expect(res.status()).toBe(200);
  return ((await res.json()) as { version: number }).version;
}

test("AI の編集をカードから元に戻し、やり直せる", async ({ page }) => {
  await loginViaKeycloak(page);
  const csv = await createCsv(page, `${uniqueName("revert")}.csv`, "key,value\na,original\n");

  // AI が csv.patch で編集する（書込は承認ゲートを通る）。
  await page.goto("/");
  const input = page.getByLabel("メッセージを入力");
  await input.fill(
    `calltool:csv.patch {"node_id":"${csv.node_id}","base_rev":${csv.version},"ops":[{"op":"cell_update","row":0,"col":1,"value":"ai-edited"}]}`,
  );
  await page.getByRole("button", { name: "送信" }).click();
  const approve = page.getByRole("button", { name: "承認して続行" });
  await expect(approve).toBeVisible({ timeout: 30_000 });
  await approve.click();

  const card = page.getByTestId("document-ref-card").last();
  await expect(card).toContainText("編集しました", { timeout: 30_000 });
  await expect.poll(() => firstValue(page, csv.node_id), { timeout: 15_000 }).toBe("ai-edited");

  // 元に戻す → 編集前の内容が新しい版として入る（履歴は残る）。
  await card.getByTestId("document-ref-undo").click();
  await expect(card).toContainText("元に戻しました", { timeout: 15_000 });
  await expect.poll(() => firstValue(page, csv.node_id)).toBe("original");
  expect(await currentVersion(page, csv.node_id)).toBe(csv.version + 2);

  // やり直す → AI の編集が戻ってくる。
  await card.getByTestId("document-ref-redo").click();
  await expect(card).toContainText("やり直しました", { timeout: 15_000 });
  await expect.poll(() => firstValue(page, csv.node_id)).toBe("ai-edited");

  // 別の更新が後から入った状態で元に戻す → 確認ダイアログ。やめると何も変わらない。
  const after = await currentVersion(page, csv.node_id);
  const patch = await page.request.post(`/api/files/${csv.node_id}/tabular/patch`, {
    headers: { "X-CSRF-Token": await csrf(page) },
    data: { base_rev: after, ops: [{ op: "row_insert", at: 1, values: ["b", "human"] }] },
  });
  expect(patch.status(), await patch.text()).toBe(200);
  await card.getByTestId("document-ref-undo").click();
  const dialog = page.getByTestId("document-ref-revert-conflict");
  await expect(dialog).toBeVisible({ timeout: 15_000 });
  await dialog.getByRole("button", { name: "やめる" }).click();
  await expect(dialog).toBeHidden();
  await expect.poll(() => firstValue(page, csv.node_id)).toBe("ai-edited");

  // それでも戻す → 編集前の内容に戻る（後の行追加も失われる）。
  await card.getByTestId("document-ref-undo").click();
  await page.getByTestId("document-ref-revert-force").click();
  await expect(card).toContainText("元に戻しました", { timeout: 15_000 });
  await expect.poll(() => firstValue(page, csv.node_id)).toBe("original");
});

/// alice → bob の共有を付ける（PUT）/外す（DELETE）。PUT は役割を足すだけで置き換えないので、
/// editor から viewer へ下げるときは editor を外してから viewer を付ける。
async function shareWithBob(
  page: Page,
  nodeId: string,
  role: "viewer" | "editor",
  method: "put" | "delete" = "put",
) {
  const res = await page.request[method](`/api/nodes/${nodeId}/shares`, {
    headers: { "X-CSRF-Token": await csrf(page) },
    data: { target: { type: "user", id: "00000000-0000-0000-0000-000000000002" }, role },
  });
  expect(res.status(), await res.text()).toBeLessThan(300);
}

test("編集権限の無い人には「元に戻す」を出さない", async ({ page, browser }) => {
  await loginViaKeycloak(page); // alice（ファイルの持ち主）
  const csv = await createCsv(page, `${uniqueName("shared")}.csv`, "key,value\na,original\n");
  await shareWithBob(page, csv.node_id, "editor");

  // bob（editor）が AI に編集させる → カードに「元に戻す」が出る。
  const bobCtx = await browser.newContext();
  const bob = await bobCtx.newPage();
  await loginAs(bob, "bob");
  // ページの時計を差し替える（普段は実時間で進む）。後で期限の前後を決定的に作るため、画面を開く前に入れる。
  await bob.clock.install();
  // 権限の問い合わせは、こちらが離すまで止めておく（最初の応答・権限を下げた後の引き直しとも）。
  const isAccess = (url: URL | string) =>
    new URL(url.toString()).pathname.endsWith(`/collab/docs/${csv.node_id}/access`);
  let gate = held();
  await bob.route(isAccess, async (route) => {
    await gate.released;
    await route.continue();
  });
  await bob.goto("/");
  await bob
    .getByLabel("メッセージを入力")
    .fill(
      `calltool:csv.patch {"node_id":"${csv.node_id}","base_rev":${csv.version},"ops":[{"op":"cell_update","row":0,"col":1,"value":"bob-ai"}]}`,
    );
  await bob.getByRole("button", { name: "送信" }).click();
  await bob.getByRole("button", { name: "承認して続行" }).click({ timeout: 30_000 });
  const card = bob.getByTestId("document-ref-card").last();
  await expect(card).toContainText("編集しました", { timeout: 30_000 });
  await expect(bob.getByRole("button", { name: "生成を停止" })).toHaveCount(0, { timeout: 30_000 });
  // 権限が分かるまでは出さない。最初の応答はカードの表示から 15 秒（ページの時計）遅らせ、
  // 結果の期限をカードの表示時刻からずらす（表示時刻起点の一定間隔では期限に追いつけないことを見分ける）。
  await expect(card.getByTestId("document-ref-undo")).toHaveCount(0);
  await bob.clock.fastForward(15_000);
  gate.release();
  await expect(card.getByTestId("document-ref-undo")).toBeVisible({ timeout: 15_000 });

  // alice が bob を閲覧のみに下げる。bob は**画面を開いたまま**（再読込も遷移もしない）。
  await shareWithBob(page, csv.node_id, "editor", "delete");
  await shareWithBob(page, csv.node_id, "viewer");
  gate = held();
  const refetch = bob.waitForRequest((r) => isAccess(r.url()), { timeout: 15_000 });
  // ページの時計を止め、結果の期限の直後まで（31 秒）だけ進める。期限ちょうどに引き直しが出ること。
  const now = await bob.evaluate(() => Date.now());
  await bob.clock.pauseAt(now + 500);
  await bob.clock.runFor(31_000);
  await refetch;
  // 期限が切れた結果は使わない: 引き直しの結果が来る前から「元に戻す」は出ない。
  await expect(card.getByTestId("document-ref-undo")).toHaveCount(0, { timeout: 15_000 });
  const access = bob.waitForResponse((r) => isAccess(r.url()), { timeout: 15_000 });
  gate.release();
  expect(((await (await access).json()) as { mode: string }).mode).toBe("viewer");
  await expect(card.getByTestId("document-ref-undo")).toHaveCount(0);
  await expect(card).toContainText("編集しました");
  await bobCtx.close();
});

/// 離すまで待たせる門（`released` は `release()` で解決する）。
function held(): { released: Promise<void>; release: () => void } {
  let release = () => {};
  const released = new Promise<void>((r) => (release = r));
  return { released, release };
}
