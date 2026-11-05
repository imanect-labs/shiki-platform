import { expect, test, type Page } from "@playwright/test";

import { loginViaKeycloak, uniqueName } from "./helpers";

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
