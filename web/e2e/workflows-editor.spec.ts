import { expect, test } from "@playwright/test";

import { loginViaKeycloak, uniqueName } from "./helpers";

/// dnd ワークフローエディタの e2e（Task 10.12 DoD）:
/// 新規作成 → ブロック追加（ヘッダ導線＋尻尾プラス）→ 検証エラーが該当ノードにバッジ表示
/// → 修正（削除）→ 保存 → 実行 → 実行履歴で succeeded まで。
/// 前提: compose の shiki-server が SHIKI__WORKFLOW__ENABLED=true・LLM は stub。

test("エディタ: 作成→追加→エラーバッジ→修正→保存→実行→履歴成功", async ({ page }) => {
  test.setTimeout(180_000);
  await loginViaKeycloak(page);

  // 一覧から新規作成 → エディタへ。
  await page.goto("/workflows");
  await page.getByRole("button", { name: "新しいワークフロー" }).click();
  await page.waitForURL(/\/workflows\/[0-9a-f-]+$/i, { timeout: 20_000 });

  // 最初のブロック（スクリプト）をヘッダ導線から追加。
  await page.getByRole("button", { name: "最初のブロック" }).click();
  await page.getByRole("button", { name: /^スクリプト/ }).click();
  const canvas = page.locator(".react-flow");
  await expect(canvas.getByText("スクリプト").first()).toBeVisible();

  // 尻尾のプラスボタンから「ファイルを保存」を追加（主導線）。
  await canvas.getByLabel("次のブロックを追加").first().click();
  await page.getByRole("button", { name: /^ファイルを保存/ }).click();
  await expect(canvas.getByText("ファイルを保存").first()).toBeVisible();

  // ライブ検証（600ms debounce）が declared_scopes 不足を検出し、該当ノードにバッジが出る。
  await expect(canvas.locator('[aria-label^="検証エラー"]').first()).toBeVisible({
    timeout: 10_000,
  });

  // 修正: 問題のブロックを設定パネルから削除 → バッジが消える。
  await canvas.getByText("ファイルを保存").first().click();
  await page.getByRole("button", { name: "このブロックを削除" }).click();
  await expect(canvas.locator('[aria-label^="検証エラー"]')).toHaveCount(0, {
    timeout: 10_000,
  });

  // 保存 → 実行 → 実行履歴の deep-link へ遷移し、ライブ更新で成功する。
  await page.getByRole("button", { name: "保存", exact: true }).click();
  // toast は aria-live 領域にも複製されるため first() で strict 違反を避ける。
  await expect(page.getByText(/保存しました/).first()).toBeVisible({ timeout: 15_000 });
  await page.getByRole("button", { name: "実行", exact: true }).click();
  await page.getByRole("button", { name: "実行する" }).click();
  await page.waitForURL(/\/runs\?run=/, { timeout: 20_000 });
  const sheet = page.locator('[role="dialog"]');
  await expect(sheet.getByText("実行の詳細")).toBeVisible();
  await expect(sheet.locator("h2").getByText("成功")).toBeVisible({ timeout: 60_000 });
});

