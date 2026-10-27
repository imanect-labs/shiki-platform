import { expect, test, type Browser, type Locator, type Page } from "@playwright/test";

import { loginViaKeycloak, uniqueName } from "./helpers";

/// 「AI に聞く」の設定フォームの思考の深さと出力の形（llm.invoke の effort・output_schema・GAP-13 の T3）を、
/// エディタの画面で確かめる。
///
/// - 出力の形を項目の表（名前・種類・必須・選択肢・説明）で組み、並べ替え、保存して開き直しても同じ形と順で出る
///   （保存される形は `x-order` 付きの閉じたオブジェクト）。項目名の重なりはその場で知らせ、直すまで書き戻さない。
/// - JSON で書いた形の誤りは、保存時の検証の位置（`/params/output_schema/...`）から欄の下に出る。
/// - 後ろのブロックの「差し込む」の候補に、出力の形の項目（`data.<項目>`）が出る。形を外すと data も出ない。
/// - 思考の深さは保存して開き直しても残る（stub のモデルは対応しないので、送られない旨を欄の下に出す）。
/// 前提: compose の shiki-server が SHIKI__WORKFLOW__ENABLED=true・LLM は stub。モデルは stub（default）を明示し、
/// 実行はしない（実 LLM を呼ばない）。作ったワークフローは最後に消す。

const STUB_MODEL = "default";

async function csrfHeader(page: Page): Promise<Record<string, string>> {
  const csrf = (await page.context().cookies()).find((c) => c.name === "shiki_csrf")?.value ?? "";
  return { "x-csrf-token": csrf, "content-type": "application/json" };
}

async function createFlow(page: Page): Promise<string> {
  const name = uniqueName("wf-llm-form");
  const res = await page.request.post("/api/workflows", {
    headers: await csrfHeader(page),
    data: {
      ir: {
        ir_version: 1,
        name,
        display_name: name,
        triggers: [{ kind: "interactive" }],
        input_schema: { type: "object", properties: { body: { type: "string" } }, required: ["body"] },
        nodes: [
          {
            id: "triage",
            type: "llm.invoke",
            label: "種類を判定",
            params: { model: STUB_MODEL, prompt: { $from: "input", path: "/body" }, effort: "low" },
          },
          {
            id: "note",
            type: "llm.invoke",
            label: "結果を整える",
            params: { model: STUB_MODEL, prompt: "判定の結果を短く書き出してください" },
          },
        ],
        edges: [{ from: "triage", from_port: "out", to: "note" }],
      },
    },
  });
  expect(res.ok(), await res.text()).toBeTruthy();
  return ((await res.json()) as { id: string }).id;
}

async function cleanup(page: Page, ids: string[]) {
  const headers = await csrfHeader(page);
  for (const id of ids) {
    const res = await page.request.delete(`/api/artifacts/${id}`, { headers });
    expect([204, 404], `DELETE /artifacts/${id} → ${res.status()}`).toContain(res.status());
  }
}

type Ir = { nodes: { id: string; params: Record<string, unknown> }[] };

async function savedParams(page: Page, id: string, node: string): Promise<Record<string, unknown>> {
  const res = await page.request.get(`/api/workflows/${id}`);
  expect(res.ok(), await res.text()).toBeTruthy();
  const body = (await res.json()) as { ir: Ir };
  return body.ir.nodes.find((n) => n.id === node)!.params;
}

/// キャンバスの表示位置（fitView のアニメーション）が落ち着くまで待つ（workflows-editor.spec と同じ）。
async function settleViewport(page: Page) {
  const read = () => page.locator(".react-flow__viewport").evaluate((el) => (el as HTMLElement).style.transform);
  let prev = await read();
  await expect
    .poll(
      async () => {
        const cur = await read();
        const still = cur === prev;
        prev = cur;
        return still;
      },
      { intervals: [200] },
    )
    .toBe(true);
}

async function openNode(page: Page, id: string, nodeId: string): Promise<Locator> {
  await page.goto(`/workflows/${id}`);
  const node = page.getByTestId(`rf__node-${nodeId}`);
  await expect(node).toBeVisible({ timeout: 15_000 });
  await settleViewport(page);
  await node.click();
  const panel = page.getByRole("complementary", { name: "ブロックの設定" });
  await expect(panel).toBeVisible();
  return panel;
}

/// Radix の選択の欄で選ぶ（開いた一覧は panel の外に出る）。
async function choose(page: Page, trigger: Locator, option: string) {
  await trigger.click();
  await page.getByRole("option", { name: option, exact: true }).click();
}

