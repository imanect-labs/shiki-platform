import { expect, test, type Page } from "@playwright/test";

import { loginViaKeycloak, uniqueName } from "./helpers";

/// ノートの ```ui ブロック（Yui・#530）を図として描く NodeView の受け入れ条件:
/// - ```ui + 空白（入力規則）で作ったブロックは、空のうちは記述（ソース）と書き方の一言を見せる
/// - 記述を書くと下に図を描き直し、カーソルが外れると図だけになる（「ソース」で戻せる）
/// - 図にできない記述は記述のまま、短いエラーの一行を出す（「図」には切り替えられない）
/// - 保存形式は language=ui の通常のコードブロックのまま（md 直列化・リロード後も同じ）
/// - 読み取り専用では図だけ（切り替えボタンは出さない）

async function createNoteViaApi(page: Page, name: string): Promise<string> {
  return page.evaluate(async (noteName) => {
    const csrf = document.cookie.match(/(?:^|;\s*)shiki_csrf=([^;]+)/)?.[1] ?? "";
    const res = await fetch("/api/notes", {
      method: "POST",
      credentials: "include",
      headers: { "Content-Type": "application/json", "X-CSRF-Token": csrf },
      body: JSON.stringify({ name: noteName, parent_id: null, markdown: null }),
    });
    if (!res.ok) throw new Error(`ノート作成に失敗: ${res.status}`);
    return ((await res.json()) as { id: string }).id;
  }, name);
}

async function openNote(page: Page, nodeId: string) {
  await page.goto(`/notes/${nodeId}`);
  await expect(page.getByTestId("note-sync-status")).toHaveText("同期済み", {
    timeout: 20_000,
  });
}

/// エディタの文書（ProseMirror JSON＝Yjs に載る構造）を取り出す。
async function editorJson(page: Page): Promise<string> {
  return page.evaluate(() => {
    type Ed = { getJSON: () => unknown };
    const ed = (window as unknown as { __shikiNoteEditor?: Ed }).__shikiNoteEditor;
    if (!ed) throw new Error("エディタが見つからない");
    return JSON.stringify(ed.getJSON());
  });
}

test("```ui ブロックを図として描き、ソースと切り替えられる", async ({ page }) => {
  await loginViaKeycloak(page);
  await page.goto("/drive");
  const nodeId = await createNoteViaApi(page, uniqueName("note-yui"));
  await openNote(page, nodeId);

  const editor = page.getByTestId("note-editor");
  await editor.click();
  // 入力規則: ```ui + 空白で language=ui のコードブロックになる。
  await editor.pressSequentially("```ui ");
  const block = editor.getByTestId("note-yui-block").first();
  await expect(block).toHaveAttribute("data-state", "empty");
  await expect(block.getByTestId("note-yui-hint")).toContainText("bar 月別売上");

  await editor.pressSequentially("bar 月別売上");
  await editor.press("Enter");
  await editor.pressSequentially("1月 | 120");
  await editor.press("Enter");
  await editor.pressSequentially("2月 | 150");
  await editor.press("Enter");
  await editor.pressSequentially("3月 | 90");
  // 編集中は記述を見せたまま、下に図を描き直す。
  await expect(block).toHaveAttribute("data-mode", "source");
  await expect(block).toHaveAttribute("data-state", "ok");
  await expect(block.locator("[data-yui] svg").first()).toBeVisible();

  // カーソルが外れると図だけになる（記述は隠れる）。
  await page.getByTestId("note-title-input").click();
  await expect(block).toHaveAttribute("data-mode", "figure");
  await expect(block.getByTestId("note-yui-source")).toBeHidden();
  await expect(block.getByTestId("ui-figure")).toHaveAttribute("data-ui-kind", "bar");

  // 「ソース」で記述に戻り、続きを書ける（カーソルは記述の末尾）。
  await block.getByRole("button", { name: "図の記述（ソース）を編集" }).click();
  await expect(block).toHaveAttribute("data-mode", "source");
  await expect(block.getByTestId("note-yui-source")).toBeVisible();
  await page.keyboard.press("Enter");
  await page.keyboard.type("4月 | 130");
  await expect(block.getByTestId("note-yui-source")).toContainText("4月 | 130");

  // キーボード: 記述から Tab で操作欄へ出ると図に戻り、フォーカスは「ソース」に移る。
  // Enter で記述へ戻り、カーソルは記述の末尾（そのまま書ける）。
  await page.keyboard.press("Tab");
  await expect(block).toHaveAttribute("data-mode", "figure");
  const toggle = block.getByRole("button", { name: "図の記述（ソース）を編集" });
  await expect(toggle).toBeFocused();
  await page.keyboard.press("Enter");
  await expect(block).toHaveAttribute("data-mode", "source");
  await page.keyboard.type("0");
  await expect(block.getByTestId("note-yui-source")).toContainText("4月 | 1300");
  await page.keyboard.press("Backspace");

  // 「図」ボタンで図に戻る。
  await block.getByTestId("note-yui-toggle").click();
  await expect(block).toHaveAttribute("data-mode", "figure");

  // 保存形式は通常のコードブロック（language=ui・本文は記述そのまま）。
  const json = JSON.parse(await editorJson(page)) as {
    content: { type: string; attrs?: { language?: string }; content?: { text: string }[] }[];
  };
  const code = json.content.find((n) => n.type === "codeBlock");
  expect(code?.attrs?.language).toBe("ui");
  expect(code?.content?.[0]?.text).toBe("bar 月別売上\n1月 | 120\n2月 | 150\n3月 | 90\n4月 | 130");

  // リロード後（Yjs から復元）も図として描く。
  await page.reload();
  await expect(page.getByTestId("note-sync-status")).toHaveText("同期済み", { timeout: 20_000 });
  const restored = page.getByTestId("note-yui-block").first();
  await expect(restored).toHaveAttribute("data-mode", "figure");
  await expect(restored.getByTestId("ui-figure")).toBeVisible();
});

