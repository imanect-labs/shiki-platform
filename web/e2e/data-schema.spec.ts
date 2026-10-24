import { expect, test, type Page } from "@playwright/test";

import { loginViaKeycloak, uniqueName } from "./helpers";

/// 列の編集（A-5）と参照の値の入力（A-6）: 全型の列の追加・選択肢の編集・列の削除・未保存の確認、
/// レコードの追加でユーザー・リンクを候補から選ぶ、グリッドのリンクのセルを候補から選び直す。

const ROW_MARKER = 32;
const COL_WIDTH = 160;
const HEADER = 36;
const ROW = 34;

async function csrf(page: Page): Promise<string> {
  return (await page.context().cookies()).find((c) => c.name === "shiki_csrf")?.value ?? "";
}

async function createTable(page: Page, name: string, fields: unknown[], rows: Record<string, unknown>[] = []) {
  const headers = { "X-CSRF-Token": await csrf(page) };
  const res = await page.request.post("/api/data/tables", { headers, data: { name, schema: { fields } } });
  expect(res.status(), await res.text()).toBe(201);
  const id = ((await res.json()) as { id: string }).id;
  for (const data of rows) {
    const r = await page.request.post(`/api/data/tables/${id}/records`, { headers, data: { data } });
    expect(r.status(), await r.text()).toBe(201);
  }
  return id;
}

async function schemaOf(page: Page, id: string) {
  const t = (await (await page.request.get(`/api/data/tables/${id}`)).json()) as {
    schema: { fields: { key: string; display_name: string; type: string; options?: string[]; formula?: unknown; ref_table?: string }[] };
  };
  return t.schema.fields;
}

test("列の編集: リンク・数式の追加、選択肢の追加、足した列の取り消しを保存し、未保存の変更は閉じる前に確かめる（A-5）", async ({ page }) => {
  await loginViaKeycloak(page);
  const refName = uniqueName("e2e-参照先");
  await createTable(page, refName, [{ key: "title", display_name: "題名", type: "text", indexed: true }]);
  const id = await createTable(page, uniqueName("e2e-列"), [
    { key: "name", display_name: "件名", type: "text", indexed: true },
    { key: "qty", display_name: "数量", type: "number" },
    { key: "st", display_name: "状態", type: "select", options: ["A"] },
    { key: "memo", display_name: "メモ", type: "text" },
  ]);
  await page.goto(`/data/${id}`);
  await expect(page.getByTestId("data-grid")).toBeVisible();
  await page.getByTestId("schema-editor-open").click();
  const editor = page.getByTestId("schema-editor");

  // 1. リンクの列（参照先・つながり方）。
  await page.getByTestId("schema-add-field").click();
  await page.getByLabel("新しい列の表示名").fill("商品");
  await page.getByLabel("新しい列の型").click();
  await page.getByRole("option", { name: "リンク", exact: true }).click();
  await page.getByLabel("参照先のテーブル").click();
  await page.getByRole("option", { name: refName }).click();
  await page.getByTestId("schema-add-confirm").click();

  // 2. 数式の列（書き間違いはその場で知らせ、追加できない）。
  await page.getByTestId("schema-add-field").click();
  await page.getByLabel("新しい列の表示名").fill("二倍");
  await page.getByLabel("新しい列の型").click();
  await page.getByRole("option", { name: "数式", exact: true }).click();
  await page.locator("#schema-formula").fill("「数量」 * ");
  await expect(page.getByTestId("schema-add-confirm")).toBeDisabled();
  await page.locator("#schema-formula").fill("「数量」 * 2");
  await page.getByTestId("schema-add-confirm").click();

  // 3. 既存の列の選択肢を足す。保存済みの列は削除できない（追加のみの改訂）ので削除の操作は出さず、
  //    足したばかりの列は取り消せる。
  await page.getByRole("button", { name: "「状態」の設定" }).click();
  await page.getByLabel("選択肢を追加").fill("B, C");
  await page.keyboard.press("Enter");
  await expect(page.getByRole("button", { name: "「メモ」の追加を取り消す" })).toHaveCount(0);
  await page.getByTestId("schema-add-field").click();
  await page.getByLabel("新しい列の表示名").fill("取り消す列");
  await page.getByTestId("schema-add-confirm").click();
  await page.getByRole("button", { name: "「取り消す列」の追加を取り消す" }).click();

  await page.getByTestId("schema-save").click();
  await expect(editor).toBeHidden();
  const fields = await schemaOf(page, id);
  expect(fields.find((f) => f.key === "memo")).toBeDefined();
  expect(fields.find((f) => f.display_name === "取り消す列")).toBeUndefined();
  expect(fields.find((f) => f.key === "st")?.options).toEqual(["A", "B", "C"]);
  expect(fields.find((f) => f.display_name === "商品")?.type).toBe("record_ref");
  expect(fields.find((f) => f.display_name === "二倍")?.formula).toEqual({ mul: [{ field: "qty" }, { lit: 2 }] });

  // 4. 未保存の変更があるまま閉じようとすると確かめる（「キャンセル」で編集に戻れる）。
  await page.getByTestId("schema-editor-open").click();
  await page.getByLabel("「name」の表示名").fill("件名を変えた");
  await page.keyboard.press("Escape");
  await expect(page.getByText("保存していない変更があります")).toBeVisible();
  await page.getByRole("button", { name: "キャンセル" }).click();
  await expect(page.getByText("保存していない変更があります")).toBeHidden();
  await expect(page.getByLabel("「name」の表示名")).toHaveValue("件名を変えた");
  await page.keyboard.press("Escape");
  await page.getByRole("button", { name: "変更を捨てて閉じる" }).click();
  await expect(editor).toBeHidden();
  expect((await schemaOf(page, id)).find((f) => f.key === "name")?.display_name).toBe("件名");
});