test("AI に聞く: 出力の形を表で組んで保存し、開き直しても同じ形と順で出る（思考の深さも残る）", async ({ page }) => {
  test.setTimeout(180_000);
  await loginViaKeycloak(page);
  await page.setViewportSize({ width: 1440, height: 1000 });
  const id = await createFlow(page);
  try {
    const panel = await openNode(page, id, "triage");

    // 思考の深さ: 保存済みの low が出る。stub のモデルは思考に対応しないので、送られない旨を出す。
    const effort = panel.getByRole("combobox", { name: "思考の深さ（省略可）" });
    await expect(effort).toHaveText("速い");
    await expect(panel.getByText("このモデルは思考の深さに対応しないため")).toBeVisible();
    await choose(page, effort, "じっくり");
    await expect(effort).toHaveText("じっくり");

    // 出力の形: 3 項目（選択肢・文字の一覧・任意の文字列）を組む。キーボードで続けて書けるよう、足した行の
    // 項目名の欄にフォーカスが移る。
    const editor = panel.getByRole("group", { name: "出力の形（省略可）" });
    await editor.getByRole("button", { name: "出力の項目を追加" }).click();
    const row1 = editor.getByRole("group", { name: "出力の項目 1" });
    await expect(row1.getByRole("textbox", { name: "項目名" })).toBeFocused();
    await row1.getByRole("textbox", { name: "項目名" }).fill("category");
    await choose(page, row1.getByRole("combobox", { name: "種類" }), "選択肢");
    await row1.getByRole("textbox", { name: "選択肢" }).fill("billing\nbug\nhowto");
    await row1.getByRole("textbox", { name: "説明" }).fill("問い合わせの種類");

    await editor.getByRole("button", { name: "出力の項目を追加" }).click();
    const row2 = editor.getByRole("group", { name: "出力の項目 2" });
    await row2.getByRole("textbox", { name: "項目名" }).fill("summary");
    await row2.getByRole("switch", { name: "必須" }).click();

    await editor.getByRole("button", { name: "出力の項目を追加" }).click();
    const row3 = editor.getByRole("group", { name: "出力の項目 3" });
    await row3.getByRole("textbox", { name: "項目名" }).fill("tags");
    await choose(page, row3.getByRole("combobox", { name: "種類" }), "文字の一覧");

    // 項目名の重なりはその場で知らせ、直すまで書き戻さない。
    await row3.getByRole("textbox", { name: "項目名" }).fill("category");
    await expect(row3.getByText("項目名「category」が重なっています")).toBeVisible();
    await expect(editor.getByText("直すまで出力の形の変更は保存されません")).toBeVisible();
    await row3.getByRole("textbox", { name: "項目名" }).fill("tags");
    await expect(editor.getByText("直すまで出力の形の変更は保存されません")).toHaveCount(0);

    // 並べ替え（キーボード）: tags を上へ 1 つ → category, tags, summary。押したボタンにフォーカスが付いて行く。
    const up = editor.getByRole("button", { name: "出力の項目「tags」を上へ" });
    await up.focus();
    await page.keyboard.press("Enter");
    await expect(editor.getByRole("button", { name: "出力の項目「tags」を上へ" })).toBeFocused();
    await page.screenshot({ path: "test-results/llm-form-output-schema-pc.png", fullPage: false });

    await page.getByRole("button", { name: "保存", exact: true }).click();
    await expect(page.getByText(/保存しました/).first()).toBeVisible({ timeout: 15_000 });

    // 保存された形: 閉じたオブジェクト（additionalProperties は書かない）・必須・並び（x-order）・説明。
    const params = await savedParams(page, id, "triage");
    expect(params.effort).toBe("high");
    expect(params.output_schema).toEqual({
      type: "object",
      properties: {
        category: { type: "string", enum: ["billing", "bug", "howto"], description: "問い合わせの種類" },
        tags: { type: "array", items: { type: "string" } },
        summary: { type: "string" },
      },
      required: ["category", "tags"],
      "x-order": ["category", "tags", "summary"],
    });

    // 開き直しても同じ順・同じ値で出る。
    const again = await openNode(page, id, "triage");
    const reopened = again.getByRole("group", { name: "出力の形（省略可）" });
    await expect
      .poll(() =>
        reopened
          .getByRole("textbox", { name: "項目名" })
          .evaluateAll((els) => els.map((e) => (e as HTMLInputElement).value)),
      )
      .toEqual(["category", "tags", "summary"]);
    await expect(reopened.getByRole("group", { name: "出力の項目 1" }).getByRole("textbox", { name: "選択肢" })).toHaveValue(
      "billing\nbug\nhowto",
    );
    await expect(reopened.getByRole("group", { name: "出力の項目 3" }).getByRole("switch", { name: "必須" })).not.toBeChecked();
    await expect(again.getByRole("combobox", { name: "思考の深さ（省略可）" })).toHaveText("じっくり");

    // 後ろのブロックの「差し込む」に、出力の形の項目が出る。
    await page.getByTestId("rf__node-note").click();
    const notePanel = page.getByRole("complementary", { name: "ブロックの設定" });
    await notePanel
      .locator('[data-value-expr="AI への指示"]')
      .getByRole("button", { name: "差し込む" })
      .click();
    for (const label of ["data", "data.category", "data.tags", "data.summary"]) {
      await expect(page.getByRole("menuitem", { name: `「種類を判定」の ${label}`, exact: true })).toBeVisible();
    }
    await page.keyboard.press("Escape");
  } finally {
    await cleanup(page, [id]);
  }
});

