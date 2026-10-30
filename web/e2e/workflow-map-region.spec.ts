import { expect, test, type Browser, type Locator, type Page } from "@playwright/test";

import { cleanupQueue, deleteTableApi, loginViaKeycloak, pinStubOrgDefault, removeWorkflowApi, uniqueName } from "./helpers";

/// 「繰り返し」（control.map）の中をエディターで編集する e2e（BUG-103・D-85）:
/// - 置いた直後（中が空）でも保存でき、注意で「中にブロックがありません」と言う（実行の前に止まる）。
/// - キャンバスでは枠の中の「中にブロックを追加」から、中に AI に聞く・データを追加を足せる。中のブロックの値の候補に
///   「繰り返し中の 1 件」が出る。保存した IR では中のブロックの parent が繰り返しになり、中だけで線がつながる。
/// - リストの 3 件で実行すると、中のブロックが 3 回動き、テーブルに 3 件入る。
/// - 枠へ入れる・枠から出す・枠ごと動かす・枠の大きさ・消すと中も消えること、どれも Undo 1 段で戻ること。キーボードで
///   中に足せること。
/// - スマホ幅では、中のブロックを字下げした一覧に並べ、中の＋で足せる（置けない種類は理由つきで灰色）。
/// 前提: shiki-server が SHIKI__WORKFLOW__ENABLED=true・LLM は stub（組織の既定を stub にしてから流し、最後に戻す）。
/// 作ったテーブル・ワークフローは最後に消す。

const SHOTS = process.env.E2E_SHOTS_DIR;

const cleanup = cleanupQueue();
test.afterEach(() => cleanup.run());

async function csrfHeaders(page: Page): Promise<Record<string, string>> {
  const csrf = (await page.context().cookies()).find((c) => c.name === "shiki_csrf")?.value ?? "";
  return { "x-csrf-token": csrf, "content-type": "application/json" };
}

type IrNode = { id: string; type: string; label?: string; parent?: string | null; params: Record<string, unknown> };
type Ir = { nodes: IrNode[]; edges: { from: string; from_port?: string; to: string }[] };

async function savedIr(page: Page, id: string): Promise<Ir> {
  const res = await page.request.get(`/api/workflows/${id}`);
  expect(res.ok(), await res.text()).toBeTruthy();
  return ((await res.json()) as { ir: Ir }).ir;
}

async function createWorkflow(page: Page, nodes: unknown[], edges: unknown[] = []): Promise<string> {
  const name = uniqueName("wf-map-region");
  const res = await page.request.post("/api/workflows", {
    headers: await csrfHeaders(page),
    data: { ir: { ir_version: 1, name, display_name: name, triggers: [{ kind: "interactive" }], nodes, edges } },
  });
  expect(res.ok(), await res.text()).toBeTruthy();
  const id = ((await res.json()) as { id: string }).id;
  cleanup.add(() => removeWorkflowApi(page, id));
  return id;
}

/// 保存のボタンを押し、保存できたことを待つ。
async function save(page: Page) {
  const done = page.waitForResponse((r) => r.request().method() === "PUT" && /\/api\/workflows\/[^/]+$/.test(r.url()));
  await page.getByRole("button", { name: "保存", exact: true }).click();
  const res = await done;
  expect(res.ok(), await res.text()).toBeTruthy();
}

/// キャンバスの表示位置（fitView のアニメーション）が落ち着くまで待つ。
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

/// 要素の画面上の矩形が、枠の矩形の内側にあるか。
async function inside(inner: Locator, outer: Locator): Promise<boolean> {
  const a = (await inner.boundingBox())!;
  const b = (await outer.boundingBox())!;
  return a.x >= b.x && a.y >= b.y && a.x + a.width <= b.x + b.width && a.y + a.height <= b.y + b.height;
}