test("参照の値: レコードの追加でユーザーとリンクを候補から選び、グリッドでリンクを選び直す（A-6）", async ({ page }) => {
  await loginViaKeycloak(page);
  const ref = await createTable(page, uniqueName("e2e-果物"), [{ key: "title", display_name: "題名", type: "text", indexed: true }], [
    { title: "りんご" },
    { title: "みかん" },
  ]);
  const id = await createTable(page, uniqueName("e2e-参照"), [
    { key: "name", display_name: "件名", type: "text", indexed: true },
    { key: "who", display_name: "担当", type: "user_ref" },
    { key: "item", display_name: "商品", type: "record_ref", ref_table: ref, link: { cardinality: "many_to_one" } },
  ]);
  await page.goto(`/data/${id}`);
  await page.getByTestId("add-record").click();
  const dialog = page.getByTestId("record-dialog");
  await dialog.getByLabel("件名").fill("参照の行");
  await dialog.locator("#rec-who").fill("bob");
  await dialog.getByRole("option", { name: /bob@a-corp/ }).click();
  await dialog.locator("#rec-item").click();
  await dialog.getByRole("option", { name: "みかん" }).click();
  const created = page.waitForResponse((r) => r.url().includes(`/data/tables/${id}/records`) && r.request().method() === "POST");
  await page.getByTestId("record-dialog-submit").click();
  const body = (await (await created).json()) as { id: string; data: { who: string; item: string[] } };
  await expect(dialog).toBeHidden();
  expect(body.data.who).toBe("00000000-0000-0000-0000-000000000002");
  expect(body.data.item).toHaveLength(1);

  // グリッドのリンクのセル: Enter で候補を開き、別の行を選ぶと保存する。
  const grid = page.getByTestId("data-grid");
  await expect(grid).toHaveAttribute("data-loaded", "1");
  const box = (await grid.boundingBox())!;
  await page.mouse.click(box.x + ROW_MARKER + 2 * COL_WIDTH + COL_WIDTH / 2, box.y + HEADER + ROW / 2);
  await page.keyboard.press("Enter");
  const picker = page.getByTestId("ref-cell-editor");
  await expect(picker).toBeVisible();
  const patched = page.waitForResponse((r) => r.url().includes(`/data/tables/${id}/records/`) && r.request().method() === "PATCH");
  await picker.getByRole("option", { name: "りんご" }).click();
  const res = await patched;
  expect(res.status()).toBe(200);
  const saved = (await res.json()) as { data: { item: string[] } };
  expect(saved.data.item).toHaveLength(1);
  expect(saved.data.item[0]).not.toBe(body.data.item[0]);
});

test("日本語名のリンクを 2 本足しても逆向きの列の参照名が重ならず、読める表示名になる（2-A-P2-1）", async ({ page }) => {
  await loginViaKeycloak(page);
  const refName = uniqueName("e2e-取引先");
  const ref = await createTable(page, refName, [{ key: "title", display_name: "社名", type: "text", indexed: true }]);
  const srcName = uniqueName("e2e-請求");
  const id = await createTable(page, srcName, [{ key: "name", display_name: "件名", type: "text", indexed: true }]);
  await page.goto(`/data/${id}`);
  await expect(page.getByTestId("data-grid")).toBeVisible();
  await page.getByTestId("schema-editor-open").click();
  const editor = page.getByTestId("schema-editor");
  for (const [label, oneToOne] of [["取引先", false], ["請求先", true]] as const) {
    await page.getByTestId("schema-add-field").click();
    const form = editor.getByTestId("schema-add-form");
    await form.getByLabel("新しい列の表示名").fill(label);
    await form.getByLabel("新しい列の型").click();
    await page.getByRole("option", { name: "リンク", exact: true }).click();
    await form.getByLabel("参照先のテーブル").click();
    await page.getByRole("option", { name: refName }).click();
    const symmetric = form.getByRole("switch", { name: "参照先にも逆向きの列を作る" });
    if (oneToOne) {
      // 一対一は逆向きの列が必須（オンのまま変えられない）。
      await form.getByLabel("つながり方").click();
      await page.getByRole("option", { name: /一対一/ }).click();
      await expect(symmetric).toBeChecked();
      await expect(symmetric).toBeDisabled();
    } else {
      await symmetric.click();
    }
    await form.getByTestId("schema-add-confirm").click();
  }
  // 逆向きの列の表示名は作成時に渡す（`symmetric_display_name`）。保存の後に参照先のスキーマを書き換えない。
  const refSchemaWrites: string[] = [];
  page.on("request", (r) => {
    if (r.method() === "PUT" && r.url().includes(`/data/tables/${ref}/schema`)) refSchemaWrites.push(r.url());
  });
  const saved = page.waitForResponse((r) => r.url().includes(`/data/tables/${id}/schema`) && r.request().method() === "PUT");
  await page.getByTestId("schema-save").click();
  const body = (await (await saved).request().postDataJSON()) as {
    schema: { fields: { link?: { symmetric_display_name?: string } }[] };
  };
  expect(body.schema.fields.flatMap((f) => (f.link?.symmetric_display_name ? [f.link.symmetric_display_name] : [])).sort()).toEqual(
    [`${srcName}（取引先）`, `${srcName}（請求先）`].sort(),
  );
  await expect(editor).toBeHidden();
  const reverse = (await schemaOf(page, ref)).filter((f) => f.type === "record_ref");
  expect(reverse).toHaveLength(2);
  expect(new Set(reverse.map((f) => f.key)).size).toBe(2);
  expect(reverse.map((f) => f.key)).not.toContain("rev");
  expect(reverse.map((f) => f.display_name).sort()).toEqual([`${srcName}（取引先）`, `${srcName}（請求先）`].sort());
  expect(refSchemaWrites).toEqual([]);
});

