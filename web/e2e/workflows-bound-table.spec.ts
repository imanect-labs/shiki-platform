import { expect, test, type Page } from "@playwright/test";

import { cleanupAll, cleanupQueue, deleteArtifactApi, deleteTableApi, loginViaKeycloak, removeWorkflowApi, uniqueName } from "./helpers";

/// テストで作ったもの（テーブル・ワークフロー・フォルダー）を、テストの後に片付ける（失敗したテストでも走る・BUG-126）。
const cleanup = cleanupQueue();
test.afterEach(() => cleanup.run());


/// 束縛済みのテーブルがテーブルの一覧（GET /data/tables は更新の新しい順に 200 件で打ち切る）に無いときの
/// ワークフローの設定画面の e2e（BUG-68）:
/// - 承認を待つブロック・data.* のブロック・データ変更のきっかけの「テーブル」の欄に、束縛済みのテーブルの名前が
///   出て、状態・列の選択肢も出る（一覧に無い束縛済みの id は 1 件ずつ GET /data/tables/{id} で取る）。
/// - 読めない（無い・削除された・見えない）id なら、空欄のまま黙らずに「このテーブルを読めません」と出す。
/// 一覧に入らない状況は、一覧の応答から束縛済みのテーブルを抜いて作る（実際に 200 件を超えて作らない）。
/// 前提: shiki-server が SHIKI__WORKFLOW__ENABLED=true・LLM は stub。

async function csrfHeaders(page: Page): Promise<Record<string, string>> {
  const csrf = (await page.context().cookies()).find((c) => c.name === "shiki_csrf")?.value ?? "";
  return { "x-csrf-token": csrf, "content-type": "application/json" };
}

type Setup = { tableId: string; tableName: string; workflowId: string };

/// 申請の表（状態の遷移の規則つき）と、その表に束ねたきっかけ・状態を進めるブロック・承認を待つブロック・行を
/// 更新するブロックを持つワークフローを作る。
async function setup(page: Page): Promise<Setup> {
  const headers = await csrfHeaders(page);
  const states = ["下書き", "申請中", "受付", "承認", "却下"];
  const tableName = uniqueName("e2e-束縛の表");
  const created = await page.request.post("/api/data/tables", {
    headers,
    data: {
      name: tableName,
      schema: {
        fields: [
          { key: "title", display_name: "件名", type: "text", indexed: true },
          { key: "amount", display_name: "金額", type: "number" },
          { key: "approver", display_name: "承認者", type: "user_ref", indexed: true },
          { key: "status", display_name: "状態", type: "select", options: states, indexed: true },
        ],
        status_field: "status",
      },
    },
  });
  expect(created.status(), await created.text()).toBe(201);
  const table = (await created.json()) as { id: string; schema: Record<string, unknown>; schema_version: number };
  let fsmId = "";
  cleanup.add(() =>
    cleanupAll(
      () => deleteTableApi(page, table.id),
      async () => fsmId && deleteArtifactApi(page, fsmId),
    ),
  );
  const fsm = await page.request.post("/api/data/fsms", {
    headers,
    data: {
      name: uniqueName("e2e-束縛の流れ"),
      table_id: table.id,
      body: {
        states,
        transitions: [
          { from: "下書き", to: "申請中", actor: "public" },
          { from: "申請中", to: "受付", actor: "public" },
          { from: "受付", to: "承認", actor: "public" },
          { from: "受付", to: "却下", actor: "public" },
        ],
      },
    },
  });
  expect(fsm.status(), await fsm.text()).toBe(201);
  const f = (await fsm.json()) as { id: string; version?: number };
  fsmId = f.id;
  const put = await page.request.put(`/api/data/tables/${table.id}/schema`, {
    headers,
    data: {
      schema: { ...table.schema, fsm_ref: { artifact_id: f.id, version: f.version ?? 1 } },
      expected_schema_version: table.schema_version,
    },
  });
  expect(put.status(), await put.text()).toBe(200);

  const name = uniqueName("bound-table");
  const flow = await page.request.post("/api/workflows", {
    headers,
    data: {
      ir: {
        ir_version: 1,
        name,
        display_name: name,
        triggers: [
          {
            kind: "event",
            source: "data.transition",
            scope: { table: table.id },
            filter: { cmp: { left: { $from: "trigger", path: "/to" }, op: "eq", right: "申請中" } },
          },
        ],
        nodes: [
          {
            id: "accept",
            type: "data.transition",
            label: "受け付ける",
            params: { table: table.id, record: { $from: "trigger", path: "/record_id" }, to: "受付" },
          },
          {
            id: "ok",
            type: "human.approval",
            label: "上長の承認",
            params: {
              title: "経費申請の承認",
              record: { table: table.id, id: { $from: "trigger", path: "/record_id" } },
              assignees: [{ field: "approver" }],
              approve: { to: "承認" },
              reject: { to: "却下" },
              show_fields: ["title", "amount"],
            },
          },
          {
            id: "memo",
            type: "data.record.update",
            label: "件名を書き換える",
            params: { table: table.id, record: { $from: "trigger", path: "/record_id" }, patch: { title: "確認済み" } },
          },
          { id: "ng", type: "control.join", label: "却下された", params: {} },
        ],
        edges: [
          { from: "accept", to: "ok" },
          { from: "ok", from_port: "approved", to: "memo" },
          { from: "ok", from_port: "rejected", to: "ng" },
        ],
        policies: { run_timeout_sec: 1209600 },
      },
    },
  });
  expect(flow.ok(), await flow.text()).toBeTruthy();
  const workflowId = ((await flow.json()) as { id: string }).id;
  cleanup.add(() => removeWorkflowApi(page, workflowId));
  return { tableId: table.id, tableName, workflowId };
}