/// `node` を見出しで掴んで、画面上で (dx, dy) だけ動かして離す。
async function drag(page: Page, node: Locator, dx: number, dy: number) {
  const box = (await node.boundingBox())!;
  const x = box.x + Math.min(box.width / 2, 120);
  const y = box.y + 24;
  await page.mouse.move(x, y);
  await page.mouse.down();
  await page.mouse.move(x + dx / 2, y + dy / 2, { steps: 8 });
  await page.mouse.move(x + dx, y + dy, { steps: 8 });
  await page.mouse.up();
}

test("繰り返し: 中に AI に聞くとデータを追加を足して保存し、リストの 3 件で中が 3 回動く（BUG-103・D-85）", async ({ page }) => {
  test.setTimeout(240_000);
  await page.setViewportSize({ width: 1440, height: 900 });
  await loginViaKeycloak(page);
  cleanup.add(await pinStubOrgDefault(page));
  page.on("dialog", (d) => void d.accept());

  const tableName = uniqueName("e2e-書類の台帳");
  const t = await page.request.post("/api/data/tables", {
    headers: await csrfHeaders(page),
    data: {
      name: tableName,
      schema: {
        fields: [
          { key: "name", display_name: "書類", type: "text" },
          { key: "summary", display_name: "言い換え", type: "text" },
        ],
      },
    },
  });
  expect(t.status(), await t.text()).toBe(201);
  const tableId = ((await t.json()) as { id: string }).id;
  cleanup.add(() => deleteTableApi(page, tableId));
  const id = await createWorkflow(page, []);

  await page.goto(`/workflows/${id}`);
  const panel = page.getByRole("complementary", { name: "ブロックの設定" });
  await page.getByRole("button", { name: "最初のブロック" }).click();
  await page.getByRole("button", { name: /^繰り返し/ }).click();
  const frame = page.locator('[data-frame="map_1"]');
  await expect(frame).toBeVisible();
  await expect(frame.getByRole("button", { name: "「繰り返し」の中にブロックを追加" })).toBeVisible();
  await expect(panel.getByTestId("map-inside-note")).toContainText("「中にブロックを追加」から、このブロックの中に追加します");
  if (SHOTS) await page.screenshot({ path: `${SHOTS}/map-region-empty-pc.png` });

  // 置いた直後（中が空・リストも未選択）でも保存できる。中が空であることは注意で言う（実行の前に止まる）。
  await save(page);
  let ir = await savedIr(page, id);
  expect(ir.nodes.map((n) => n.type)).toEqual(["control.map"]);
  await expect(panel.getByTestId("node-warnings")).toContainText(
    "「繰り返し」の中にブロックがありません。1 件ごとに行う処理を中に置くまで、実行も自動実行のオンもできません。",
  );
  // 中が空のままでは実行できない（実行を作らずに理由を返す）。
  const blocked = await page.request.post(`/api/workflows/${id}/runs`, { headers: await csrfHeaders(page), data: { input: {} } });
  expect(blocked.status()).toBe(400);
  expect(await blocked.text()).toContain("「繰り返し」の中にブロックがありません。");

  // 繰り返すリスト: 3 件の固定のリスト。
  const items = panel.locator('[data-value-expr="繰り返すリスト"]');
  await items.getByRole("combobox", { name: "値の決め方" }).click();
  await page.getByRole("option", { name: "固定値" }).click();
  await items.getByRole("textbox", { name: "繰り返すリスト" }).fill('["請求書", "見積書", "納品書"]');
  await items.getByRole("switch").click();

  // 枠の中に「AI に聞く」を足す。中のブロックの値の候補に「繰り返し中の 1 件」が出る。
  await frame.getByRole("button", { name: "「繰り返し」の中にブロックを追加" }).click();
  await page.getByTestId("add-node-menu").getByRole("button", { name: /^AI に聞く/ }).click();
  const ask = page.getByTestId("rf__node-invoke_1");
  await expect(ask).toBeVisible();
  expect(await inside(ask, frame)).toBe(true);
  const prompt = panel.locator('[data-value-expr="AI への指示"]');
  await prompt.getByRole("textbox", { name: "AI への指示" }).fill("次の書類の名前を、短く言い換えてください: ");
  await prompt.getByRole("button", { name: "差し込む" }).click();
  await page.getByRole("menuitem", { name: "「繰り返し」の繰り返し中の 1 件", exact: true }).click();
  // 差し込みのメニューが閉じて、指示の欄に戻るのを待つ（閉じた後の欄へのフォーカスで、次に開くメニューが閉じないように）。
  await expect(page.getByRole("menu")).toHaveCount(0);
  await expect(prompt.getByRole("textbox", { name: "AI への指示" })).toBeFocused();

  // 「AI に聞く」の後ろ（中）に「データを追加」を足す。
  await ask.getByLabel("次のブロックを追加").click();
  const menu = page.getByTestId("add-node-menu");
  // 繰り返しの中には置けない種類は、理由つきで灰色になる。
  await expect(menu.locator('[data-refused="workflow.reply"]')).toContainText("「繰り返し」の中に置けません");
  await menu.getByRole("searchbox", { name: "ブロックを検索" }).fill("データを追加");
  await menu.getByRole("button", { name: /^データを追加/ }).click();
  const create = page.getByTestId("rf__node-create_1");
  await expect(create).toBeVisible();
  expect(await inside(create, frame)).toBe(true);
  // Undo 1 段で足したブロックが消え、Redo で戻る。
  await page.locator(".react-flow__pane").click({ position: { x: 20, y: 20 } });
  await page.keyboard.press("Control+z");
  await expect(create).toHaveCount(0);
  await page.keyboard.press("Control+Shift+z");
  await expect(create).toBeVisible();
  await create.click();
  // テーブルは ID で指定してから一覧の表示に戻す（手元の開発用の DB は alice のテーブルが一覧の上限を超えていて、
  // 新しいテーブルが名前で探しても出ないことがある。ID で束ねたテーブルは 1 件ずつ取って名前を出す）。
  await panel.getByRole("button", { name: "前の結果から指定する" }).click();
  await panel.getByRole("textbox", { name: "対象のテーブル" }).fill(tableId);
  await panel.getByRole("button", { name: "一覧から選ぶ" }).click();
  await expect(panel.getByRole("combobox", { name: "対象のテーブル" })).toContainText(tableName, { timeout: 30_000 });
  await panel
    .getByRole("textbox", { name: "レコードの値" })
    .fill(
      JSON.stringify({
        name: { $from: "each", path: "/item" },
        summary: { $from: "nodes.invoke_1.output", path: "/text" },
      }),
    );

  await save(page);
  ir = await savedIr(page, id);
  const byId = new Map(ir.nodes.map((n) => [n.id, n]));
  expect(byId.get("invoke_1")?.parent).toBe("map_1");
  expect(byId.get("create_1")?.parent).toBe("map_1");
  expect(ir.edges).toEqual([{ from: "invoke_1", from_port: "out", to: "create_1" }]);
  await expect(panel.getByTestId("node-warnings")).toHaveCount(0);
  await settleViewport(page);
  if (SHOTS) await page.screenshot({ path: `${SHOTS}/map-region-pc.png` });

  // 3 件で実行すると、中のブロックが 3 回ずつ動き、テーブルに 3 件入る。
  const started = await page.request.post(`/api/workflows/${id}/runs`, { headers: await csrfHeaders(page), data: { input: {} } });
  expect(started.status(), await started.text()).toBe(202);
  const runId = ((await started.json()) as { run_id: string }).run_id;
  await expect
    .poll(
      async () => ((await (await page.request.get(`/api/workflows/${id}/runs/${runId}`)).json()) as { status: string }).status,
      { timeout: 90_000 },
    )
    .toBe("succeeded");
  const detail = (await (await page.request.get(`/api/workflows/${id}/runs/${runId}`)).json()) as {
    steps: { step_path: string; status: string }[];
  };
  const ran = (node: string) => detail.steps.filter((s) => s.step_path.endsWith(`].${node}`) && s.status === "succeeded");
  expect(ran("invoke_1")).toHaveLength(3);
  expect(ran("create_1")).toHaveLength(3);
  const records = await page.request.get(`/api/data/tables/${tableId}/records`);
  expect(records.ok(), await records.text()).toBeTruthy();
  const rows = ((await records.json()) as { items: { data: { name?: string; summary?: string } }[] }).items;
  expect(rows.map((r) => r.data.name).sort()).toEqual(["納品書", "見積書", "請求書"].sort());
  for (const r of rows) expect(r.data.summary?.length ?? 0).toBeGreaterThan(0);
});