test("今のデータと合わない設定（値の無い行がある列の必須化）は、保存できない理由をその列の下に出す", async ({ page }) => {
  await loginViaKeycloak(page);
  const id = await createTable(
    page,
    uniqueName("e2e-必須化"),
    [
      { key: "name", display_name: "件名", type: "text", indexed: true },
      { key: "memo", display_name: "備考", type: "text" },
    ],
    [{ name: "a" }, { name: "b", memo: "あり" }],
  );
  await page.goto(`/data/${id}`);
  await page.getByTestId("schema-editor-open").click();
  const editor = page.getByTestId("schema-editor");
  await editor.getByRole("button", { name: "「備考」の設定" }).click();
  await editor.getByTestId("schema-settings-memo").getByRole("switch", { name: "必須" }).click();
  const saved = page.waitForResponse((r) => r.url().includes(`/data/tables/${id}/schema`) && r.request().method() === "PUT");
  await page.getByTestId("schema-save").click();
  const res = await saved;
  expect(res.status()).toBe(400);
  expect(((await res.json()) as { code?: string }).code).toBe("schema_conflicts_with_data");
  // サーバの文言（列の表示名と件数）を、その列の下に出す。シートは閉じない。
  const note = editor.getByTestId("schema-field-memo").getByTestId("schema-data-conflict");
  await expect(note).toBeVisible();
  await expect(note).toContainText("備考");
  await expect(note).toContainText("1 件");
  await expect(editor).toBeVisible();
  // 設定を戻すと理由は消え、保存できる。
  await editor.getByTestId("schema-settings-memo").getByRole("switch", { name: "必須" }).click();
  await expect(note).toBeHidden();
});

test("行ポリシーのあるテーブルの一意・マスク列の必須には注意を出し、表示名は空にできない（2-B-P2-3・2-B-P3-5）", async ({ page }) => {
  await loginViaKeycloak(page);
  const headers = { "X-CSRF-Token": await csrf(page) };
  const res = await page.request.post("/api/data/tables", {
    headers,
    data: {
      name: uniqueName("e2e-行ポリシー"),
      schema: {
        fields: [
          { key: "code", display_name: "コード", type: "text", indexed: true },
          { key: "memo", display_name: "社内メモ", type: "text" },
        ],
        row_policy: { read: { any: ["is_owner", "public"] } },
        field_policy: [{ field: "memo", readable_by: { has_role: { role: "a-corp/e2e-nobody", subtree: false } } }],
      },
    },
  });
  expect(res.status(), await res.text()).toBe(201);
  const id = ((await res.json()) as { id: string }).id;
  await page.goto(`/data/${id}`);
  await page.getByTestId("schema-editor-open").click();
  const editor = page.getByTestId("schema-editor");
  await editor.getByRole("button", { name: "「コード」の設定" }).click();
  await editor.getByRole("switch", { name: "重複を許さない（一意）" }).click();
  await expect(editor.getByTestId("schema-warning-unique-row-policy")).toBeVisible();
  await editor.getByRole("button", { name: "「社内メモ」の設定" }).click();
  await editor.getByTestId("schema-settings-memo").getByRole("switch", { name: "必須" }).click();
  await expect(editor.getByTestId("schema-warning-required-masked")).toBeVisible();
  await editor.getByLabel("「code」の表示名").fill("");
  await expect(editor.getByText("表示名を入れてください（空にはできません）")).toBeVisible();
  await expect(page.getByTestId("schema-save")).toBeDisabled();
});