test("AI に聞く: JSON で書いた出力の形の誤りが欄の下に出て、直すと消える。形を外すと差し込みの data も消える", async ({
  page,
}) => {
  test.setTimeout(180_000);
  await loginViaKeycloak(page);
  await page.setViewportSize({ width: 1440, height: 1000 });
  const id = await createFlow(page);
  try {
    const panel = await openNode(page, id, "triage");
    const editor = panel.getByRole("group", { name: "出力の形（省略可）" });
    await editor.getByRole("button", { name: "JSON で編集" }).click();
    const json = editor.getByRole("textbox", { name: "出力の形（JSON Schema）" });
    // 型の組（[string, integer]）は書けない（[型, "null"] だけ）。表では扱えない形なので JSON のまま。
    await json.fill('{"type":"object","properties":{"category":{"type":["string","integer"]}}}');
    const errors = editor.getByTestId("output-schema-errors");
    await expect(errors).toContainText("「category」: type は 1 つの型", { timeout: 10_000 });
    await expect(editor.getByText("表では編集できません")).toBeVisible();

    // 保存しようとしても止まり、誤りは同じ欄に出たまま。
    await page.getByRole("button", { name: "保存", exact: true }).click();
    await expect(page.getByText("保存できません（検証エラー）").first()).toBeVisible({ timeout: 15_000 });
    await expect(errors).toContainText("「category」");

    // 直すと誤りが消え、表で書ける形なら表に戻せる。
    await json.fill('{"type":"object","properties":{"category":{"type":"string"}},"required":["category"]}');
    await expect(errors).toHaveCount(0, { timeout: 10_000 });
    await editor.getByRole("button", { name: "表で編集" }).click();
    await expect(editor.getByRole("group", { name: "出力の項目 1" }).getByRole("textbox", { name: "項目名" })).toHaveValue(
      "category",
    );

    // 項目を消すと形も外れ、後ろのブロックの「差し込む」から data が消える（形の無い出力に data は無い）。
    await editor.getByRole("button", { name: "出力の項目「category」を削除" }).click();
    await page.getByTestId("rf__node-note").click();
    const notePanel = page.getByRole("complementary", { name: "ブロックの設定" });
    await notePanel
      .locator('[data-value-expr="AI への指示"]')
      .getByRole("button", { name: "差し込む" })
      .click();
    await expect(page.getByRole("menuitem", { name: "「種類を判定」の text", exact: true })).toBeVisible();
    await expect(page.getByRole("menuitem", { name: "「種類を判定」の data", exact: true })).toHaveCount(0);
    await page.keyboard.press("Escape");
  } finally {
    await cleanup(page, [id]);
  }
});

/// スマホ幅（設定は下からのシート）でも出力の形を組め、横にはみ出さない。
test("AI に聞く: スマホのシートでも出力の形を組める", async ({ browser, page }) => {
  test.setTimeout(180_000);
  const phone = await phonePage(browser, page);
  const id = await createFlow(page);
  try {
    await phone.goto(`/workflows/${id}`);
    const flow = phone.getByTestId("workflow-mobile-flow");
    await expect(flow).toBeVisible({ timeout: 15_000 });
    await flow.getByRole("button", { name: /種類を判定/ }).filter({ hasText: "種類を判定" }).click();
    const sheet = phone.getByRole("dialog");
    await expect(sheet).toBeVisible();
    const editor = sheet.getByRole("group", { name: "出力の形（省略可）" });
    await editor.getByRole("button", { name: "出力の項目を追加" }).scrollIntoViewIfNeeded();
    await editor.getByRole("button", { name: "出力の項目を追加" }).click();
    const row = editor.getByRole("group", { name: "出力の項目 1" });
    await row.getByRole("textbox", { name: "項目名" }).fill("category");
    await choose(phone, row.getByRole("combobox", { name: "種類" }), "選択肢");
    await row.getByRole("textbox", { name: "選択肢" }).fill("billing\nbug");
    await row.scrollIntoViewIfNeeded();
    expect(await sheet.evaluate((el) => el.scrollWidth <= el.clientWidth + 1)).toBe(true);
    expect(await phone.evaluate(() => document.documentElement.scrollWidth <= window.innerWidth + 1)).toBe(true);
    await phone.screenshot({ path: "test-results/llm-form-output-schema-phone.png" });
  } finally {
    await cleanup(page, [id]);
    await phone.context().close();
  }
});

/// ログインは PC の画面で済ませ、そのセッションをタッチ端末の文脈へ持ち込む（workflow-model-default.spec と同じ）。
async function phonePage(browser: Browser, page: Page): Promise<Page> {
  await loginViaKeycloak(page);
  const ctx = await browser.newContext({
    storageState: await page.context().storageState(),
    viewport: { width: 390, height: 844 },
    hasTouch: true,
    isMobile: true,
    deviceScaleFactor: 3,
    ignoreHTTPSErrors: true,
  });
  return ctx.newPage();
}