test("図にできない ```ui は記述とエラーの一行を見せ、閲覧時は切り替えを出さない", async ({
  page,
}) => {
  await loginViaKeycloak(page);
  await page.goto("/drive");
  const nodeId = await createNoteViaApi(page, uniqueName("note-yui-error"));
  await openNote(page, nodeId);

  const editor = page.getByTestId("note-editor");
  await editor.click();
  await editor.pressSequentially("```ui ");
  await editor.pressSequentially("pie 円グラフ");
  await editor.press("Enter");
  await editor.pressSequentially("A | 1");
  const block = editor.getByTestId("note-yui-block").first();
  await expect(block).toHaveAttribute("data-state", "error");
  await expect(block.getByTestId("note-yui-error")).toContainText("図の種類が分かりません");
  await expect(block.getByTestId("note-yui-toggle")).toBeDisabled();

  // カーソルが外れても、図にできない間は記述のまま。
  await page.getByTestId("note-title-input").click();
  await expect(block).toHaveAttribute("data-mode", "source");
  await expect(block.getByTestId("note-yui-source")).toBeVisible();

  // 読み取り専用（閲覧者・印刷ビューと同じ editable=false）: 切り替えボタンは出さない。
  await page.evaluate(() => {
    type Ed = { setEditable: (v: boolean) => void };
    (window as unknown as { __shikiNoteEditor: Ed }).__shikiNoteEditor.setEditable(false);
  });
  await expect(block.getByTestId("note-yui-toggle")).toHaveCount(0);
  await expect(block.getByTestId("note-yui-hint")).toContainText("記述をそのまま表示しています");
});

test("素の Markdown で貼った ```ui bar は種類を保ったまま図になる", async ({ page }) => {
  await loginViaKeycloak(page);
  await page.goto("/drive");
  const nodeId = await createNoteViaApi(page, uniqueName("note-yui-paste"));
  await openNote(page, nodeId);

  // text/plain だけのペースト（MarkdownClipboard のブロック変換経路）。
  await page.evaluate((text) => {
    const el = document.querySelector<HTMLElement>('[data-testid="note-editor"]');
    if (!el) throw new Error("エディタ DOM が見つからない");
    el.focus();
    const dt = new DataTransfer();
    dt.setData("text/plain", text);
    el.dispatchEvent(
      new ClipboardEvent("paste", { clipboardData: dt, bubbles: true, cancelable: true }),
    );
  }, "前置き\n\n```ui bar\n月別売上\n1月 | 120\n2月 | 150\n```\n\n後書き\n");

  const block = page.getByTestId("note-yui-block").first();
  await expect(block).toHaveAttribute("data-state", "ok");
  // 情報文字列の種類（bar）は language=ui:bar として残る（1 語に切り詰めない）。
  const json = JSON.parse(await editorJson(page)) as {
    content: { type: string; attrs?: { language?: string } }[];
  };
  expect(json.content.find((n) => n.type === "codeBlock")?.attrs?.language).toBe("ui:bar");
  await page.getByTestId("note-title-input").click();
  await expect(block.getByTestId("ui-figure")).toHaveAttribute("data-ui-kind", "bar");
});

