import { expect, test, type Browser, type BrowserContext, type Page } from "@playwright/test";

import { csrfHeaders } from "./approval-fixture";
import {
  cleanupAll,
  deleteArtifactApi,
  deleteTableApi,
  grantTableEditor,
  loginAs,
  loginViaKeycloak,
  removeWorkflowApi,
  uniqueName,
} from "./helpers";

/// テーブルの設定の「状態の進め方」（GAP-44・D-84・D-115）:
/// - テーブルを開き、画面で状態の進め方を作る（状態の列・状態・進め方・申請者の列）。誤り（重複・未選択）は欄の下で言い、
///   直すまで保存できない。
/// - 承認を待つワークフローを組み、ほかの人（bob）が承認すると状態が進む。既定のままでは、申請した本人（alice）は
///   承認者の列が自分でも判断できず、最初の状態を過ぎると申請を書き換えられない。
/// - ワークフローのエディターで、状態の進め方の無いテーブルを選んでいると、作る画面への案内が出て、開くとその画面が出る。
///
/// `E2E_SHOTS_DIR` があれば、PC（1440px）とスマホ（390px）の画面を撮る。

const SHOTS = process.env.E2E_SHOTS_DIR;

type Me = { id: string; tenant_id: string };

async function me(page: Page): Promise<Me> {
  return (await (await page.request.get("/api/me")).json()) as Me;
}

/// 申請のテーブル（状態の進め方はまだ無い・状態の列の選択肢だけがある）。
async function createTable(page: Page, name: string): Promise<string> {
  const res = await page.request.post("/api/data/tables", {
    headers: await csrfHeaders(page),
    data: {
      name,
      schema: {
        fields: [
          { key: "title", display_name: "件名", type: "text", indexed: true },
          { key: "applicant", display_name: "申請者", type: "user_ref" },
          { key: "approver", display_name: "承認者", type: "user_ref", indexed: true },
          { key: "status", display_name: "状態", type: "select", options: ["申請中", "承認", "却下"], indexed: true },
        ],
      },
    },
  });
  expect(res.status(), await res.text()).toBe(201);
  return ((await res.json()) as { id: string }).id;
}

/// テーブルが指している状態の進め方の定義の id（無ければ null）。
async function fsmIdOf(page: Page, tableId: string): Promise<string | null> {
  const res = await page.request.get(`/api/data/tables/${tableId}`);
  if (!res.ok()) return null;
  const t = (await res.json()) as { schema: { fsm_ref?: { artifact_id: string } | null } };
  return t.schema.fsm_ref?.artifact_id ?? null;
}

async function addRecord(page: Page, tableId: string, data: Record<string, unknown>): Promise<{ id: string; rev: number }> {
  const res = await page.request.post(`/api/data/tables/${tableId}/records`, { headers: await csrfHeaders(page), data: { data } });
  expect(res.status(), await res.text()).toBe(201);
  return (await res.json()) as { id: string; rev: number };
}

async function recordOf(page: Page, tableId: string, id: string): Promise<{ rev: number; data: Record<string, unknown> }> {
  const res = await page.request.get(`/api/data/tables/${tableId}/records/${id}`);
  expect(res.ok(), await res.text()).toBeTruthy();
  return (await res.json()) as { rev: number; data: Record<string, unknown> };
}