const SCRIPT = { inline: "function main(input) { return input; }" };

/// 受付 → 1 件ずつ（中に「中の処理」）と、つながっていない「外のブロック」のワークフローを、決まった配置で開く。
async function openRegionFlow(page: Page): Promise<string> {
  const id = await createWorkflow(
    page,
    [
      { id: "a", type: "script.run", label: "受付", params: { source: SCRIPT } },
      { id: "m", type: "control.map", label: "1 件ずつ", params: { items: [1, 2] } },
      { id: "inner", type: "script.run", label: "中の処理", parent: "m", params: { source: SCRIPT } },
      { id: "b", type: "script.run", label: "外のブロック", params: { source: SCRIPT } },
    ],
    [{ from: "a", from_port: "out", to: "m" }],
  );
  const layout = await page.request.put(`/api/workflows/${id}/layout`, {
    headers: await csrfHeaders(page),
    data: {
      layout: {
        positions: { a: { x: 0, y: 0 }, m: { x: 340, y: -20 }, inner: { x: 392, y: 68 }, b: { x: 360, y: 420 } },
        // 枠を描く版で保存した配置（前の版の配置として並べ直さない）。
        frames: 1,
      },
    },
  });
  expect(layout.ok(), await layout.text()).toBeTruthy();
  await page.goto(`/workflows/${id}`);
  await expect(page.locator('[data-frame="m"]')).toBeVisible({ timeout: 15_000 });
  await settleViewport(page);
  return id;
}