/// テーブルの一覧の応答から `hidden` のテーブルを抜く（一覧の 200 件に入らなかったのと同じ見え方）。抜いた回数を返す。
async function hideFromList(page: Page, hidden: string): Promise<{ hits: () => number }> {
  let hits = 0;
  await page.route("**/api/data/tables", async (route) => {
    if (route.request().method() !== "GET") return route.continue();
    const res = await route.fetch();
    const body = (await res.json()) as { items: { id: string }[] };
    if (body.items.some((t) => t.id === hidden)) hits += 1;
    body.items = body.items.filter((t) => t.id !== hidden);
    await route.fulfill({ response: res, json: body });
  });
  return { hits: () => hits };
}

test("束縛済みのテーブルが一覧に無くても、承認・data.*・きっかけの欄に名前と状態・列の選択肢が出る（BUG-68）", async ({
  page,
}) => {
  test.setTimeout(120_000);
  await loginViaKeycloak(page);
  const s = await setup(page);
  const hidden = await hideFromList(page, s.tableId);
  await page.goto(`/workflows/${s.workflowId}`);
  const canvas = page.locator(".react-flow");
  await expect(canvas.getByText("上長の承認").first()).toBeVisible({ timeout: 15_000 });

  // データ変更のきっかけ（キャンバスの先頭にあるので最初に開く）: テーブルと「この状態になったとき」。
  await canvas.getByText("テーブルの行の状態が変わったとき").first().click();
  const triggerPanel = page.getByRole("complementary", { name: "きっかけの設定" });
  await expect(triggerPanel.getByRole("combobox", { name: "対象のテーブル" })).toContainText(s.tableName);
  expect(hidden.hits()).toBeGreaterThan(0);
  const toState = triggerPanel.getByRole("combobox", { name: "この状態になったとき" });
  await expect(toState).toContainText("申請中");
  await expect(toState).toBeEnabled();

  // 承認を待つブロック: テーブル・進める状態・相手の列・受信箱に出す列。
  await canvas.getByText("上長の承認").first().click();
  const panel = page.getByRole("complementary", { name: "ブロックの設定" });
  await expect(panel.getByRole("combobox", { name: "承認するテーブル" })).toContainText(s.tableName);
  const approveTo = panel.getByRole("combobox", { name: "承認したときに進める状態" });
  await expect(approveTo).toContainText("承認");
  await expect(panel.getByRole("combobox", { name: "却下したときに進める状態" })).toContainText("却下");
  await expect(panel.getByRole("textbox", { name: /に進める状態/ })).toHaveCount(0);
  await expect(panel.getByRole("combobox", { name: "承認者 1 の列" })).toContainText("承認者");
  await expect(panel.getByRole("button", { name: "件名", pressed: true })).toBeVisible();
  await expect(panel.getByRole("button", { name: "金額", pressed: true })).toBeVisible();
  await expect(panel.getByTestId("workflow-table-note")).toHaveCount(0);
  // 状態は選択肢から選べる（差し戻しを足して、テーブルの状態が並ぶ）。
  await panel.getByRole("switch", { name: "差し戻したときの出口を使う" }).click();
  await panel.getByRole("combobox", { name: "差し戻したときに進める状態" }).click();
  for (const st of ["下書き", "申請中", "受付", "承認", "却下"]) {
    await expect(page.getByRole("option", { name: st, exact: true })).toBeVisible();
  }
  await page.getByRole("option", { name: "下書き", exact: true }).click();
  // 束縛済みのテーブルは選択肢にも出る（選び直さなくてよい）。
  await panel.getByRole("combobox", { name: "承認するテーブル" }).click();
  await expect(page.getByRole("option", { name: s.tableName })).toBeVisible();
  await page.keyboard.press("Escape");

  // data.transition: テーブルと次の状態の選択肢。
  await canvas.getByText("受け付ける").first().click();
  await expect(panel.getByRole("combobox", { name: "対象のテーブル" })).toContainText(s.tableName);
  const nextState = panel.getByRole("combobox", { name: "次の状態" });
  await expect(nextState).toContainText("受付");
  await nextState.click();
  await expect(page.getByRole("option", { name: "承認", exact: true })).toBeVisible();
  await page.keyboard.press("Escape");

  // data.record.update: テーブルと列の参照名・変更する値の例。
  await canvas.getByText("件名を書き換える").first().click();
  await expect(panel.getByRole("combobox", { name: "対象のテーブル" })).toContainText(s.tableName);
  await expect(panel.getByText(/列の参照名:/)).toContainText("approver（承認者）");
  await expect(panel.getByTestId("workflow-status-field-hint")).toContainText("「状態」");

  // 保存しても束縛は変わらない（差し戻しを足した分だけ）。
  await page.getByRole("button", { name: "保存", exact: true }).click();
  await expect(page.getByText(/保存しました/).first()).toBeVisible({ timeout: 15_000 });
  const saved = (await (await page.request.get(`/api/workflows/${s.workflowId}`)).json()) as {
    ir: { triggers: { scope?: { table?: string } }[]; nodes: { id: string; params: Record<string, unknown> }[] };
  };
  const ok = saved.ir.nodes.find((n) => n.id === "ok")?.params ?? {};
  expect(ok.record).toMatchObject({ table: s.tableId });
  expect(ok.return).toEqual({ to: "下書き" });
  expect(saved.ir.nodes.find((n) => n.id === "accept")?.params.table).toBe(s.tableId);
  expect(saved.ir.triggers[0]?.scope?.table).toBe(s.tableId);
});