/// レコードが追加されたら、承認者の列の人に承認を頼むワークフロー（alice が自動実行をオンにする）。
async function createApprovalWorkflow(page: Page, tableId: string): Promise<string> {
  const headers = await csrfHeaders(page);
  const name = uniqueName("expense-state-flow");
  const flow = await page.request.post("/api/workflows", {
    headers,
    data: {
      ir: {
        ir_version: 1,
        name,
        display_name: name,
        triggers: [{ kind: "event", source: "data.record.created", scope: { table: tableId } }],
        nodes: [
          {
            id: "ok",
            type: "human.approval",
            label: "上長の承認",
            params: {
              title: "経費の承認",
              record: { table: tableId, id: { $from: "trigger", path: "/record_id" } },
              assignees: [{ field: "approver" }],
              approve: { to: "承認" },
              reject: { to: "却下" },
              show_fields: ["title"],
            },
          },
          { id: "done", type: "control.join", label: "承認された", params: {} },
          { id: "ng", type: "control.join", label: "却下された", params: {} },
        ],
        edges: [
          { from: "ok", from_port: "approved", to: "done" },
          { from: "ok", from_port: "rejected", to: "ng" },
        ],
        policies: { run_timeout_sec: 1_209_600 },
      },
    },
  });
  expect(flow.ok(), await flow.text()).toBeTruthy();
  const id = ((await flow.json()) as { id: string }).id;
  const enabled = await page.request.post(`/api/workflows/${id}/enable`, {
    headers,
    data: {
      version: 1,
      grants: [
        { scope: "data.read", object_type: "data_table", object_id: tableId, relation: "viewer" },
        { scope: "data.write", object_type: "data_table", object_id: tableId, relation: "editor" },
      ],
    },
  });
  expect(enabled.ok(), await enabled.text()).toBeTruthy();
  return id;
}

/// `page` の利用者の「承認」の画面に、レコード `recordId` の依頼が出るまで待つ（relay の tick）。
async function waitForInbox(page: Page, recordId: string): Promise<void> {
  await expect
    .poll(
      async () => {
        const list = (await (await page.request.get("/api/approvals")).json()) as { items: { record: { id: string } }[] };
        return list.items.some((i) => i.record.id === recordId);
      },
      { timeout: 60_000, intervals: [1_000] },
    )
    .toBe(true);
}

async function phoneOf(browser: Browser, from: BrowserContext): Promise<BrowserContext> {
  return browser.newContext({
    storageState: await from.storageState(),
    viewport: { width: 390, height: 844 },
    deviceScaleFactor: 2,
    isMobile: true,
    hasTouch: true,
  });
}

