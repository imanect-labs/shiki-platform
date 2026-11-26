import { expect, test, type Page } from "@playwright/test";

import { loginViaKeycloak, uniqueName } from "./helpers";

/// 承認カードの中身（#1339・前提: LLM=stub）。
///
/// 生のツール名・共通の定型文・入力の JSON の代わりに、どのファイルに（名前と場所）・何をして（操作ごとに 1 行）・
/// 何が起きるか（副作用）を読める形で出す。生の JSON は「送る内容（JSON）」に畳む。
///
/// stub 駆動: `calltool:<ツール名> <入力 JSON>` でツールを決定的に呼ぶ。承認せず却下して、ファイルは変えない。

async function csrf(page: Page): Promise<string> {
  return (await page.context().cookies()).find((c) => c.name === "shiki_csrf")?.value ?? "";
}

async function createFolder(page: Page, name: string): Promise<string> {
  const res = await page.request.post("/api/folders", {
    headers: { "X-CSRF-Token": await csrf(page) },
    data: { name, parent_id: null },
  });
  expect(res.status(), await res.text()).toBeLessThan(300);
  return ((await res.json()) as { id: string }).id;
}

async function createNote(page: Page, name: string, parentId: string): Promise<string> {
  const res = await page.request.post("/api/notes", {
    headers: { "X-CSRF-Token": await csrf(page) },
    data: { name, parent_id: parentId, markdown: `# ${name}\n\n## 概要\n\n旧社名の説明。\n` },
  });
  expect(res.status(), await res.text()).toBe(200);
  return ((await res.json()) as { id: string }).id;
}

async function ask(page: Page, text: string) {
  const input = page.getByLabel("メッセージを入力");
  await input.fill(text);
  await page.getByRole("button", { name: "送信" }).click();
}

test("ノートの編集の承認カードに、対象のノートと場所・操作ごとの内容・起きることを出す", async ({ page }) => {
  test.setTimeout(120_000);
  await loginViaKeycloak(page);
  const folderName = uniqueName("承認カードのフォルダ");
  const folderId = await createFolder(page, folderName);
  const noteName = uniqueName("承認カードのメモ");
  const noteId = await createNote(page, noteName, folderId);

  await page.goto("/");
  await ask(
    page,
    `calltool:document.edit ${JSON.stringify({
      node_id: noteId,
      ops: [
        { op: "replace_text", find: "旧社名", replace: "新社名" },
        { op: "delete_section", heading: "概要" },
      ],
    })}`,
  );
  const card = page.getByTestId("approval-card");
  await expect(card).toBeVisible({ timeout: 30_000 });

  // どのファイルに: ノートの名前と今の場所。
  await expect(card.getByTestId("approval-headline")).toHaveText(new RegExp(`${noteName}.* を編集します`));
  await expect(card.getByTestId("approval-location")).toContainText(folderName);
  // 何をして: 操作ごとに 1 行。消す操作は印を付ける。
  const steps = card.getByTestId("approval-steps").getByRole("listitem");
  await expect(steps).toHaveCount(2);
  await expect(steps.nth(0)).toHaveText("「旧社名」を「新社名」に置き換え");
  await expect(steps.nth(1)).toHaveText("見出し「概要」の節を見出しごと削除");
  await expect(steps.nth(1)).toHaveAttribute("data-destructive", "true");
  // 何が起きるか。
  const effects = card.getByTestId("approval-effects");
  await expect(effects).toContainText("内容を消す操作が 1 件あります");
  await expect(effects).toContainText("開いている人の画面にもすぐ出ます");
  // 生のツール名・定型文・JSON は前に出さない（畳んだ中にだけある）。
  await expect(card.getByText("破壊的/権限/高コストな操作のため承認が必要です")).toHaveCount(0);
  const json = card.locator("details");
  await expect(json.locator("pre")).toBeHidden();
  await json.locator("summary").click();
  await expect(json.locator("pre")).toContainText(noteId);

  await card.getByRole("button", { name: "却下" }).click();
  await expect(card).toHaveCount(0, { timeout: 30_000 });
});

test("新しい CSV の承認カードに、ファイル名・保存先・行数と、新しいファイルができることを出す", async ({ page }) => {
  test.setTimeout(120_000);
  await loginViaKeycloak(page);
  const folderName = uniqueName("承認カードの保存先");
  const folderId = await createFolder(page, folderName);
  const csvName = uniqueName("承認カードの表");

  await page.goto("/");
  await ask(
    page,
    `calltool:csv.write ${JSON.stringify({ name: csvName, csv: "key,value\na,1\nb,2\n", parent_id: folderId })}`,
  );
  const card = page.getByTestId("approval-card");
  await expect(card).toBeVisible({ timeout: 30_000 });

  await expect(card.getByTestId("approval-headline")).toHaveText(`「${csvName}」を CSV で作成します`);
  await expect(card.getByTestId("approval-folder")).toHaveText(`保存先: ${folderName}`);
  await expect(card.getByTestId("approval-steps")).toHaveText("見出しの行と 2 行のデータで CSV を作る");
  await expect(card.getByTestId("approval-effects")).toContainText("ドライブに新しいファイルを作ります");

  await card.getByRole("button", { name: "却下" }).click();
  await expect(card).toHaveCount(0, { timeout: 30_000 });
});
