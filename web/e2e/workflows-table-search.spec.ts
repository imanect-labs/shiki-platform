import { expect, test, type Browser, type Page } from "@playwright/test";

import { loginViaKeycloak, uniqueName } from "./helpers";

/// ワークフローの設定画面のテーブルの選択を名前で探す e2e（GAP-42）:
/// - 一覧（GET /data/tables）の先頭のページに無いテーブルを、探す欄に名前を入れて探し（サーバの `?q=`）、
///   キーボードだけで選べる。選んだテーブルの状態・列が、そのまま承認のブロックの選択肢になる
/// - 一覧を打ち切っているときは「さらに読み込む」で続き（サーバの `?cursor=`）を足せる
/// - 狭い画面（設定パネルが下からのシート）でも、シートの中に広げた一覧で探して選べる
/// 一覧を打ち切った状況は、最初の一覧の応答を本物のサーバの 1 件のページ（`?limit=1`・本物の続きの位置つき）に
/// 差し替えて作る（実際に 200 件を超えて作らない）。探す・続きを読むの要求は本物のサーバへ届く。
/// 前提: shiki-server が SHIKI__WORKFLOW__ENABLED=true・LLM は stub。作った表・状態の流れ・ワークフローは最後に消す。

async function csrfHeaders(page: Page): Promise<Record<string, string>> {
  const csrf = (await page.context().cookies()).find((c) => c.name === "shiki_csrf")?.value ?? "";
  return { "x-csrf-token": csrf, "content-type": "application/json" };
}

type Made = { tables: string[]; artifacts: string[] };

/// 状態の遷移の規則つきの表を作る。
async function fsmTable(page: Page, made: Made, name: string, states: string[]): Promise<string> {
  const headers = await csrfHeaders(page);
  const created = await page.request.post("/api/data/tables", {
    headers,
    data: {
      name,
      schema: {
        fields: [
          { key: "title", display_name: "件名", type: "text", indexed: true },
          { key: "approver", display_name: "承認者", type: "user_ref", indexed: true },
          { key: "status", display_name: "状態", type: "select", options: states, indexed: true },
        ],
        status_field: "status",
      },
    },
  });
  expect(created.status(), await created.text()).toBe(201);
  const table = (await created.json()) as { id: string; schema: Record<string, unknown>; schema_version: number };
  made.tables.push(table.id);
  const fsm = await page.request.post("/api/data/fsms", {
    headers,
    data: {
      name: uniqueName("e2e-探す流れ"),
      table_id: table.id,
      body: {
        states,
        transitions: states.slice(1).map((to) => ({ from: states[0], to, actor: "public" })),
      },
    },
  });
  expect(fsm.status(), await fsm.text()).toBe(201);
  const f = (await fsm.json()) as { id: string; version?: number };
  made.artifacts.push(f.id);
  const put = await page.request.put(`/api/data/tables/${table.id}/schema`, {
    headers,
    data: {
      schema: { ...table.schema, fsm_ref: { artifact_id: f.id, version: f.version ?? 1 } },
      expected_schema_version: table.schema_version,
    },
  });
  expect(put.status(), await put.text()).toBe(200);
  return table.id;
}

/// 承認を待つブロックと状態を進めるブロックを、どちらも `table` に束ねたワークフロー。
async function flow(page: Page, made: Made, table: string): Promise<string> {
  const name = uniqueName("table-search");
  const res = await page.request.post("/api/workflows", {
    headers: await csrfHeaders(page),
    data: {
      ir: {
        ir_version: 1,
        name,
        display_name: name,
        triggers: [{ kind: "event", source: "data.transition", scope: { table } }],
        nodes: [
          {
            id: "accept",
            type: "data.transition",
            label: "受け付ける",
            params: { table, record: { $from: "trigger", path: "/record_id" }, to: "受付" },
          },
          {
            id: "ok",
            type: "human.approval",
            label: "上長の承認",
            params: {
              title: "経費申請の承認",
              record: { table, id: { $from: "trigger", path: "/record_id" } },
              assignees: [{ field: "approver" }],
              approve: { to: "承認" },
              reject: { to: "却下" },
              show_fields: ["title"],
            },
          },
        ],
        edges: [{ from: "accept", to: "ok" }],
        policies: { run_timeout_sec: 1209600 },
      },
    },
  });
  expect(res.ok(), await res.text()).toBeTruthy();
  const id = ((await res.json()) as { id: string }).id;
  made.artifacts.push(id);
  return id;
}