test("繰り返しの枠: 枠ごと動かす・入れる・出す・広げる・消す、どれも Undo 1 段で戻り、キーボードで中に足せる（D-85）", async ({ page }) => {
  test.setTimeout(240_000);
  await page.setViewportSize({ width: 1440, height: 900 });
  await loginViaKeycloak(page);
  const id = await openRegionFlow(page);
  const frame = page.locator('[data-frame="m"]');
  const frameNode = page.getByTestId("rf__node-m");
  const inner = page.getByTestId("rf__node-inner");
  const outside = page.getByTestId("rf__node-b");

  // 中のブロックは枠の中に描き、読み上げでは枠の中だと言う。きっかけからの破線は中へ引かず、枠の左の点から引く。
  expect(await inside(inner, frame)).toBe(true);
  await expect(frameNode).toHaveAttribute("aria-label", "「1 件ずつ」の枠。中のブロック 1 個");
  await expect(inner).toHaveAttribute("aria-label", "「中の処理」（「1 件ずつ」の中）");
  await expect(page.locator('[data-testid^="rf__edge-t:"][data-testid$=":inner"]')).toHaveCount(0);
  await expect(page.getByTestId("rf__edge-each:m:inner")).toHaveCount(1);
  if (SHOTS) await page.screenshot({ path: `${SHOTS}/map-frame-ops-pc.png` });

  // 枠ごと動かすと、中のブロックも同じだけ動く。
  const f0 = (await frame.boundingBox())!;
  const i0 = (await inner.boundingBox())!;
  await drag(page, frame, 60, 40);
  await expect.poll(async () => (await frame.boundingBox())!.x - f0.x).toBeGreaterThan(30);
  const f2 = (await frame.boundingBox())!;
  const i1 = (await inner.boundingBox())!;
  expect(Math.abs(i1.x - i0.x - (f2.x - f0.x))).toBeLessThan(1.5);
  expect(Math.abs(i1.y - i0.y - (f2.y - f0.y))).toBeLessThan(1.5);

  // 外のブロックを枠の中で離すと中に入る（線の誤りは注意に出る）。Undo 1 段で外の元の位置へ戻る。
  const b0 = (await outside.boundingBox())!;
  const f1 = (await frame.boundingBox())!;
  const grab = { x: b0.x + Math.min(b0.width / 2, 120), y: b0.y + 24 };
  await drag(page, outside, f1.x + f1.width - 150 - grab.x, f1.y + f1.height - 80 - grab.y);
  await expect(page.getByText("「外のブロック」を「1 件ずつ」の中に入れました。", { exact: true })).toBeVisible();
  await expect(frameNode).toHaveAttribute("aria-label", "「1 件ずつ」の枠。中のブロック 2 個");
  expect(await inside(outside, frame)).toBe(true);
  await expect(frame.getByLabel(/^検証エラー/)).toBeVisible({ timeout: 10_000 });
  await page.keyboard.press("Control+z");
  await expect(frameNode).toHaveAttribute("aria-label", "「1 件ずつ」の枠。中のブロック 1 個");
  await expect.poll(async () => Math.round((await outside.boundingBox())!.y)).toBe(Math.round(b0.y));
  expect(await inside(outside, frame)).toBe(false);

  // 中のブロックを枠の外で離すと外へ出る。中が空になると「中にブロックを追加」を真ん中に出す。Undo で中へ戻る。
  const i2 = (await inner.boundingBox())!;
  await drag(page, inner, 0, 360);
  await expect(page.getByText("「中の処理」を繰り返しの外に出しました。", { exact: true })).toBeVisible();
  await expect(frameNode).toHaveAttribute("aria-label", "「1 件ずつ」の枠。中のブロック 0 個");
  await expect(frame.getByText("1 件ごとに行う処理を、ここに置きます。", { exact: false })).toBeVisible();
  await page.keyboard.press("Control+z");
  await expect(frameNode).toHaveAttribute("aria-label", "「1 件ずつ」の枠。中のブロック 1 個");
  await expect.poll(async () => Math.round((await inner.boundingBox())!.y)).toBe(Math.round(i2.y));

  // 右下の角で枠を広げる（大きさは配置として保存する）。
  const w0 = await frameNode.evaluate((el) => (el as HTMLElement).getBoundingClientRect().width);
  await frame.hover({ position: { x: 24, y: 24 } });
  const corner = frameNode.locator(".react-flow__resize-control");
  const c = (await corner.boundingBox())!;
  const savedLayout = page.waitForResponse(
    (r) => r.request().method() === "PUT" && r.url().endsWith(`/workflows/${id}/layout`),
  );
  await page.mouse.move(c.x + c.width / 2, c.y + c.height / 2);
  await page.mouse.down();
  await page.mouse.move(c.x + c.width / 2 + 120, c.y + c.height / 2 + 60, { steps: 8 });
  await page.mouse.up();
  await expect
    .poll(async () => frameNode.evaluate((el) => (el as HTMLElement).getBoundingClientRect().width))
    .toBeGreaterThan(w0 + 60);
  const sent = (await savedLayout).request().postDataJSON() as { layout: { sizes?: Record<string, { width: number }> } };
  expect(sent.layout.sizes?.m?.width ?? 0).toBeGreaterThan(0);

  // キーボード: 「中にブロックを追加」に移って Enter で開き、探して Enter で中の最後に足す。
  const addInside = frame.getByRole("button", { name: "「1 件ずつ」の中にブロックを追加" });
  await addInside.focus();
  await page.keyboard.press("Enter");
  const menu = page.getByTestId("add-node-menu");
  await expect(menu.getByRole("searchbox", { name: "ブロックを検索" })).toBeFocused();
  await page.keyboard.type("スクリプト");
  await page.keyboard.press("Tab");
  await expect(menu.getByRole("button", { name: /^スクリプト/ })).toBeFocused();
  await page.keyboard.press("Enter");
  await expect(frameNode).toHaveAttribute("aria-label", "「1 件ずつ」の枠。中のブロック 2 個");
  await expect(page.getByTestId("rf__edge-e:inner|out|run_1")).toHaveCount(1);

  // 枠を選んで Delete で消すと、中のブロックも一緒に消える。Undo 1 段で全部戻る。
  await frame.getByText("リストの要素ごとに同じ処理を繰り返す").click();
  await settleViewport(page);
  await page.keyboard.press("Delete");
  await expect(frameNode).toHaveCount(0);
  await expect(inner).toHaveCount(0);
  await expect(page.getByTestId("rf__node-run_1")).toHaveCount(0);
  await page.keyboard.press("Control+z");
  await expect(frameNode).toHaveCount(1);
  await expect(inner).toHaveCount(1);
  await expect(page.getByTestId("rf__node-run_1")).toHaveCount(1);

  await save(page);
  const ir = await savedIr(page, id);
  const parents = Object.fromEntries(ir.nodes.map((n) => [n.id, n.parent ?? null]));
  expect(parents).toEqual({ a: null, m: null, inner: "m", b: null, run_1: "m" });
  expect(ir.edges).toContainEqual({ from: "inner", from_port: "out", to: "run_1" });
});