test("画面で状態の進め方を作り、承認すると状態が進む。既定のままでは申請した本人は自分で承認できない（GAP-44・D-115）", async ({
  page,
  browser,
}) => {
  test.setTimeout(300_000);
  await page.setViewportSize({ width: 1440, height: 900 });
  // bob（承認する人）を先にログインさせる（ディレクトリに載せる＝メンバーの列に入れられる）。
  const bobCtx = await browser.newContext({ viewport: { width: 1440, height: 900 } });
  const bob = await bobCtx.newPage();
  await loginAs(bob, "bob");
  await loginViaKeycloak(page);
  const alice = await me(page);
  const bobMe = await me(bob);
  const name = uniqueName("e2e-経費の申請");
  let tableId = "";
  let fsmId: string | null = null;
  let workflowId = "";
  let revokeBob: (() => Promise<void>) | null = null;
  try {
    tableId = await createTable(page, name);
    revokeBob = await grantTableEditor(page, alice.tenant_id, bobMe.id, tableId);

    // 画面: テーブルを開き、「状態の進め方」を作る。状態の列・状態・申請者の列は見当で入っている。
    await page.goto(`/data/${tableId}`);
    await expect(page.getByTestId("data-grid")).toBeVisible({ timeout: 15_000 });
    await page.getByTestId("state-flow-open").click();
    const editor = page.getByTestId("state-flow-editor");
    await expect(editor.getByRole("combobox", { name: "状態の列" })).toContainText("状態", { timeout: 15_000 });
    const states = editor.getByTestId("state-flow-state");
    await expect(states).toHaveCount(3);
    await expect(states.first()).toContainText("最初の状態");
    await expect(editor.getByRole("combobox", { name: "申請者の列" })).toContainText("申請者");
    await expect(editor.getByTestId("state-flow-write-lock")).toBeChecked();
    if (SHOTS) await page.screenshot({ path: `${SHOTS}/state-flow-new-pc.png` });

    // 進め方を 2 つ足す: 申請中 → 承認、申請中 → 却下。進められる人は「承認者」の人、申請した本人は除く（既定）。
    await editor.getByTestId("state-flow-add-move").click();
    await editor.getByTestId("state-flow-add-move").click();
    const moves = editor.getByTestId("state-flow-move");
    await expect(moves).toHaveCount(2);
    await expect(moves.nth(0).getByRole("combobox", { name: "進め方 1のいまの状態" })).toHaveText("申請中");
    await expect(moves.nth(0).getByRole("combobox", { name: "進め方 1の次の状態" })).toHaveText("承認");
    await expect(moves.nth(1).getByRole("combobox", { name: "進め方 2の次の状態" })).toHaveText("却下");
    for (const i of [0, 1]) {
      await expect(moves.nth(i)).toContainText("「承認者」の人");
      await expect(moves.nth(i).getByRole("switch", { name: `進め方 ${i + 1}で申請した本人は除く` })).toBeChecked();
    }

    // 誤りは欄の下で言い、直すまで保存できない: 3 つ目は次の状態が無い → 承認を選ぶと重複。
    await editor.getByTestId("state-flow-add-move").click();
    const third = moves.nth(2);
    await expect(third).toContainText("次の状態を選んでください。");
    await expect(editor.getByTestId("state-flow-save")).toBeDisabled();
    await third.getByRole("combobox", { name: "進め方 3の次の状態" }).click();
    await page.getByRole("option", { name: "承認", exact: true }).click();
    await expect(third).toContainText("同じ進め方がもう 1 つあります。1 つにまとめてください。");
    await expect(editor.getByTestId("state-flow-save")).toBeDisabled();
    await expect(page.getByRole("listbox")).toHaveCount(0);
    await third.scrollIntoViewIfNeeded();
    if (SHOTS) await page.screenshot({ path: `${SHOTS}/state-flow-error-pc.png` });
    // 申請した本人を除くのをオフにすると、欄の下でそう言う（戻しておく）。
    await moves.nth(0).getByRole("switch", { name: "進め方 1で申請した本人は除く" }).click();
    await expect(moves.nth(0).getByTestId("state-flow-self-allowed")).toHaveText("申請した本人も、この進め方で進められます。");
    await moves.nth(0).getByRole("switch", { name: "進め方 1で申請した本人は除く" }).click();
    await third.getByRole("button", { name: /進め方 3（「申請中」から「承認」）を削除/ }).first().click();
    await expect(moves).toHaveCount(2);
    await expect(editor.getByTestId("state-flow-save")).toBeEnabled();
    if (SHOTS) await page.screenshot({ path: `${SHOTS}/state-flow-pc.png` });
    await editor.getByTestId("state-flow-save").click();
    await expect(editor).toBeHidden({ timeout: 15_000 });
    await expect(page.getByText("状態の進め方を保存しました").first()).toBeVisible();

    // 裏取り: 進められる人は「承認者の列の人で、申請した本人は除く」、書き込みの決まりは申請者の列で入った。
    const saved = (await (await page.request.get(`/api/data/tables/${tableId}/fsm`)).json()) as {
      fsm: { body: { states: string[]; transitions: { from: string; to: string; actor: unknown }[] } };
      applicant_write_lock: string | null;
    };
    fsmId = await fsmIdOf(page, tableId);
    expect(saved.fsm.body.states).toEqual(["申請中", "承認", "却下"]);
    const guard = {
      all: [
        { field_cmp: { field: "approver", op: "eq", value: "user_id" } },
        { field_cmp: { field: "applicant", op: "ne", value: "user_id" } },
      ],
    };
    expect(saved.fsm.body.transitions).toEqual([
      { from: "申請中", to: "承認", actor: guard },
      { from: "申請中", to: "却下", actor: guard },
    ]);
    expect(saved.applicant_write_lock).toBe("applicant");

    // 列の編集: 状態の列の選択肢は、ここでは変えずに「状態の進め方」で変えると言う。
    await page.getByTestId("schema-editor-open").click();
    const schemaEditor = page.getByTestId("schema-editor");
    await schemaEditor.getByRole("button", { name: "「状態」の設定" }).click();
    const stateOptions = schemaEditor.getByTestId("schema-state-options");
    await expect(stateOptions).toContainText("選択肢は、テーブルの「状態の進め方」で状態として変えてください。");
    await expect(schemaEditor.getByPlaceholder("選択肢を追加（カンマ区切りで複数）")).toHaveCount(0);
    if (SHOTS) await page.screenshot({ path: `${SHOTS}/state-flow-schema-pc.png` });
    await page.keyboard.press("Escape");
    await expect(schemaEditor).toBeHidden();

    // 承認を待つワークフロー（レコードが追加されたら承認者の列の人に頼む）。
    workflowId = await createApprovalWorkflow(page, tableId);
    // 申請 1: alice が申請し、承認者の列も alice。申請 2: alice が申請し、承認者は bob。
    const own = await addRecord(page, tableId, { title: "自分で承認したい出張", applicant: alice.id, approver: alice.id });
    const other = await addRecord(page, tableId, { title: "bob に頼む出張", applicant: alice.id, approver: bobMe.id });

    // alice の「承認」の画面: 依頼は出るが、申請した本人なので判断できない（理由を言う）。
    await waitForInbox(page, own.id);
    await page.goto("/approvals");
    const ownItem = page.getByTestId("approval-item").filter({ hasText: "自分で承認したい出張" });
    await expect(ownItem).toBeVisible({ timeout: 15_000 });
    await ownItem.click();
    const sheet = page.getByTestId("approval-sheet");
    await expect(sheet.getByTestId("approval-not-allowed")).toBeVisible({ timeout: 15_000 });
    await expect(sheet.getByRole("radio", { name: /承認する/ })).toBeDisabled();
    if (SHOTS) await page.screenshot({ path: `${SHOTS}/state-flow-self-approval-blocked-pc.png` });
    await page.keyboard.press("Escape");
    // 画面を通さず遷移しても断られ、状態は申請中のまま。
    const forced = await page.request.post(`/api/data/tables/${tableId}/records/${own.id}/transition`, {
      headers: await csrfHeaders(page),
      data: { to: "承認", expected_rev: (await recordOf(page, tableId, own.id)).rev },
    });
    expect(forced.status()).toBe(403);
    expect((await recordOf(page, tableId, own.id)).data.status).toBe("申請中");

    // bob の「承認」の画面: alice の申請を承認すると、レコードの状態が進む。
    await waitForInbox(bob, other.id);
    await bob.goto("/approvals");
    const item = bob.getByTestId("approval-item").filter({ hasText: "bob に頼む出張" });
    await expect(item).toBeVisible({ timeout: 15_000 });
    await item.click();
    const bobSheet = bob.getByTestId("approval-sheet");
    await bobSheet.getByRole("radio", { name: /承認する/ }).click();
    const decided = bob.waitForResponse((r) => r.url().includes("/decision") && r.request().method() === "POST");
    await bobSheet.getByTestId("approval-send").click();
    expect((await decided).status()).toBe(202);
    await expect.poll(async () => (await recordOf(page, tableId, other.id)).data.status, { timeout: 30_000 }).toBe("承認");

    // 最初の状態を過ぎたら、申請者（alice）は書き換えられない（承認者の列を自分にして承認し直す道を閉じる）。
    const after = await recordOf(page, tableId, other.id);
    const patch = await page.request.patch(`/api/data/tables/${tableId}/records/${other.id}`, {
      headers: await csrfHeaders(page),
      data: { patch: { approver: alice.id }, expected_rev: after.rev },
    });
    expect(patch.status()).toBe(403);
  } finally {
    await cleanupAll(
      () => (workflowId ? removeWorkflowApi(page, workflowId) : Promise.resolve()),
      () => (revokeBob ? revokeBob() : Promise.resolve()),
      async () => {
        fsmId ??= tableId ? await fsmIdOf(page, tableId) : null;
      },
      () => (tableId ? deleteTableApi(page, tableId) : Promise.resolve()),
      () => (fsmId ? deleteArtifactApi(page, fsmId) : Promise.resolve()),
      () => bobCtx.close(),
    );
  }
});