/// 最初の一覧（検索も続きも無い GET /data/tables）を、本物のサーバの 1 件のページに差し替える。差し替えた回数を返す。
async function truncateList(page: Page): Promise<{ hits: () => number }> {
  let hits = 0;
  await page.route(/\/api\/data\/tables(\?.*)?$/, async (route) => {
    const url = new URL(route.request().url());
    if (route.request().method() !== "GET" || url.search !== "") return route.continue();
    hits += 1;
    const res = await route.fetch({ url: `${url.origin}${url.pathname}?limit=1` });
    await route.fulfill({ response: res });
  });
  return { hits: () => hits };
}

async function cleanup(page: Page, made: Made) {
  const headers = await csrfHeaders(page);
  for (const id of [...made.artifacts.reverse(), ...made.tables.reverse().map((t) => `table:${t}`)]) {
    const path = id.startsWith("table:") ? `/api/data/tables/${id.slice(6)}` : `/api/artifacts/${id}`;
    const res = await page.request.delete(path, { headers });
    expect.soft(res.ok(), `${path}: ${res.status()}`).toBeTruthy();
  }
}

test("一覧の先頭に無いテーブルを名前で探してキーボードで選び、続きも読み込める（GAP-42）", async ({ page }) => {
  test.setTimeout(150_000);
  await loginViaKeycloak(page);
  const made: Made = { tables: [], artifacts: [] };
  try {
    await pickOutsideTheFirstPage(page, made);
  } finally {
    await page.unroute(/\/api\/data\/tables(\?.*)?$/);
    await cleanup(page, made);
  }
});