/// 条件の意味論の切替（2026-10）で分岐の結果が変わり得る条件は、保存を止めずに警告で知らせる
/// （保存 API・検証 API の `warnings`・Task 15.7 / 監査 B3-2）。
test("エディタ: 意味論の切替で結果が変わり得る条件を、開いた時と保存の時に知らせる", async ({ page }) => {
  test.setTimeout(120_000);
  await loginViaKeycloak(page);
  const csrf = (await page.context().cookies()).find((c) => c.name === "shiki_csrf")?.value ?? "";
  const res = await page.request.post("/api/workflows", {
    headers: { "x-csrf-token": csrf, "content-type": "application/json" },
    data: {
      ir: {
        ir_version: 1,
        name: uniqueName("semantics-e2e"),
        display_name: "配列の判定",
        triggers: [{ kind: "interactive" }],
        nodes: [
          {
            id: "br",
            type: "control.branch",
            label: "タグの判定",
            // 配列のリテラルに contains（2026-10 から「いずれかの要素の部分文字列」）。
            params: { condition: { cmp: { left: ["経費", "交通"], op: "contains", right: "経" } } },
          },
        ],
        edges: [],
      },
    },
  });
  expect(res.ok(), await res.text()).toBeTruthy();
  const created = (await res.json()) as { id: string; warnings: { code: string }[] };
  expect(created.warnings.map((w) => w.code)).toEqual(["ir.condition_semantics_changed"]);

  // 開いた時点で、ノードに注意の印・上部に件数が出る（保存済みの定義の確認）。
  await page.goto(`/workflows/${created.id}`);
  const canvas = page.locator(".react-flow");
  await expect(canvas.locator('[aria-label="注意 1 件"]')).toBeVisible({ timeout: 15_000 });
  const summary = page.getByTestId("workflow-warnings");
  await expect(summary).toContainText("確認が必要な条件 1 件");

  // 一覧から該当のブロックを開くと、設定パネルにも理由が出る。
  await summary.click();
  await page.getByRole("button", { name: /タグの判定/ }).click();
  await expect(page.getByTestId("node-warnings")).toContainText("配列値に contains");

  // 名前を変えて保存しても止まらず、保存の通知に警告の件数が出る。
  await page.getByPlaceholder("条件分岐").fill("タグの判定（確認済み）");
  await page.getByRole("button", { name: "保存", exact: true }).click();
  await expect(page.getByText(/保存しました（v2）・確認が必要な条件が 1 件あります/).first()).toBeVisible({
    timeout: 15_000,
  });
  await expect(canvas.locator('[aria-label="注意 1 件"]')).toBeVisible();
});

test("データを追加のレコードの値は JSON のオブジェクトに限り、文字列では保存しない（C-P2-1）", async ({ page }) => {
  await loginViaKeycloak(page);
  const csrf = (await page.context().cookies()).find((c) => c.name === "shiki_csrf")?.value ?? "";
  const headers = { "x-csrf-token": csrf };
  const t = await page.request.post("/api/data/tables", {
    headers,
    data: { name: uniqueName("e2e-wf-data"), schema: { fields: [{ key: "name", display_name: "件名", type: "text" }] } },
  });
  expect(t.status(), await t.text()).toBe(201);
  const tableId = ((await t.json()) as { id: string }).id;
  const name = uniqueName("wf-data-object");
  const res = await page.request.post("/api/workflows", {
    headers,
    data: {
      ir: {
        ir_version: 1,
        name,
        display_name: name,
        declared_scopes: ["data.write"],
        triggers: [{ kind: "interactive" }],
        nodes: [{ id: "c", type: "data.record.create", label: "データを追加", params: { table: tableId, data: { name: "x" } } }],
        edges: [],
      },
    },
  });
  expect(res.ok(), await res.text()).toBeTruthy();
  const id = ((await res.json()) as { id: string }).id;
  await page.goto(`/workflows/${id}`);
  await page.locator(".react-flow").getByText("データを追加").first().click();
  const value = page.getByLabel("レコードの値");
  await value.fill('"文字列"');
  await expect(page.getByText("{ … } のオブジェクトで書いてください")).toBeVisible();
  await value.fill('{"name": "出張"}');
  await expect(page.getByText("{ … } のオブジェクトで書いてください")).toHaveCount(0);
  await page.getByRole("button", { name: "保存", exact: true }).click();
  await expect(page.getByText(/保存しました/).first()).toBeVisible({ timeout: 15_000 });
  const saved = (await (await page.request.get(`/api/workflows/${id}`)).json()) as {
    ir: { nodes: { params: { data: unknown } }[] };
  };
  expect(saved.ir.nodes[0].params.data).toEqual({ name: "出張" });
});