/// ノートを作って開き、```ui ブロック（bar・2 行）の前後に段落を置く（前 / 図 / 後）。
async function noteWithFigureBetween(page: Page, name: string) {
  await loginViaKeycloak(page);
  await page.goto("/drive");
  const nodeId = await createNoteViaApi(page, uniqueName(name));
  await openNote(page, nodeId);
  const editor = page.getByTestId("note-editor");
  await editor.click();
  await editor.pressSequentially("前");
  await editor.press("Enter");
  await editor.pressSequentially("```ui ");
  await editor.pressSequentially("bar 月別売上");
  await editor.press("Enter");
  await editor.pressSequentially("1月 | 120");
  // 記述の末尾で ↓ はブロックを出て次の段落（TrailingNode）へ。
  await editor.press("ArrowDown");
  await editor.pressSequentially("後");
  const block = editor.getByTestId("note-yui-block").first();
  await expect(block).toHaveAttribute("data-mode", "figure");
  return { editor, block };
}

type DocJson = {
  content: { type: string; attrs?: { language?: string }; content?: { text?: string }[] }[];
};

test("「図」をマウスで押して図に戻した後、ツールバーの操作で ```ui ブロックが変わらない", async ({
  page,
}) => {
  const { editor, block } = await noteWithFigureBetween(page, "note-yui-click");

  // 記述を開く（カーソルは記述の中）。
  await block.getByRole("button", { name: "図の記述（ソース）を編集" }).click();
  await expect(block).toHaveAttribute("data-mode", "source");

  // 実クリック（mousedown → mouseup）で「図」を押す。mousedown でフォーカスを奪うと
  // blur で操作欄が消え、クリックが届かず選択が隠した記述の中に残っていた。
  await block.getByTestId("note-yui-toggle").click();
  await expect(block).toHaveAttribute("data-mode", "figure");
  await expect(block.getByTestId("note-yui-source")).toBeHidden();
  // エディタのフォーカスは保ったまま（ボタンへ移さない）。
  await expect(editor).toBeFocused();

  // ツールバーの「見出し 1」「引用」は chain().focus() で選択を戻す。ブロックの外の選択に効く。
  const toolbar = page.getByTestId("note-toolbar");
  await toolbar.getByRole("button", { name: "見出し 1" }).click();
  await toolbar.getByRole("button", { name: "引用" }).click();

  const json = JSON.parse(await editorJson(page)) as DocJson;
  const code = json.content.find((n) => n.type === "codeBlock");
  expect(code?.attrs?.language).toBe("ui");
  expect(code?.content?.[0]?.text).toBe("bar 月別売上\n1月 | 120");
  expect(json.content.some((n) => n.type === "heading" || n.type === "blockquote")).toBe(true);
  await expect(block).toHaveAttribute("data-mode", "figure");
  await expect(block.getByTestId("ui-figure")).toBeVisible();
});

test("記述からフォーカスが外れて図に戻っても、ツールバーは隠した記述を変えない", async ({ page }) => {
  const { block } = await noteWithFigureBetween(page, "note-yui-blur");
  await block.getByRole("button", { name: "図の記述（ソース）を編集" }).click();
  await expect(block).toHaveAttribute("data-mode", "source");

  // エディタの外（題名）へフォーカスを移す → 図に戻る。選択もブロックの外へ移る。
  await page.getByTestId("note-title-input").click();
  await expect(block).toHaveAttribute("data-mode", "figure");
  await page.getByTestId("note-toolbar").getByRole("button", { name: "見出し 1" }).click();

  const json = JSON.parse(await editorJson(page)) as DocJson;
  expect(json.content.find((n) => n.type === "codeBlock")?.attrs?.language).toBe("ui");
  await expect(block).toHaveAttribute("data-mode", "figure");
});