async function pickOutsideTheFirstPage(page: Page, made: Made) {
  // 探して選ぶ表を先に作る（あとで作る表の方が新しく、1 件のページにはそちらだけが入る）。
  const key = uniqueName("探す");
  const targetName = `e2e-${key}-受付簿`;
  const target = await fsmTable(page, made, targetName, ["未着手", "対応中", "完了", "差し戻し"]);
  const listedName = uniqueName("e2e-一覧の表");
  const listed = await fsmTable(page, made, listedName, ["下書き", "申請中", "受付", "承認", "却下"]);
  const id = await flow(page, made, listed);

  // API: 名前で探すと当たり、読めない続きの位置は cursor_invalid。
  const found = await page.request.get(`/api/data/tables?q=${encodeURIComponent(key)}`);
  expect(found.status()).toBe(200);
  expect(((await found.json()) as { items: { id: string }[] }).items.map((t) => t.id)).toEqual([target]);
  const bad = await page.request.get("/api/data/tables?cursor=garbage");
  expect(bad.status()).toBe(400);
  expect(((await bad.json()) as { code?: string }).code).toBe("cursor_invalid");

  const truncated = await truncateList(page);
  await page.goto(`/workflows/${id}`);
  const canvas = page.locator(".react-flow");
  await expect(canvas.getByText("上長の承認").first()).toBeVisible({ timeout: 15_000 });
  await canvas.getByText("上長の承認").first().click();
  const panel = page.getByRole("complementary", { name: "ブロックの設定" });
  const select = panel.getByRole("combobox", { name: "承認するテーブル" });
  await expect(select).toContainText(listedName);
  expect(truncated.hits()).toBeGreaterThan(0);

  // キーボードだけで開く: ↓ で開き、探す欄にフォーカスが移る。先頭のページには探す表が無い。
  await select.focus();
  await page.keyboard.press("ArrowDown");
  const picker = page.getByTestId("workflow-table-picker");
  const search = picker.getByRole("combobox", { name: "テーブルを名前で探す" });
  await expect(search).toBeFocused();
  const options = picker.getByRole("listbox", { name: "承認するテーブル" }).getByRole("option");
  await expect(options).toHaveCount(1);
  await expect(options.first()).toHaveText(listedName);
  await expect(picker.getByText("更新の新しい順に 1 件を出しています")).toBeVisible();
  await expect(picker.getByRole("button", { name: "さらに読み込む" })).toBeVisible();

  // 名前の一部を打つと、サーバで探して当たったものが出る。Enter で選ぶ。
  await page.keyboard.type(key);
  await expect(options).toHaveCount(1);
  await expect(options.first()).toHaveText(targetName);
  await expect(search).toHaveAttribute("aria-activedescendant", /.+/);
  await page.keyboard.press("Enter");
  await expect(picker).toHaveCount(0);
  await expect(select).toContainText(targetName);
  await expect(select).toBeFocused();
  await expect(panel.getByTestId("workflow-table-note")).toHaveCount(0);

  // 選んだ表の定義（一覧の外）で、判断の行き先の選択肢が選び直しになる。
  const approveTo = panel.getByRole("combobox", { name: "承認したときに進める状態" });
  const rejectTo = panel.getByRole("combobox", { name: "却下したときに進める状態" });
  await expect(approveTo).toContainText("状態を選ぶ");
  await approveTo.click();
  for (const st of ["未着手", "対応中", "完了", "差し戻し"]) {
    await expect(page.getByRole("option", { name: st, exact: true })).toBeVisible();
  }
  await page.getByRole("option", { name: "完了", exact: true }).click();
  await rejectTo.click();
  await page.getByRole("option", { name: "差し戻し", exact: true }).click();
  await expect(panel.getByRole("combobox", { name: "相手 1 の列" })).toContainText("承認者");

  // Esc は一覧だけを閉じ、選ぶ欄へ戻る。
  await select.click();
  await expect(picker).toBeVisible();
  await page.keyboard.press("Escape");
  await expect(picker).toHaveCount(0);
  await expect(select).toBeFocused();
  await expect(panel).toBeVisible();

  // 「さらに読み込む」: 続きの位置で本物の次のページを足すと、先頭のページに無かった表が出る。
  await canvas.getByText("受け付ける").first().click();
  const tableOfTransition = panel.getByRole("combobox", { name: "対象のテーブル" });
  await expect(tableOfTransition).toContainText(listedName);
  await tableOfTransition.click();
  const more = picker.getByRole("button", { name: "さらに読み込む" });
  await expect(more).toBeVisible();
  const before = await picker.getByRole("option").count();
  await more.click();
  await expect(picker.getByRole("option", { name: targetName })).toBeVisible();
  expect(await picker.getByRole("option").count()).toBeGreaterThan(before);
  // 足した最初の候補へ移り、一覧にフォーカスが移る（押したボタンが消えても行き場を失わない）。
  await expect(picker.getByRole("listbox", { name: "対象のテーブル" })).toBeFocused();
  await expect(picker.getByRole("option").nth(before)).toHaveAttribute("data-active", "true");
  await page.keyboard.press("Escape");

  // 保存すると、承認のブロックは探して選んだ表に束ねられている。
  await page.getByRole("button", { name: "保存", exact: true }).click();
  await expect(page.getByText(/保存しました/).first()).toBeVisible({ timeout: 15_000 });
  const ir = ((await (await page.request.get(`/api/workflows/${id}`)).json()) as {
    ir: { nodes: { id: string; params: Record<string, unknown> }[] };
  }).ir;
  const ok = ir.nodes.find((n) => n.id === "ok")!.params as {
    record: { table: string };
    approve: { to: string };
    reject: { to: string };
  };
  expect(ok.record.table).toBe(target);
  expect(ok.approve.to).toBe("完了");
  expect(ok.reject.to).toBe("差し戻し");
}