test("束縛済みのテーブルを読めない（無い・削除された）ときは、空欄にせず「このテーブルを読めません」と出す（BUG-68）", async ({
  page,
}) => {
  test.setTimeout(120_000);
  await loginViaKeycloak(page);
  const s = await setup(page);
  // 無いテーブルを指す版を開く（テーブルが消えた・見えなくなった）。保存は検証が止めるので、エディタが読み込む
  // IR だけを書き換える。
  const ghost = "00000000-0000-4000-8000-00000000b068";
  let injected = false;
  await page.route(`**/api/workflows/${s.workflowId}`, async (route) => {
    if (injected || route.request().method() !== "GET") return route.continue();
    injected = true;
    const res = await route.fetch();
    const body = (await res.json()) as {
      ir: { triggers: { scope?: { table?: string } }[]; nodes: { id: string; params: Record<string, unknown> }[] };
    };
    for (const t of body.ir.triggers) if (t.scope?.table) t.scope.table = ghost;
    for (const n of body.ir.nodes) {
      if (n.params.table === s.tableId) n.params.table = ghost;
      const rec = n.params.record as { table?: string } | undefined;
      if (rec && typeof rec === "object" && rec.table === s.tableId) rec.table = ghost;
    }
    await route.fulfill({ response: res, json: body });
  });
  await page.goto(`/workflows/${s.workflowId}`);
  const canvas = page.locator(".react-flow");
  await expect(canvas.getByText("上長の承認").first()).toBeVisible({ timeout: 15_000 });
  expect(injected).toBe(true);
  const NOTE = "このテーブルを読めません（または削除されました）";

  // データ変更のきっかけ（キャンバスの先頭にあるので最初に開く）。
  await canvas.getByText("テーブルの行の状態が変わったとき").first().click();
  const triggerPanel = page.getByRole("complementary", { name: "きっかけの設定" });
  await expect(triggerPanel.getByRole("combobox", { name: "対象のテーブル" })).toContainText("読めないテーブル");
  await expect(triggerPanel.getByTestId("workflow-table-note")).toHaveText(NOTE);

  // 承認を待つブロック: テーブルの欄が読めないと言い、依存する欄は「先にテーブルを選んでください」と言わない。
  await canvas.getByText("上長の承認").first().click();
  const panel = page.getByRole("complementary", { name: "ブロックの設定" });
  await expect(panel.getByRole("combobox", { name: "承認するテーブル" })).toContainText("読めないテーブル");
  await expect(panel.getByTestId("workflow-table-note")).toHaveText(NOTE);
  await expect(panel.getByRole("textbox", { name: /に進める状態/ })).toHaveCount(0);
  await expect(panel.getByText("先にテーブルを選んでください")).toHaveCount(0);
  // 承認・却下の進める状態と、承認者 1 の列。
  await expect(panel.getByText("テーブルを読めないため選べません")).toHaveCount(3);

  // data.* のブロックも同じ。一覧から選び直せば注記は消え、状態の選択肢が出る。
  await canvas.getByText("受け付ける").first().click();
  const tableField = panel.getByRole("combobox", { name: "対象のテーブル" });
  await expect(tableField).toContainText("読めないテーブル");
  await expect(panel.getByTestId("workflow-table-note")).toHaveText(NOTE);
  await tableField.click();
  await page.getByRole("option", { name: s.tableName }).click();
  await expect(tableField).toContainText(s.tableName);
  await expect(panel.getByTestId("workflow-table-note")).toHaveCount(0);
  await expect(panel.getByRole("combobox", { name: "次の状態" })).toContainText("受付");
});

