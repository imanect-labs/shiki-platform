import { expect, test, type Page } from "@playwright/test";

import { loginViaKeycloak, uniqueName } from "./helpers";

/// スキルエディタ（#919）の受け入れ条件（前提: LLM=stub）:
/// - アシスタントに頼むと save_skill の下書きがエディタへ流し込まれ、そのまま作成できる
/// - ドライブから選んだフォルダが、ID ではなくパスの青いラベルで本文に入り、保存後も残る
/// - 通常のチャットで作った下書きスキルはカードからエディタで開ける
///
/// stub 駆動: `saveskill:<name>` → save_skill（固定の説明・指示文・使うツール）。

async function csrf(page: Page): Promise<string> {
  return (await page.context().cookies()).find((c) => c.name === "shiki_csrf")?.value ?? "";
}

async function createFolder(page: Page, name: string, parentId?: string): Promise<string> {
  const res = await page.request.post("/api/folders", {
    headers: { "X-CSRF-Token": await csrf(page) },
    data: { name, parent_id: parentId ?? null },
  });
  expect(res.status(), await res.text()).toBeLessThan(300);
  return ((await res.json()) as { id: string }).id;
}

type SavedBody = {
  instructions: string;
  knowledge_scope: unknown;
  scripts: { path: string }[];
  description: string;
};

/// 保存済みスキルの本文（API）を読む。
async function skillBody(page: Page, id: string): Promise<SavedBody> {
  const res = await page.request.get(`/api/skills/${id}`);
  expect(res.status()).toBe(200);
  return ((await res.json()) as { body: SavedBody }).body;
}

/// API でスキルを作る（既存スキルの編集の前提づくり）。
async function createSkill(page: Page, name: string, body: Record<string, unknown>): Promise<string> {
  const res = await page.request.post("/api/skills", {
    headers: { "X-CSRF-Token": await csrf(page) },
    data: { name, body: { description: "既存のスキル", instructions: "x", ...body } },
  });
  expect(res.status(), await res.text()).toBeLessThan(300);
  return ((await res.json()) as { id: string }).id;
}

test("アシスタントに頼むと下書きが流し込まれ、そのまま作成できる", async ({ page }) => {
  await loginViaKeycloak(page);
  await page.goto("/skills/new");

  // 新規はアシスタントと一緒に開く。
  const assistant = page.getByTestId("skill-assistant");
  await expect(assistant.getByTestId("skill-assistant-hint")).toBeVisible();
  // 編集中のスキルが入力欄に添えられている。
  await expect(assistant.getByTestId("selection-chip")).toContainText("編集中のスキル");

  const name = uniqueName("ai-skill");
  await assistant.getByLabel("メッセージを入力").fill(`saveskill:${name}`);
  await assistant.getByRole("button", { name: "送信" }).click();

  // 下書きがエディタへ入る（名前・説明・本文・使うツール）。
  await expect(page.getByTestId("skill-name-input")).toHaveValue(name, { timeout: 30_000 });
  await expect(page.getByTestId("skill-description-input")).toHaveValue(
    "経費精算の質問に社内規程を根拠に答える",
  );
  await expect(page.getByTestId("skill-body-editor")).toContainText("経費精算アシスタント");
  await expect(page.getByRole("button", { name: "社内文書検索" })).toHaveAttribute("aria-pressed", "true");
  // 会話にも下書きカードが残り、パネル内では「エディタに反映」になる。
  await expect(assistant.getByTestId("skill-draft-apply")).toBeVisible();
  // 会話は URL に残る（再読み込みでも続けられる）。
  await expect(page).toHaveURL(/\/skills\/new\?thread=/);

  await page.getByTestId("skill-save-button").click();
  await page.waitForURL(/\/skills\/[0-9a-f-]{36}/i, { timeout: 15_000 });
  await expect(page.getByRole("heading", { name })).toBeVisible();
});

test("ドライブのフォルダをパスのラベルで参照し、保存後も残る", async ({ page }) => {
  await loginViaKeycloak(page);
  const parent = uniqueName("規程");
  const child = uniqueName("経費");
  const parentId = await createFolder(page, parent);
  const childId = await createFolder(page, child, parentId);

  await page.goto("/skills/new");
  // 本文の操作に集中するためアシスタントは閉じる。
  await page.getByTestId("skill-chat-toggle").first().click();
  const name = uniqueName("ref-skill");
  await page.getByTestId("skill-name-input").fill(name);
  await page.getByTestId("skill-description-input").fill("参照の E2E");

  // 「@」でピッカーを開き、検索して子フォルダを選ぶ。
  const body = page.getByTestId("skill-body-editor");
  await body.click();
  await page.keyboard.type("規程は @");
  const picker = page.getByRole("dialog", { name: "ドライブから参照" });
  await expect(picker).toBeVisible();
  await picker.getByTestId("drive-ref-search").fill(child);
  await picker.getByRole("button", { name: `「${child}」を選択` }).click();
  await expect(picker).toBeHidden();

  // ID ではなくパスのラベルが入り、「@」は置き換わる。
  const chip = body.getByTestId("drive-ref-chip");
  await expect(chip).toHaveText(`${parent}/${child}`);
  await expect(body).not.toContainText("@");
  await page.keyboard.type("を参照して答える。");

  // 知識スコープもパスで出る（フォルダピッカーから追加）。
  await page.getByTestId("skill-scope-add").click();
  const scopePicker = page.getByRole("dialog", { name: "フォルダを選択" });
  await scopePicker.getByTestId("drive-ref-search").fill(parent);
  await scopePicker.getByRole("button", { name: `「${parent}」を選択` }).click();
  await expect(page.getByRole("region", { name: "スキルの設定" }).getByTestId("drive-ref-chip")).toHaveText(
    parent,
  );

  await page.getByTestId("skill-save-button").click();
  await page.waitForURL(/\/skills\/([0-9a-f-]{36})/i, { timeout: 15_000 });
  const id = page.url().split("/skills/")[1];

  // 保存形式は Markdown リンク（スキルを使う AI は id を doc_read 等に渡せる）。ラベルは
  // 項目自身の名前だけ（祖先フォルダの名前は指示文に書き込まない）。
  const saved = await skillBody(page, id);
  expect(saved.instructions).toContain(`[${child}](shiki://drive/folder/${childId})`);
  expect(saved.knowledge_scope).toEqual({ folders: [parentId], files: [] });

  // 開き直してもパスのラベルで出る。
  await page.reload();
  await expect(page.getByTestId("skill-body-editor").getByTestId("drive-ref-chip")).toHaveText(
    `${parent}/${child}`,
  );
});