test("状態の進め方の無いテーブルを選んだブロックから作る画面へ案内し、スマホ幅でも作れる（GAP-44）", async ({ page, browser }) => {
  test.setTimeout(240_000);
  await page.setViewportSize({ width: 1440, height: 900 });
  await loginViaKeycloak(page);
  const headers = await csrfHeaders(page);
  let fsmTable = "";
  let plainTable = "";
  let workflowId = "";
  let phone: BrowserContext | null = null;
  try {
    // 状態の進め方のあるテーブル（API で作る）と、無いテーブル。
    fsmTable = await createTable(page, uniqueName("e2e-備品の申請"));
    plainTable = await createTable(page, uniqueName("e2e-まだ状態の無い申請"));
    const t = (await (await page.request.get(`/api/data/tables/${fsmTable}/fsm`)).json()) as { schema_version: number };
    const put = await page.request.put(`/api/data/tables/${fsmTable}/fsm`, {
      headers,
      data: {
        status_field: "status",
        body: {
          states: ["申請中", "承認", "却下"],
          transitions: [
            { from: "申請中", to: "承認", actor: "public" },
            { from: "申請中", to: "却下", actor: "public" },
          ],
        },
        applicant_write_lock: null,
        expected_schema_version: t.schema_version,
      },
    });
    expect(put.status(), await put.text()).toBe(200);
    const name = uniqueName("lend-state-flow");
    const flow = await page.request.post("/api/workflows", {
      headers,
      data: {
        ir: {
          ir_version: 1,
          name,
          display_name: name,
          triggers: [{ kind: "interactive" }],
          nodes: [
            {
              id: "ok",
              type: "human.approval",
              label: "上長の承認",
              params: {
                title: "備品の承認",
                record: { table: fsmTable, id: { $from: "input", path: "/record_id" } },
                assignees: [{ field: "approver" }],
                approve: { to: "承認" },
              },
            },
          ],
          edges: [],
          input_schema: { type: "object", properties: { record_id: { type: "string" } } },
          policies: { run_timeout_sec: 1_209_600 },
        },
      },
    });
    expect(flow.ok(), await flow.text()).toBeTruthy();
    workflowId = ((await flow.json()) as { id: string }).id;
    // 承認するテーブルを、状態の進め方の無いテーブルにした版を開く（保存の照合が入る前の定義・AI が書いた定義）。
    const bindPlain = async (p: Page) => {
      await p.route(`**/api/workflows/${workflowId}`, async (route) => {
        if (route.request().method() !== "GET") return route.continue();
        const res = await route.fetch();
        const body = (await res.json()) as { ir: { nodes: { id: string; params: { record: { table: string } } }[] } };
        const node = body.ir.nodes.find((n) => n.id === "ok");
        if (node) node.params.record.table = plainTable;
        await route.fulfill({ response: res, json: body });
      });
    };

    // PC: ブロックの設定に、状態の進め方を作る画面へのリンクが出る。開くと、そのテーブルの「状態の進め方」が出る。
    await bindPlain(page);
    await page.goto(`/workflows/${workflowId}`);
    const canvas = page.locator(".react-flow");
    await expect(canvas.getByText("上長の承認").first()).toBeVisible({ timeout: 15_000 });
    await canvas.getByText("上長の承認").first().click();
    const panel = page.getByRole("complementary", { name: "ブロックの設定" });
    await expect(panel.getByTestId("workflow-table-note")).toHaveText("このテーブルには状態の進め方がないため、承認には使えません。", {
      timeout: 15_000,
    });
    const link = panel.getByTestId("state-flow-link");
    await expect(link).toHaveAttribute("href", `/data/${plainTable}?panel=states`);
    await expect(link).toHaveAttribute("target", "_blank");
    if (SHOTS) await panel.screenshot({ path: `${SHOTS}/state-flow-link-pc.png` });
    const [opened] = await Promise.all([page.context().waitForEvent("page"), link.click()]);
    const editor = opened.getByTestId("state-flow-editor");
    await expect(editor).toBeVisible({ timeout: 30_000 });
    await expect(editor.getByTestId("state-flow-state")).toHaveCount(3, { timeout: 15_000 });
    await opened.close();

    // スマホ: エディターの一覧からブロックを開いても同じ案内が出る。
    phone = await phoneOf(browser, page.context());
    const p = await phone.newPage();
    await bindPlain(p);
    await p.goto(`/workflows/${workflowId}`);
    const mobileFlow = p.getByTestId("workflow-mobile-flow");
    await expect(mobileFlow).toBeVisible({ timeout: 15_000 });
    await mobileFlow.getByRole("button", { name: /上長の承認/ }).filter({ hasText: "上長の承認" }).click();
    const sheet = p.getByRole("dialog");
    await expect(sheet.getByTestId("state-flow-link")).toBeVisible({ timeout: 15_000 });
    if (SHOTS) await p.screenshot({ path: `${SHOTS}/state-flow-link-phone.png` });

    // スマホ: 案内の先（?panel=states）で状態の進め方を作れる。横にはみ出さない。
    await p.unrouteAll({ behavior: "ignoreErrors" });
    await p.goto(`/data/${plainTable}?panel=states`);
    const phoneEditor = p.getByTestId("state-flow-editor");
    await expect(phoneEditor.getByTestId("state-flow-state")).toHaveCount(3, { timeout: 15_000 });
    await phoneEditor.getByTestId("state-flow-add-move").click();
    await phoneEditor.getByTestId("state-flow-add-move").click();
    const moves = phoneEditor.getByTestId("state-flow-move");
    await expect(moves).toHaveCount(2);
    await expect(moves.nth(1)).toContainText("「承認者」の人");
    expect(await phoneEditor.evaluate((el) => el.scrollWidth <= el.clientWidth + 1)).toBe(true);
    if (SHOTS) await p.screenshot({ path: `${SHOTS}/state-flow-phone.png` });
    await moves.nth(1).scrollIntoViewIfNeeded();
    if (SHOTS) await p.screenshot({ path: `${SHOTS}/state-flow-phone-moves.png` });
    await phoneEditor.getByTestId("state-flow-save").click();
    await expect(phoneEditor).toBeHidden({ timeout: 15_000 });
    // 案内の印（?panel=states）は閉じると外れる。
    await expect(p).toHaveURL(new RegExp(`/data/${plainTable}$`));
    expect(await fsmIdOf(p, plainTable)).not.toBeNull();
  } finally {
    const ids = [fsmTable, plainTable].filter(Boolean);
    const fsms = await Promise.all(ids.map((id) => fsmIdOf(page, id).catch(() => null)));
    await cleanupAll(
      () => (workflowId ? removeWorkflowApi(page, workflowId) : Promise.resolve()),
      ...ids.map((id) => () => deleteTableApi(page, id)),
      ...fsms.map((id) => () => (id ? deleteArtifactApi(page, id) : Promise.resolve())),
      () => (phone ? phone.close() : Promise.resolve()),
    );
  }
});