test("スマホ: 繰り返しの中を字下げした一覧に並べ、中の＋で足せる（D-85）", async ({ browser, page }) => {
  test.setTimeout(180_000);
  await loginViaKeycloak(page);
  const id = await createWorkflow(page, [
    { id: "m", type: "control.map", label: "1 件ずつ", params: { items: ["a", "b"] } },
  ]);
  const phoneCtx = await phoneContext(browser, page);
  try {
    const phone = await phoneCtx.newPage();
    await phone.goto(`/workflows/${id}`);
    const list = phone.getByTestId("workflow-mobile-flow");
    await expect(list).toBeVisible({ timeout: 15_000 });
    await expect(list.getByTestId("mobile-region-m")).toContainText("「1 件ずつ」の中（1 件ずつ実行）");
    if (SHOTS) await phone.screenshot({ path: `${SHOTS}/map-region-phone-empty.png` });

    // 中が空: 中の＋で足す。置けない種類は理由つきで灰色。
    await list.getByRole("button", { name: "「1 件ずつ」の中にブロックを追加" }).click();
    const sheet = phone.getByRole("dialog", { name: "ブロックを追加" });
    await expect(sheet.getByText("「1 件ずつ」の中に追加")).toBeVisible();
    await expect(sheet.locator('[data-refused="workflow.reply"]')).toContainText("「繰り返し」の中に置けません");
    if (SHOTS) {
      await sheet.locator('[data-refused="workflow.reply"]').scrollIntoViewIfNeeded();
      await phone.screenshot({ path: `${SHOTS}/map-region-phone-add.png` });
    }
    await sheet.getByRole("button", { name: /^スクリプト/ }).click();
    // 足したブロックの設定が下から開く（閉じて一覧に戻る）。
    const settings = phone.getByRole("dialog").filter({ hasText: "このブロックを削除" });
    await expect(settings).toBeVisible();
    await phone.keyboard.press("Escape");
    await expect(settings).toHaveCount(0);
    // 足したブロックは繰り返しの下に字下げして並ぶ。その後ろの＋も中に足す。
    const added = list.locator('[data-depth="1"]').getByRole("button", { name: "開発者向け スクリプト" });
    await expect(added).toBeVisible();
    await list.getByRole("button", { name: "「スクリプト」の後ろにブロックを追加" }).click();
    await sheet.getByRole("button", { name: /^スクリプト/ }).click();
    await expect(settings).toBeVisible();
    await phone.keyboard.press("Escape");
    await expect(settings).toHaveCount(0);
    await expect(list.locator('[data-depth="1"]').getByRole("button", { name: "開発者向け スクリプト" })).toHaveCount(2);
    if (SHOTS) await phone.screenshot({ path: `${SHOTS}/map-region-phone.png` });

    // スマホの保存は「その他の操作」の中。
    await phone.getByTestId("chrome-menu").click();
    await phone.getByRole("menuitem", { name: "保存", exact: true }).click();
    await expect(phone.getByText(/保存しました/).first()).toBeVisible({ timeout: 15_000 });
    const ir = await savedIr(page, id);
    const parents = Object.fromEntries(ir.nodes.map((n) => [n.id, n.parent ?? null]));
    expect(parents).toEqual({ m: null, run_1: "m", run_2: "m" });
    expect(ir.edges).toEqual([{ from: "run_1", from_port: "out", to: "run_2" }]);
    // 横にはみ出さない。
    expect(await list.evaluate((el) => el.scrollWidth <= el.clientWidth + 1)).toBe(true);
  } finally {
    await phoneCtx.close();
  }
});