/// ログインは PC の画面で済ませ、そのセッションをタッチ端末の文脈へ持ち込む（workflows-editor.spec と同じ）。
async function phonePage(browser: Browser, page: Page): Promise<Page> {
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

test("狭い画面では、シートの中に広げた一覧で名前を探して選べる（GAP-42）", async ({ page, browser }) => {
  test.setTimeout(120_000);
  await loginViaKeycloak(page);
  const made: Made = { tables: [], artifacts: [] };
  const phone = await phonePage(browser, page);
  try {
    const key = uniqueName("狭い");
    const targetName = `e2e-${key}-受付簿`;
    await fsmTable(page, made, targetName, ["未着手", "対応中", "完了"]);
    const listedName = uniqueName("e2e-一覧の表");
    const listed = await fsmTable(page, made, listedName, ["下書き", "申請中", "受付", "承認", "却下"]);
    const id = await flow(page, made, listed);
    await truncateList(phone);

    await phone.goto(`/workflows/${id}`);
    const steps = phone.getByTestId("workflow-mobile-flow");
    await expect(steps).toBeVisible({ timeout: 15_000 });
    await steps.getByRole("button", { name: /受け付ける/ }).filter({ hasText: "受け付ける" }).click();
    const sheet = phone.getByRole("dialog");
    const select = sheet.getByRole("combobox", { name: "対象のテーブル" });
    await expect(select).toContainText(listedName);
    await select.tap();
    // シートの上にポップオーバーを重ねず、シートの中に一覧を広げる。
    const picker = sheet.getByTestId("workflow-table-picker");
    await expect(picker).toBeVisible();
    await picker.getByRole("combobox", { name: "テーブルを名前で探す" }).fill(key);
    const option = picker.getByRole("option", { name: targetName });
    await expect(option).toBeVisible();
    await option.tap();
    await expect(picker).toHaveCount(0);
    await expect(select).toContainText(targetName);
    await expect(sheet).toBeVisible();
    // 選んだ後も Esc で閉じた後も、フォーカスはシートの容器へ落ちずに選ぶ欄へ戻る。
    await expect(select).toBeFocused();
    await select.tap();
    await expect(picker).toBeVisible();
    await phone.keyboard.press("Escape");
    await expect(picker).toHaveCount(0);
    await expect(select).toBeFocused();
    await expect(sheet).toBeVisible();
    // 横にはみ出さない。
    expect(await sheet.evaluate((el) => el.scrollWidth <= el.clientWidth + 1)).toBe(true);
    expect(await phone.evaluate(() => document.documentElement.scrollWidth)).toBeLessThanOrEqual(390);
  } finally {
    await phone.context().close();
    await cleanup(page, made);
  }
});

test("読み込んだ一覧に状態の遷移の規則があるテーブルが無くても、続きがあれば「作ってください」と断定しない（GAP-42）", async ({
  page,
}) => {
  test.setTimeout(120_000);
  await loginViaKeycloak(page);
  const made: Made = { tables: [], artifacts: [] };
  const listed = await fsmTable(page, made, uniqueName("e2e-束ねた表"), ["下書き", "申請中", "受付", "承認", "却下"]);
  const id = await flow(page, made, listed);
  // いちばん新しい表は状態の遷移の規則を持たない（1 件のページにはこの表だけが入り、続きの位置が付く）。
  const plain = await page.request.post("/api/data/tables", {
    headers: await csrfHeaders(page),
    data: {
      name: uniqueName("e2e-規則の無い表"),
      schema: { fields: [{ key: "title", display_name: "件名", type: "text", indexed: true }] },
    },
  });
  expect(plain.status(), await plain.text()).toBe(201);
  made.tables.push(((await plain.json()) as { id: string }).id);
  try {
    await truncateList(page);
    // 束ねた表の 1 件取りも読めないことにして、読み込んだ表を規則の無い 1 件だけにする。
    await page.route(new RegExp(`/api/data/tables/${listed}$`), (route) =>
      route.request().method() === "GET" ? route.fulfill({ status: 404, json: {} }) : route.continue(),
    );
    await page.goto(`/workflows/${id}`);
    const canvas = page.locator(".react-flow");
    await expect(canvas.getByText("上長の承認").first()).toBeVisible({ timeout: 15_000 });
    await canvas.getByText("上長の承認").first().click();
    const panel = page.getByRole("complementary", { name: "ブロックの設定" });
    const note = panel.getByTestId("approval-no-fsm-table");
    await expect(note).toHaveText(
      "読み込んだテーブルに、状態の遷移の規則があるものはありません。選ぶ欄で名前で探してください。",
    );
    await expect(panel.getByText("テーブルの設定で状態の列と遷移の規則を作ってください")).toHaveCount(0);
  } finally {
    await page.unroute(/\/api\/data\/tables(\?.*)?$/);
    await page.unroute(new RegExp(`/api/data/tables/${listed}$`));
    await cleanup(page, made);
  }
});
