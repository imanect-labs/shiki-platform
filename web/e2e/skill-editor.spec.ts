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

type EditorHandle = {
  commands: { focus: (p: string) => void };
  view: { hasFocus: () => boolean };
  getText: () => string;
};

/// 本文の末尾へキャレットを置く。フォーカスは次の描画で移るので、移るのを待ってから打つ
/// （人がキャレットを見てから打つのと同じ。待たないと打った文字がどこにも入らない）。
async function focusBodyEnd(page: Page) {
  await page.evaluate(() =>
    (window as unknown as { __shikiSkillEditor: EditorHandle }).__shikiSkillEditor.commands.focus("end"),
  );
  await expect
    .poll(() =>
      page.evaluate(() =>
        (window as unknown as { __shikiSkillEditor: EditorHandle }).__shikiSkillEditor.view.hasFocus(),
      ),
    )
    .toBe(true);
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
  // 詳細設定は畳んであるが、AI が入れた値は見出しの横の要約で分かる。
  await expect(page.getByTestId("skill-settings-summary")).toContainText("ツール: 2 件");
  await page.getByTestId("skill-settings-toggle").click();
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
  await page.getByTestId("skill-settings-toggle").click();
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
    "<rules>\n- doc_search で規程を探す\n- snake_case の名前はそのまま\n</rules>\n\n" +
    "<example>入力 a*b</example> を参考にする。\n\n" +
    "記号: [必須] C:\\share\\規程 A|B x~y\n";
  const id = await createSkill(page, uniqueName("literal-skill"), { instructions });
  await page.goto(`/skills/${id}`);
  const body = page.getByTestId("skill-body-editor");
  await expect(body).toContainText("<rules>");
  await focusBodyEnd(page);
  await page.keyboard.type("追記");
  // 日本語は 1 回の DOM 挿入で入るので、エディタの状態に読み込まれてから次のキーを押す。
  await expect
    .poll(() =>
      page.evaluate(() =>
        (window as unknown as { __shikiSkillEditor: { getText: () => string } })
          .__shikiSkillEditor.getText(),
      ),
    )
    .toContain("追記");
  // 強制改行の後の行頭「>」は引用にせず文字のまま残す。
  await page.keyboard.press("Shift+Enter");
  await page.keyboard.type("> 10 件なら要約する");
  await page.getByTestId("skill-save-button").click();
  await expect(page.getByText("保存済み v2").first()).toBeVisible({ timeout: 15_000 });

  const saved = (await skillBody(page, id)).instructions;
  // 記法に化けない記号は書き換えない（バックスラッシュを足さない）。
  expect(saved).toContain("a*b");
  expect(saved).toContain("[必須] C:\\share\\規程 A|B x~y");
  // 開き直しても引用にならない（本文に「>」の文字が残る）。
  await page.reload();
  await expect(page.getByTestId("skill-body-editor")).toContainText("> 10 件なら要約する");
  await expect(page.getByTestId("skill-body-editor").locator("blockquote")).toHaveCount(0);
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

test("参照のラベルの括弧は、保存してもリンクを壊さない", async ({ page }) => {
  await loginViaKeycloak(page);
  const folderId = await createFolder(page, uniqueName("規程"));
  // 名前に「]」を含む項目の参照（ラベルは入れたときの名前）。
  const instructions = `次を参照: [旧\\[規程\\]](shiki://drive/folder/${folderId})`;
  const id = await createSkill(page, uniqueName("bracket-skill"), { instructions });
  await page.goto(`/skills/${id}`);
  await expect(page.getByTestId("skill-body-editor").getByTestId("drive-ref-chip")).toHaveCount(1);
  await focusBodyEnd(page);
  await page.keyboard.type("x");
  await page.getByTestId("skill-save-button").click();
  await expect(page.getByText("保存済み v2").first()).toBeVisible({ timeout: 15_000 });

  // 括弧はエスケープされ、読み直しても 1 つの参照のまま（リンクの外へ文字がこぼれない）。
  const saved = (await skillBody(page, id)).instructions;
  expect(saved).toContain(`[旧\\[規程\\]](shiki://drive/folder/${folderId})`);
  await page.reload();
  const body = page.getByTestId("skill-body-editor");
  await expect(body.getByTestId("drive-ref-chip")).toHaveCount(1);
  await expect(body).not.toContainText("shiki://");
});

test("未保存の変更があるとき、アプリ内のリンクで離れる前に確かめる", async ({ page }) => {
  await loginViaKeycloak(page);
  const id = await createSkill(page, uniqueName("guard-skill"), {});
  await page.goto(`/skills/${id}`);
  await page.getByTestId("skill-description-input").fill("直しかけの説明");

  // サイドバーなどのリンクで離れようとすると、破棄の確認が出る。
  await page.locator('a[href="/skills"]').first().click();
  const dialog = page.getByTestId("text-discard-dialog");
  await expect(dialog).toBeVisible();
  await dialog.getByTestId("text-discard-cancel").click();
  await expect(page).toHaveURL(new RegExp(`/skills/${id}`));
  await expect(page.getByTestId("skill-description-input")).toHaveValue("直しかけの説明");

  // ブラウザの「戻る」でも確かめる（編集を続ければ同じ画面に残る）。
  await page.goBack();
  await expect(dialog).toBeVisible();
  await dialog.getByTestId("text-discard-cancel").click();
  await expect(page).toHaveURL(new RegExp(`/skills/${id}`));
  await expect(page.getByTestId("skill-description-input")).toHaveValue("直しかけの説明");

  // 破棄すれば移る。
  await page.locator('a[href="/skills"]').first().click();
  await dialog.getByTestId("text-discard-confirm").click();
  // 手元の next dev は開く画面をその場でコンパイルし直すことがあるので、移った先の URL は長めに待つ。
  await expect(page).toHaveURL(/\/skills$/, { timeout: 20_000 });
});

test("行頭の記号は、保存しても箇条書き・水平線・コードにならない", async ({ page }) => {
  await loginViaKeycloak(page);
  // 段落の行頭に記号を文字として置いた指示文（保存形式はエスケープ済み）。
  const instructions = "\\* 注意\n\n\\*\\*\\*\n\n\\`\\`\\`\n\n\\~~~\n";
  const id = await createSkill(page, uniqueName("lead-skill"), { instructions });
  await page.goto(`/skills/${id}`);
  const body = page.getByTestId("skill-body-editor");
  await expect(body).toContainText("* 注意");
  await focusBodyEnd(page);
  await page.keyboard.type("x");
  await page.getByTestId("skill-save-button").click();
  await expect(page.getByText("保存済み v2").first()).toBeVisible({ timeout: 15_000 });

  await page.reload();
  await expect(body).toContainText("* 注意");
  await expect(body).toContainText("***");
  await expect(body).toContainText("```");
  await expect(body.locator("ul, hr, pre")).toHaveCount(0);
});

test("使うモデルは公開されているモデルから選び、保存すると既定に入る", async ({ page }) => {
  await loginViaKeycloak(page);
  const models = (await (await page.request.get("/api/llm/models")).json()) as {
    models: { id: string; label: string }[];
  };
  test.skip(models.models.length === 0, "公開モデルが無い構成");
  const target = models.models[0];
  const id = await createSkill(page, uniqueName("model-skill"), {});
  await page.goto(`/skills/${id}`);
  await page.getByTestId("skill-settings-toggle").click();

  const select = page.getByTestId("skill-model-select");
  await expect(select).toContainText("指定しない");
  await select.click();
  await page.getByRole("option", { name: target.label }).first().click();
  await expect(select).toContainText(target.label);
  await expect(page.getByTestId("skill-model-hint")).toContainText("Auto のとき");

  await page.getByTestId("skill-save-button").click();
  // ボタンは保存中も無効になるので、保存が終わった印（版の表示）を待つ。
  await expect(page.getByText("保存済み v2").first()).toBeVisible();
  const body = (await (await page.request.get(`/api/skills/${id}`)).json()) as {
    body: { model: { model: string | null } | null };
  };
  expect(body.body.model?.model).toBe(target.id);

  // 「指定しない」に戻すとモデルの指定は外れる。
  await select.click();
  await page.getByRole("option", { name: "指定しない（システム既定）" }).click();
  await page.getByTestId("skill-save-button").click();
  await expect(page.getByText("保存済み v3").first()).toBeVisible();
  const after = (await (await page.request.get(`/api/skills/${id}`)).json()) as {
    body: { model: { model: string | null } | null };
  };
  expect(after.body.model?.model ?? null).toBeNull();
});

test("選べないモデルの指定も消さずに見せ、そのことを伝える", async ({ page }) => {
  await loginViaKeycloak(page);
  const id = await createSkill(page, uniqueName("retired-model"), {
    model: { model: "retired-model-x", temperature: 0.3 },
  });
  await page.goto(`/skills/${id}`);
  // 畳んでいても要約にモデルが出る。
  await expect(page.getByTestId("skill-settings-summary")).toContainText("モデル: retired-model-x");
  await page.getByTestId("skill-settings-toggle").click();
  await expect(page.getByTestId("skill-model-select")).toContainText("retired-model-x");
  await expect(page.getByTestId("skill-model-hint")).toContainText("選べるモデルにありません");
  // 値のある詳細パラメータは開いて見せる。
  await expect(page.locator("#skill-temperature")).toHaveValue("0.3");
});

test("アシスタントを開いて本文欄が細くなっても、詳細設定の見出しと値を縦に積む", async ({ page }) => {
  await page.setViewportSize({ width: 1100, height: 900 });
  await loginViaKeycloak(page);
  await page.goto("/skills/new");
  await expect(page.getByTestId("skill-assistant")).toBeVisible();
  await page.getByTestId("skill-settings-toggle").click();
  // ビューポートは広くても本文欄は細いので、値（ツールのピル）は見出しの下に来る。
  const label = page.getByRole("region", { name: "スキルの設定" }).getByText("使うツール", { exact: true });
  const tools = page.getByRole("group", { name: "使うツール" });
  const lb = await label.boundingBox();
  const tb = await tools.boundingBox();
  expect(lb && tb).toBeTruthy();
  expect(tb!.y).toBeGreaterThanOrEqual(lb!.y + lb!.height - 1);
  // ピルが値の列で潰れず、横に並ぶ（「すべて」と次のピルが同じ行）。
  const all = await page.getByTestId("skill-tools-all").boundingBox();
  const next = await page.getByRole("button", { name: "社内文書検索" }).boundingBox();
  expect(Math.abs(all!.y - next!.y)).toBeLessThan(2);
});