async function phoneContext(browser: Browser, page: Page) {
  return browser.newContext({
    storageState: await page.context().storageState(),
    viewport: { width: 390, height: 844 },
    hasTouch: true,
    isMobile: true,
    deviceScaleFactor: 3,
    ignoreHTTPSErrors: true,
  });
}

test("座標の無い定義（チャットで作ったもの）と、枠の無かった頃の配置を開いても、中のブロックは枠の中に並ぶ（D-85）", async ({ page }) => {
  test.setTimeout(120_000);
  await page.setViewportSize({ width: 1440, height: 900 });
  await loginViaKeycloak(page);
  const nodes = [
    { id: "a", type: "script.run", label: "受付", params: { source: SCRIPT } },
    { id: "m", type: "control.map", label: "1 件ずつ", params: { items: [1, 2] } },
    { id: "x", type: "script.run", label: "中の 1", parent: "m", params: { source: SCRIPT } },
    { id: "y", type: "script.run", label: "中の 2", parent: "m", params: { source: SCRIPT } },
    { id: "c", type: "script.run", label: "後ろ", params: { source: SCRIPT } },
  ];
  const edges = [
    { from: "a", from_port: "out", to: "m" },
    { from: "m", from_port: "out", to: "c" },
    { from: "x", from_port: "out", to: "y" },
  ];
  const frame = page.locator('[data-frame="m"]');
  const check = async () => {
    await expect(frame).toBeVisible({ timeout: 15_000 });
    await settleViewport(page);
    for (const id of ["x", "y"]) expect(await inside(page.getByTestId(`rf__node-${id}`), frame), id).toBe(true);
    for (const id of ["a", "c"]) {
      const box = (await page.getByTestId(`rf__node-${id}`).boundingBox())!;
      const f = (await frame.boundingBox())!;
      const center = { x: box.x + box.width / 2, y: box.y + box.height / 2 };
      const covered = center.x > f.x && center.x < f.x + f.width && center.y > f.y && center.y < f.y + f.height;
      expect(covered, `${id} は枠に重ならない`).toBe(false);
    }
  };

  // 座標の無い定義: 中を先に並べ、外の並びでは繰り返しを枠の大きさで扱う。
  const fresh = await createWorkflow(page, nodes, edges);
  await page.goto(`/workflows/${fresh}`);
  await check();
  if (SHOTS) await page.screenshot({ path: `${SHOTS}/map-region-no-layout-pc.png` });

  // 枠の無かった頃の配置（中のブロックを外と同じ並びに置いた）: 枠が外のブロックを囲むので、中を枠の中へ並べ直す。
  const old = await createWorkflow(page, nodes, edges);
  const put = await page.request.put(`/api/workflows/${old}/layout`, {
    headers: await csrfHeaders(page),
    data: {
      layout: {
        positions: {
          a: { x: 0, y: 0 },
          m: { x: 340, y: 0 },
          c: { x: 680, y: 0 },
          x: { x: 1020, y: 0 },
          y: { x: 1360, y: 0 },
        },
      },
    },
  });
  expect(put.ok(), await put.text()).toBeTruthy();
  await page.goto(`/workflows/${old}`);
  await check();
});