test("矢印キーで図の表示中の ```ui ブロックへ入り、記述を開ける（キーボードのみ）", async ({
  page,
}) => {
  const { editor, block } = await noteWithFigureBetween(page, "note-yui-arrows");
  const source = block.getByTestId("note-yui-source");

  // 後 の先頭から ↑: 記述の末尾へ入る（記述が開く）。
  await editor.getByText("後", { exact: true }).click();
  await page.keyboard.press("Home");
  await page.keyboard.press("ArrowUp");
  await expect(block).toHaveAttribute("data-mode", "source");
  await page.keyboard.type("0");
  await expect(source).toContainText("1月 | 1200");
  await page.keyboard.press("Backspace");

  // 記述の末尾から ↓ で出ると図に戻る。そこから ← で末尾へ入り直す。
  await page.keyboard.press("ArrowDown");
  await expect(block).toHaveAttribute("data-mode", "figure");
  await page.keyboard.press("Home");
  await page.keyboard.press("ArrowLeft");
  await expect(block).toHaveAttribute("data-mode", "source");
  await page.keyboard.type("5");
  await expect(source).toContainText("1月 | 1205");
  await page.keyboard.press("Backspace");

  // 前 の末尾から ↓: 記述の先頭へ入る。
  await editor.getByText("前", { exact: true }).click();
  await expect(block).toHaveAttribute("data-mode", "figure");
  await page.keyboard.press("End");
  await page.keyboard.press("ArrowDown");
  await expect(block).toHaveAttribute("data-mode", "source");
  await page.keyboard.type("x");
  await expect(source).toContainText("xbar 月別売上");
  await page.keyboard.press("Backspace");

  // 前 の末尾から →: 記述の先頭へ入る。
  await editor.getByText("前", { exact: true }).click();
  await expect(block).toHaveAttribute("data-mode", "figure");
  await page.keyboard.press("End");
  await page.keyboard.press("ArrowRight");
  await expect(block).toHaveAttribute("data-mode", "source");
  await page.keyboard.type("y");
  await expect(source).toContainText("ybar 月別売上");
  await page.keyboard.press("Backspace");

  // 文書は変わっていない（入力は打ち消した）。
  const json = JSON.parse(await editorJson(page)) as DocJson;
  const code = json.content.find((n) => n.type === "codeBlock");
  expect(code?.attrs?.language).toBe("ui");
  expect(code?.content?.[0]?.text).toBe("bar 月別売上\n1月 | 120");
});

test("種類付きの言語は分かる種類だけ ui:<種類>（小文字）に寄せ、HTML 往復でも保つ", async ({
  page,
}) => {
  await loginViaKeycloak(page);
  await page.goto("/drive");
  const nodeId = await createNoteViaApi(page, uniqueName("note-yui-lang"));
  await openNote(page, nodeId);

  const paste = (data: Record<string, string>) =>
    page.evaluate((entries) => {
      const el = document.querySelector<HTMLElement>('[data-testid="note-editor"]');
      if (!el) throw new Error("エディタ DOM が見つからない");
      el.focus();
      const dt = new DataTransfer();
      for (const [type, value] of Object.entries(entries)) dt.setData(type, value);
      el.dispatchEvent(
        new ClipboardEvent("paste", { clipboardData: dt, bubbles: true, cancelable: true }),
      );
    }, data);
  const languages = async () =>
    (JSON.parse(await editorJson(page)) as DocJson).content
      .filter((n) => n.type === "codeBlock")
      .map((n) => n.attrs?.language);

  // 素の Markdown: `UI Bar` → ui:bar。種類として分からない `ui a```b` は 1 語目だけ（寄せない）。
  await paste({
    "text/plain": "前置き\n\n```UI Bar\n月別売上\n1月 | 120\n```\n\n~~~ui a```b\nx\n~~~\n\n後書き\n",
  });
  await expect.poll(languages).toEqual(["ui:bar", "ui"]);

  // HTML: collab（md_parse）由来の `class="language-ui bar"` は ui:bar として受ける
  // （前後の段落ごとコピーした範囲。ブロック 1 つだけだと既定で今の段落へ文字として入る）。
  await paste({
    "text/html":
      '<p>上</p><pre><code class="language-ui bar">売上\n1月 | 10\n2月 | 20</code></pre><p>下</p>',
    "text/plain": "上\n\n売上\n1月 | 10\n2月 | 20\n\n下",
  });
  await expect.poll(languages).toEqual(["ui:bar", "ui", "ui:bar"]);

  // 書き出す HTML（コピー）: language=`ui bar` のブロックも class 1 つ（language-ui:bar）にする。
  const html = await page.evaluate(() => {
    type Ed = { getHTML: () => string; commands: { insertContent: (c: unknown) => boolean } };
    const ed = (window as unknown as { __shikiNoteEditor: Ed }).__shikiNoteEditor;
    ed.commands.insertContent({
      type: "codeBlock",
      attrs: { language: "ui bar" },
      content: [{ type: "text", text: "売上\nA | 1" }],
    });
    return ed.getHTML();
  });
  expect(html).toContain('class="language-ui:bar"');
  expect(html).not.toContain('class="language-ui bar"');
});