test("通常のチャットで作った下書きスキルをカードからエディタで開ける", async ({ page }) => {
  await loginViaKeycloak(page);
  const name = uniqueName("chat-skill");
  await page.goto("/");
  await page.getByLabel("メッセージを入力").fill(`saveskill:${name}`);
  await page.getByRole("button", { name: "送信" }).click();

  // run の完了後にスキルエディタへ移り、下書きが入っている。
  await page.waitForURL(/\/skills\/new\?thread=.+&name=/, { timeout: 30_000 });
  await expect(page.getByTestId("skill-name-input")).toHaveValue(name, { timeout: 20_000 });
  await expect(page.getByTestId("skill-body-editor")).toContainText("経費精算アシスタント");

  // 手で直した内容は再読み込みでも残る（下書きストアへ書き戻す）。
  await page.getByTestId("skill-description-input").fill("手で直した説明");
  await page.waitForTimeout(600);
  await page.reload();
  await expect(page.getByTestId("skill-description-input")).toHaveValue("手で直した説明", {
    timeout: 20_000,
  });
});

test("既存の指示文のタグや記号は、編集して保存しても書き換わらない", async ({ page }) => {
  await loginViaKeycloak(page);
  const instructions =
    "<rules>\n- doc_search で規程を探す\n- snake_case の名前はそのまま\n</rules>\n\n<example>入力 a*b</example> を参考にする。\n";
  const id = await createSkill(page, uniqueName("literal-skill"), { instructions });
  await page.goto(`/skills/${id}`);
  const body = page.getByTestId("skill-body-editor");
  await expect(body).toContainText("<rules>");
  await page.evaluate(() =>
    (window as unknown as { __shikiSkillEditor: { commands: { focus: (p: string) => void } } })
      .__shikiSkillEditor.commands.focus("end"),
  );
  await page.keyboard.type("追記");
  await page.getByTestId("skill-save-button").click();
  await expect(page.getByText("保存済み v2").first()).toBeVisible({ timeout: 15_000 });

  const saved = (await skillBody(page, id)).instructions;
  expect(saved).toContain("<rules>");
  expect(saved).toContain("</rules>");
  expect(saved).toContain("doc_search");
  expect(saved).toContain("snake_case");
  expect(saved).not.toContain("```html");
  expect(saved).toContain("追記");
});

test("AI に直してもらっても、AI が触れなかった項目（script）は消えない", async ({ page }) => {
  await loginViaKeycloak(page);
  const name = uniqueName("keep-skill");
  const id = await createSkill(page, name, {
    scripts: [{ path: "scripts/report.shiki", kind: "shiki", source: "return 1;" }],
  });
  await page.goto(`/skills/${id}`);
  await page.getByTestId("skill-chat-toggle").first().click();
  const assistant = page.getByTestId("skill-assistant");
  await assistant.getByLabel("メッセージを入力").fill(`saveskill:${name}`);
  await assistant.getByRole("button", { name: "送信" }).click();
  await expect(page.getByTestId("skill-body-editor")).toContainText("経費精算アシスタント", {
    timeout: 30_000,
  });
  // 名前は既存のまま・script は残っている。
  await expect(page.getByRole("heading", { name })).toBeVisible();
  await expect(page.getByLabel("script 1 のパス")).toHaveValue("scripts/report.shiki");

  await page.getByTestId("skill-save-button").click();
  await expect(page.getByText("保存済み v2").first()).toBeVisible({ timeout: 15_000 });
  const saved = await skillBody(page, id);
  expect(saved.scripts.map((s) => s.path)).toEqual(["scripts/report.shiki"]);
  expect(saved.description).toBe("経費精算の質問に社内規程を根拠に答える");
});

test("全角の「＠」でもドライブの参照を開ける", async ({ page }) => {
  await loginViaKeycloak(page);
  await page.goto("/skills/new");
  await page.getByTestId("skill-chat-toggle").first().click();
  await page.getByTestId("skill-body-editor").click();
  await page.keyboard.insertText("規程は ＠");
  await expect(page.getByRole("dialog", { name: "ドライブから参照" })).toBeVisible();
});