test("状態の進め方: 保存していない変更・ほかの人の先の保存・書き換えられなくなるレコードを、消す前・保存する前に言う（GAP-44・D-115）", async ({
  page,
  browser,
}) => {
  test.setTimeout(240_000);
  await page.setViewportSize({ width: 1440, height: 900 });
  await loginViaKeycloak(page);
  const headers = await csrfHeaders(page);
  const tables: string[] = [];
  let phone: BrowserContext | null = null;
  try {
    // 申請者の列を後から足したテーブル: 状態を過ぎたのに申請者が空のレコードが 2 件ある。
    const tableId = await createTable(page, uniqueName("e2e-前からある申請"));
    tables.push(tableId);
    await addRecord(page, tableId, { title: "前からある 1", status: "承認" });
    await addRecord(page, tableId, { title: "前からある 2", status: "却下" });

    await page.goto(`/data/${tableId}?panel=states`);
    const editor = page.getByTestId("state-flow-editor");
    await expect(editor.getByTestId("state-flow-state")).toHaveCount(3, { timeout: 15_000 });
    await expect(editor.getByTestId("state-flow-write-lock")).toBeChecked();
    // 申請者の列は、追加するときと最初の状態のあいだに書き換えられる（本人を除く決まりを迂回できる）ことを言う。
    await expect(editor.getByTestId("state-flow-applicant")).toContainText(
      "テーブルを編集できる人は、追加するときと「申請中」のあいだに、申請者の列をほかの人にできます。そうすると、自分の申請でも承認できます。",
    );

    // 足したばかりの進め方は、行き先を最初の状態（出し直し）にすると「申請した本人は除く」が外れる。
    for (let i = 0; i < 3; i += 1) await editor.getByTestId("state-flow-add-move").click();
    const moves = editor.getByTestId("state-flow-move");
    const third = moves.nth(2);
    const exclude = third.getByRole("switch", { name: "進め方 3で申請した本人は除く" });
    await expect(exclude).toBeChecked();
    await third.getByRole("combobox", { name: "進め方 3のいまの状態" }).click();
    await page.getByRole("option", { name: "承認", exact: true }).click();
    await third.getByRole("combobox", { name: "進め方 3の次の状態" }).click();
    await page.getByRole("option", { name: "申請中", exact: true }).click();
    await expect(exclude).not.toBeChecked();
    await third.getByRole("button", { name: /進め方 3（「承認」から「申請中」）を削除/ }).first().click();
    await expect(moves).toHaveCount(2);

    // 保存: 申請者が空で最初の状態を過ぎたレコードが誰も書き換えられなくなるので、数を言って確かめる。
    await editor.getByTestId("state-flow-save").click();
    const lockDialog = page.getByRole("dialog", { name: "2 件のレコードを書き換えられなくしますか？" });
    await expect(lockDialog).toBeVisible({ timeout: 15_000 });
    await expect(lockDialog).toContainText("「申請者」が空で「申請中」を過ぎたレコードは、保存すると誰も書き換えや削除ができなくなります。");
    if (SHOTS) await page.screenshot({ path: `${SHOTS}/state-flow-lock-confirm-pc.png` });
    await lockDialog.getByRole("button", { name: "キャンセル" }).click();
    await expect(lockDialog).toBeHidden();
    expect(await fsmIdOf(page, tableId)).toBeNull();

    // ほかの人が先にテーブルの設定を保存した: 変更が保存されていないこと・読み込むと消えることを言う。
    const table = (await (await page.request.get(`/api/data/tables/${tableId}`)).json()) as {
      schema: { fields: unknown[] } & Record<string, unknown>;
      schema_version: number;
    };
    const bumped = await page.request.put(`/api/data/tables/${tableId}/schema`, {
      headers,
      data: {
        schema: { ...table.schema, fields: [...table.schema.fields, { key: "memo", display_name: "メモ", type: "text" }] },
        expected_schema_version: table.schema_version,
      },
    });
    expect(bumped.ok(), await bumped.text()).toBeTruthy();
    await editor.getByTestId("state-flow-save").click();
    await page.getByRole("dialog", { name: "2 件のレコードを書き換えられなくしますか？" }).getByRole("button", { name: "それでも保存" }).click();
    const conflict = editor.getByTestId("state-flow-save-error");
    await expect(conflict).toContainText("ほかの人が先にテーブルの設定を保存しました", { timeout: 15_000 });
    await expect(conflict).toContainText(
      "あなたの変更は保存されていません。最新の内容を読み込むと、保存していない変更は消えます。残したい内容は先に控えてください。",
    );
    if (SHOTS) await page.screenshot({ path: `${SHOTS}/state-flow-conflict-pc.png` });
    await conflict.getByRole("button", { name: "最新の内容を読み込む（変更は消えます）" }).click();
    await expect(editor.getByTestId("state-flow-state")).toHaveCount(3, { timeout: 15_000 });
    await expect(editor.getByTestId("state-flow-move")).toHaveCount(0);
    await expect(editor.getByTestId("state-flow-save-error")).toHaveCount(0);

    // 列の編集を開く（メンバーの列の無いテーブル）: 保存していない変更があれば、消えることを確かめてから開く。
    const plain = await page.request.post("/api/data/tables", {
      headers,
      data: {
        name: uniqueName("e2e-申請者の列の無い申請"),
        schema: {
          fields: [
            { key: "title", display_name: "件名", type: "text", indexed: true },
            { key: "status", display_name: "状態", type: "select", options: ["申請中", "承認"], indexed: true },
          ],
        },
      },
    });
    expect(plain.status(), await plain.text()).toBe(201);
    const plainId = ((await plain.json()) as { id: string }).id;
    tables.push(plainId);
    await page.goto(`/data/${plainId}?panel=states`);
    await expect(editor.getByTestId("state-flow-state")).toHaveCount(2, { timeout: 15_000 });
    await editor.getByRole("textbox", { name: "状態 2 の名前" }).fill("承認済み");
    await editor.getByTestId("state-flow-open-schema").click();
    const discard = page.getByRole("dialog", { name: "保存していない変更があります" });
    await expect(discard).toContainText("列の編集を開くと、この画面を閉じます。保存していない状態の進め方の変更は消えます。");
    await discard.getByRole("button", { name: "キャンセル" }).click();
    await expect(editor.getByRole("textbox", { name: "状態 2 の名前" })).toHaveValue("承認済み");
    await editor.getByTestId("state-flow-open-schema").click();
    await discard.getByRole("button", { name: "変更を捨てて列の編集を開く" }).click();
    await expect(editor).toBeHidden({ timeout: 15_000 });
    await expect(page.getByTestId("schema-editor")).toBeVisible();
    // 案内の印（?panel=states）は外れる（再読み込みで状態の進め方の画面が開き直さない）。
    await expect(page).toHaveURL(new RegExp(`/data/${plainId}$`));
    await page.keyboard.press("Escape");

    // 列の選択肢と状態が合っていない定義: 読み込んだままでも保存できる（保存で選択肢を状態にそろえる）。
    const t = (await (await page.request.get(`/api/data/tables/${plainId}/fsm`)).json()) as { schema_version: number };
    const put = await page.request.put(`/api/data/tables/${plainId}/fsm`, {
      headers,
      data: {
        status_field: "status",
        body: { states: ["申請中", "承認"], transitions: [{ from: "申請中", to: "承認", actor: "public" }] },
        applicant_write_lock: null,
        expected_schema_version: t.schema_version,
      },
    });
    expect(put.status(), await put.text()).toBe(200);
    await page.route(`**/api/data/tables/${plainId}/fsm`, async (route) => {
      if (route.request().method() !== "GET") return route.continue();
      const res = await route.fetch();
      const body = (await res.json()) as { fsm: { body: { states: string[] } } };
      body.fsm.body.states = ["申請中", "承認", "保留"];
      await route.fulfill({ response: res, json: body });
    });
    await page.goto(`/data/${plainId}?panel=states`);
    await expect(editor.getByTestId("state-flow-states")).toContainText("両方を合わせて出しています", { timeout: 15_000 });
    await expect(editor.getByTestId("state-flow-save")).toBeEnabled();
    await page.unrouteAll({ behavior: "ignoreErrors" });

    // スマホ: 申請者の列の注意と、書き換えられなくなるレコードの確かめが横にはみ出さない。
    phone = await phoneOf(browser, page.context());
    const p = await phone.newPage();
    await p.goto(`/data/${tableId}?panel=states`);
    const phoneEditor = p.getByTestId("state-flow-editor");
    await expect(phoneEditor.getByTestId("state-flow-state")).toHaveCount(3, { timeout: 15_000 });
    await phoneEditor.getByTestId("state-flow-applicant").scrollIntoViewIfNeeded();
    expect(await phoneEditor.evaluate((el) => el.scrollWidth <= el.clientWidth + 1)).toBe(true);
    if (SHOTS) await p.screenshot({ path: `${SHOTS}/state-flow-applicant-phone.png` });
    await phoneEditor.getByTestId("state-flow-add-move").click();
    await phoneEditor.getByTestId("state-flow-save").click();
    const phoneLock = p.getByRole("dialog", { name: "2 件のレコードを書き換えられなくしますか？" });
    await expect(phoneLock).toBeVisible({ timeout: 15_000 });
    if (SHOTS) await p.screenshot({ path: `${SHOTS}/state-flow-lock-confirm-phone.png` });
    await phoneLock.getByRole("button", { name: "キャンセル" }).click();
  } finally {
    const fsms = await Promise.all(tables.map((id) => fsmIdOf(page, id).catch(() => null)));
    await cleanupAll(
      ...tables.map((id) => () => deleteTableApi(page, id)),
      ...fsms.map((id) => () => (id ? deleteArtifactApi(page, id) : Promise.resolve())),
      () => (phone ? phone.close() : Promise.resolve()),
    );
  }
});