test("テーブル ID を手で打つ途中では 1 件ずつ取りに行かず、「.」を打っても設定の欄が壊れない（BUG-68）", async ({ page }) => {
  test.setTimeout(120_000);
  await loginViaKeycloak(page);
  const s = await setup(page);
  // 一覧に無い束縛済みの id（1 件取り）以外に、/api/data/tables/{何か} を投げた回数を数える。
  const single: string[] = [];
  page.on("request", (r) => {
    const m = /\/api\/data\/tables\/([^/?]+)$/.exec(new URL(r.url()).pathname);
    if (r.method() === "GET" && m && m[1] !== s.tableId) single.push(m[1]);
  });
  await page.goto(`/workflows/${s.workflowId}`);
  const canvas = page.locator(".react-flow");
  await canvas.getByText("受け付ける").first().click();
  const panel = page.getByRole("complementary", { name: "ブロックの設定" });
  await panel.getByRole("button", { name: "前の結果から指定する" }).click();
  const input = panel.getByRole("textbox", { name: "対象のテーブル", exact: true });
  await input.fill("");
  await input.pressSequentially("1234.abcd-ef");
  await expect(input).toHaveValue("1234.abcd-ef");
  // 欄は描画されたまま（応答をテーブルとして扱って落ちない）。
  await expect(panel.getByRole("button", { name: "一覧から選ぶ" })).toBeVisible();
  expect(single).toEqual([]);
});
