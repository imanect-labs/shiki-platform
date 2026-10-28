import { expect, test, type Browser, type Page } from "@playwright/test";

import { loginViaKeycloak, uniqueName } from "./helpers";

/// dnd ワークフローエディタの e2e（Task 10.12 DoD）:
/// 新規作成 → ブロック追加（ヘッダ導線＋尻尾プラス）→ ブロックが要する権限が「使う権限」に出る
/// → 削除 → 保存 → 実行 → 実行履歴で succeeded まで。
/// 前提: compose の shiki-server が SHIKI__WORKFLOW__ENABLED=true・LLM は stub。

test("エディタ: 作成→追加→使う権限→削除→保存→実行→履歴成功", async ({ page }) => {
  test.setTimeout(180_000);
  await loginViaKeycloak(page);

  // 一覧から新規作成 → エディタへ。
  await page.goto("/workflows");
  // 一覧の読み込みを待ってから押す（先行テストが作った同名の行・削除ボタンと区別するため
  // exact 一致にする。待たないと一覧が出る前に押せてしまい、行の有無で結果が揺れる）。
  await expect(page.getByText("読み込み中…")).toHaveCount(0);
  await page.getByRole("button", { name: "新しいワークフロー", exact: true }).click();
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

  // ライブ検証（600ms debounce）が、ブロックが要する権限（declared_scopes）を補完して返し、「使う権限」に出る
  // （宣言を手で書かせない・不足のエラーにしない・C-P1-2）。
  const scopes = page.getByTestId("workflow-scopes");
  await expect(scopes).toContainText("ファイルを保存する", { timeout: 10_000 });
  await expect(canvas.locator('[aria-label^="検証エラー"]')).toHaveCount(0);

  // ブロックを設定パネルから削除 → 使う権限からも外れる。
  await canvas.getByText("ファイルを保存").first().click();
  await page.getByRole("button", { name: "このブロックを削除" }).click();
  await expect(scopes).not.toContainText("ファイルを保存する", { timeout: 10_000 });

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
  await page
    .getByRole("complementary", { name: "ブロックの設定" })
    .getByRole("textbox", { name: "ブロック名" })
    .fill("タグの判定（確認済み）");
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

// ── キャンバスの操作（BUG-2・BUG-9）─────────────────────────────────────
// AI が作ったワークフローと同じ「layout の無い」受付 → 確認 → 通知の 3 ブロックで確かめる。

/// layout を作らずに 3 ブロックのワークフローを作り、エディタで開く。
async function openChainWithoutLayout(page: Page): Promise<string> {
  const csrf = (await page.context().cookies()).find((c) => c.name === "shiki_csrf")?.value ?? "";
  const script = (id: string, label: string) => ({
    id,
    type: "script.run",
    label,
    params: { source: { inline: "function main(input) { return input; }" } },
  });
  const res = await page.request.post("/api/workflows", {
    headers: { "x-csrf-token": csrf, "content-type": "application/json" },
    data: {
      ir: {
        ir_version: 1,
        name: uniqueName("canvas-e2e"),
        display_name: "キャンバスの操作",
        triggers: [{ kind: "interactive" }],
        nodes: [script("a", "受付"), script("b", "確認"), script("c", "通知")],
        edges: [
          { from: "a", from_port: "out", to: "b" },
          { from: "b", from_port: "out", to: "c" },
        ],
      },
    },
  });
  expect(res.ok(), await res.text()).toBeTruthy();
  const id = ((await res.json()) as { id: string }).id;
  await page.goto(`/workflows/${id}`);
  await expect(page.getByTestId("rf__node-c")).toBeVisible({ timeout: 15_000 });
  await settleViewport(page);
  return id;
}

/// キャンバス上の座標（React Flow がノードの transform に書く・表示の拡大率に依らない）。
async function flowPos(page: Page, testId: string): Promise<{ x: number; y: number }> {
  const t = await page.getByTestId(testId).evaluate((el) => (el as HTMLElement).style.transform);
  const m = /translate\(([-\d.]+)px,\s*([-\d.]+)px\)/.exec(t);
  expect(m, `transform: ${t}`).not.toBeNull();
  return { x: Number(m![1]), y: Number(m![2]) };
}

/// 直前の操作の再描画を待つ（ポインタの移動は React が少し後で描画する）。
async function nextFrame(page: Page) {
  await page.evaluate(
    () => new Promise((resolve) => requestAnimationFrame(() => requestAnimationFrame(resolve))),
  );
}

/// 表示の移動・拡大（クリックでのズーム 450ms など）が止まるまで待つ。
async function settleViewport(page: Page) {
  const read = () =>
    page.locator(".react-flow__viewport").evaluate((el) => (el as HTMLElement).style.transform);
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

/// つながりの線をクリックする。中点の＋や尻尾の＋に重ならない、線の上でいちばん広く空いた所を押す。
async function clickEdge(page: Page, testId: string) {
  const pt = await page.getByTestId(testId).evaluate((g) => {
    const path = g.querySelector("path") as SVGPathElement;
    const len = path.getTotalLength();
    const m = path.getScreenCTM()!;
    let best: { x: number; y: number }[] = [];
    let run: { x: number; y: number }[] = [];
    for (let i = 1; i <= 60; i += 1) {
      const p = path.getPointAtLength((len * i) / 60);
      const x = p.x * m.a + m.e;
      const y = p.y * m.d + m.f;
      if (i < 60 && g.contains(document.elementFromPoint(x, y))) {
        run.push({ x, y });
      } else {
        if (run.length > best.length) best = run;
        run = [];
      }
    }
    return best.length > 0 ? best[Math.floor(best.length / 2)] : null;
  });
  expect(pt, "つながりの線にクリックできる所がない").not.toBeNull();
  await page.mouse.click(pt!.x, pt!.y);
}

test("キャンバス: ドラッグ中もブロックがポインタに追従し、離しても他のブロックは動かない", async ({ page }) => {
  test.setTimeout(120_000);
  await loginViaKeycloak(page);
  const layoutPuts: string[] = [];
  page.on("request", (r) => {
    if (r.method() === "PUT" && /\/workflows\/[^/]+\/layout$/.test(r.url())) layoutPuts.push(r.url());
  });
  const id = await openChainWithoutLayout(page);
  const trigger = await page.locator(".react-flow__node-trigger").getAttribute("data-id");
  const others = ["rf__node-a", "rf__node-c", `rf__node-${trigger}`];
  const before = new Map<string, { x: number; y: number }>();
  for (const t of [...others, "rf__node-b"]) before.set(t, await flowPos(page, t));
  // 開いただけでは座標を保存しない（何かを動かしたときに全座標が保存される）。
  expect(layoutPuts).toHaveLength(0);

  // 「確認」を掴んで下へ少しずつ動かす。途中で止めた位置にブロックが付いてくる（離した瞬間に飛ばない）。
  const box = (await page.getByTestId("rf__node-b").boundingBox())!;
  const cx = box.x + box.width / 2;
  const cy = box.y + box.height / 2;
  await page.mouse.move(cx, cy);
  await page.mouse.down();
  await page.mouse.move(cx + 20, cy + 60, { steps: 6 });
  await nextFrame(page);
  const mid1 = await flowPos(page, "rf__node-b");
  await page.mouse.move(cx + 40, cy + 120, { steps: 6 });
  await nextFrame(page);
  const mid2 = await flowPos(page, "rf__node-b");
  expect(mid1.y).toBeGreaterThan(before.get("rf__node-b")!.y + 10);
  expect(mid2.y).toBeGreaterThan(mid1.y + 10);
  // ドラッグ中もほかのブロックは動かない。
  for (const t of others) expect(await flowPos(page, t)).toEqual(before.get(t));
  const saved = page.waitForResponse(
    (r) => r.request().method() === "PUT" && r.url().endsWith(`/workflows/${id}/layout`),
  );
  await page.mouse.up();
  await nextFrame(page);

  // 離した位置はドラッグの最後の位置のまま。座標の無いワークフローでも全体が平行移動しない。
  expect(await flowPos(page, "rf__node-b")).toEqual(mid2);
  for (const t of others) expect(await flowPos(page, t)).toEqual(before.get(t));

  // 自動保存（1 秒後）の後に開き直しても、同じ配置で出る。
  expect((await saved).ok()).toBeTruthy();
  await page.reload();
  await expect(page.getByTestId("rf__node-b")).toBeVisible({ timeout: 15_000 });
  const reloaded = await flowPos(page, "rf__node-b");
  expect(reloaded.x).toBeCloseTo(mid2.x, 1);
  expect(reloaded.y).toBeCloseTo(mid2.y, 1);
  for (const t of others) {
    const p = await flowPos(page, t);
    expect(p.x).toBeCloseTo(before.get(t)!.x, 1);
    expect(p.y).toBeCloseTo(before.get(t)!.y, 1);
  }
});

test("キャンバス: つながりをクリックして選び、Delete でそのつながりだけを消す", async ({ page }) => {
  test.setTimeout(120_000);
  await loginViaKeycloak(page);
  await openChainWithoutLayout(page);
  const edge = page.getByTestId("rf__edge-e:a|out|b");

  // 先にブロックを選んでおく（つながりを選んだらブロックの選択は外れ、Delete で消えないこと）。
  await page.getByTestId("rf__node-a").click();
  await expect(page.getByRole("complementary", { name: "ブロックの設定" })).toBeVisible();
  await settleViewport(page);
  await clickEdge(page, "rf__edge-e:a|out|b");
  await expect(edge).toHaveClass(/\bselected\b/);
  await expect(page.getByTestId("rf__node-a")).not.toHaveClass(/\bselected\b/);
  await expect(page.getByRole("complementary", { name: "ブロックの設定" })).toHaveCount(0);
  await expect(page.getByRole("button", { name: "このつながりを削除" })).toBeVisible();

  await page.keyboard.press("Delete");
  await expect(edge).toHaveCount(0);
  for (const id of ["a", "b", "c"]) await expect(page.getByTestId(`rf__node-${id}`)).toBeVisible();
  await expect(page.getByTestId("rf__edge-e:b|out|c")).toHaveCount(1);

  // Undo 1 回で戻る。
  await page.keyboard.press("Control+z");
  await expect(edge).toHaveCount(1);
});

test("キャンバス: ブロックを Delete で消し、Undo 1 回でつながりごと同じ位置に戻す", async ({ page }) => {
  test.setTimeout(120_000);
  await loginViaKeycloak(page);
  await openChainWithoutLayout(page);
  const before = await flowPos(page, "rf__node-b");

  await page.getByTestId("rf__node-b").click();
  await expect(page.getByRole("complementary", { name: "ブロックの設定" })).toBeVisible();
  await page.keyboard.press("Delete");
  await expect(page.getByTestId("rf__node-b")).toHaveCount(0);
  await expect(page.locator('[data-testid^="rf__edge-e:"]')).toHaveCount(0);

  await page.keyboard.press("Control+z");
  await expect(page.getByTestId("rf__node-b")).toBeVisible();
  await expect(page.getByTestId("rf__edge-e:a|out|b")).toHaveCount(1);
  await expect(page.getByTestId("rf__edge-e:b|out|c")).toHaveCount(1);
  expect(await flowPos(page, "rf__node-b")).toEqual(before);
});

test("設定パネル: ブロック名の連続入力は Undo 1 回で入力前に戻る", async ({ page }) => {
  test.setTimeout(120_000);
  await loginViaKeycloak(page);
  await openChainWithoutLayout(page);
  await page.getByTestId("rf__node-a").click();
  const panel = page.getByRole("complementary", { name: "ブロックの設定" });
  const name = panel.getByRole("textbox", { name: "ブロック名" });
  await name.click();
  await name.press("End");
  await name.pressSequentially("の一次確認です");
  await expect(page.getByTestId("rf__node-a")).toContainText("受付の一次確認です");

  // 入力欄の外（キャンバスの余白）へ移ってから Undo する（入力欄の中ではブラウザの取り消しが働く）。
  await page.locator(".react-flow__pane").click({ position: { x: 24, y: 24 } });
  await page.keyboard.press("Control+z");
  await expect(page.getByTestId("rf__node-a")).toContainText("受付");
  await expect(page.getByTestId("rf__node-a")).not.toContainText("一次確認");
});

// ── ノードの設定フォーム（BUG-5・BUG-15・BUG-16・BUG-21）─────────────────

/// ドライブの選択ダイアログの先頭に並ぶ名前（一覧は名前の昇順で 50 件ずつ。開発 DB に過去の実行の
/// ファイルが溜まっても先頭に来るよう、時刻を逆順にした固定長の鍵を頭に付ける）。
function firstInDriveName(prefix: string): string {
  const key = (Number.MAX_SAFE_INTEGER - Date.now()).toString(36).padStart(11, "0");
  return `00-${key}-${prefix}-${Math.floor(Math.random() * 1e4)}`;
}

test("CSV を集計: ドライブから CSV を選び、SQL を書いて保存できる（BUG-5・BUG-21）", async ({ page }) => {
  test.setTimeout(180_000);
  await loginViaKeycloak(page);
  const csrf = (await page.context().cookies()).find((c) => c.name === "shiki_csrf")?.value ?? "";
  const fileName = `${firstInDriveName("wf-csv")}.csv`;
  const created = await page.request.post("/api/tabular/save", {
    headers: { "x-csrf-token": csrf },
    data: { parent_id: null, name: fileName, csv: "item,amount\n交通費,1200\n書籍,3000\n交通費,800\n" },
  });
  expect(created.ok(), await created.text()).toBeTruthy();
  const fileId = ((await created.json()) as { node_id: string }).node_id;

  await page.goto("/workflows");
  await expect(page.getByText("読み込み中…")).toHaveCount(0);
  await page.getByRole("button", { name: "新しいワークフロー", exact: true }).click();
  await page.waitForURL(/\/workflows\/[0-9a-f-]+$/i, { timeout: 20_000 });
  const workflowId = new URL(page.url()).pathname.split("/").pop()!;

  // 置いた直後から設定の欄が出る（「このブロックに設定はありません」ではない）。
  await page.getByRole("button", { name: "最初のブロック" }).click();
  await page.getByRole("button", { name: /^CSV を集計/ }).click();
  const panel = page.getByRole("complementary", { name: "ブロックの設定" });
  await expect(panel.getByLabel("SQL")).toHaveValue("SELECT * FROM data LIMIT 100");
  await expect(panel.getByText("このブロックに設定はありません")).toHaveCount(0);
  // ファイルが未選択の誤り（サーバは「項目 'file' がありません」）は、上の帯ではなくファイルの欄の下に出る。
  await expect(panel.getByText("入力してください")).toBeVisible({ timeout: 10_000 });
  await expect(panel.getByText(/項目 'file'/)).toHaveCount(0);

  // ドライブから CSV を選ぶと、ファイル名と列の案内が出る（誤りも消える）。
  await panel.getByRole("button", { name: "集計する CSV ファイルを選ぶ" }).click();
  const picker = page.getByRole("dialog", { name: "CSV ファイルを選ぶ" });
  await picker.getByRole("button", { name: fileName }).click();
  await expect(picker).toHaveCount(0);
  await expect(panel.getByText(fileName)).toBeVisible();
  const columns = panel.getByTestId("csv-columns");
  await expect(columns).toContainText("item", { timeout: 20_000 });
  await expect(columns).toContainText("amount");
  await expect(panel.getByText("入力してください")).toHaveCount(0, { timeout: 10_000 });

  // SQL を書いて試しに実行すると、本人の権限で上位の行が見える。
  const sql = "SELECT item, SUM(amount) AS total FROM data GROUP BY item ORDER BY item";
  await panel.getByLabel("SQL").fill(sql);
  await panel.getByRole("button", { name: "試しに実行" }).click();
  const preview = panel.getByTestId("csv-preview");
  await expect(preview).toContainText("total", { timeout: 20_000 });
  await expect(preview).toContainText("交通費");
  await expect(preview).toContainText("2000");

  // 保存 API が 200 を返し、保存した定義にファイルと SQL が入っている。
  const saved = page.waitForResponse(
    (r) => r.request().method() === "PUT" && r.url().endsWith(`/workflows/${workflowId}`),
  );
  await page.getByRole("button", { name: "保存", exact: true }).click();
  expect((await saved).status()).toBe(200);
  await expect(page.getByText(/保存しました/).first()).toBeVisible({ timeout: 15_000 });
  const wf = (await (await page.request.get(`/api/workflows/${workflowId}`)).json()) as {
    ir: { nodes: { type: string; params: unknown }[] };
  };
  expect(wf.ir.nodes.find((n) => n.type === "csv.query")?.params).toEqual({ file: fileId, sql });

  // 開き直しても ID ではなくファイル名で出る。
  await page.reload();
  await page.locator(".react-flow").getByText("CSV を集計").first().click();
  await expect(page.getByRole("complementary", { name: "ブロックの設定" }).getByText(fileName)).toBeVisible({
    timeout: 15_000,
  });
});

test("既定の値: 追加した条件は入力全体を path なしで指し、AI に聞くの指示は空で入力を促す（BUG-16・BUG-15）", async ({
  page,
}) => {
  test.setTimeout(120_000);
  await loginViaKeycloak(page);
  await page.goto("/workflows");
  await expect(page.getByText("読み込み中…")).toHaveCount(0);
  await page.getByRole("button", { name: "新しいワークフロー", exact: true }).click();
  await page.waitForURL(/\/workflows\/[0-9a-f-]+$/i, { timeout: 20_000 });
  const workflowId = new URL(page.url()).pathname.split("/").pop()!;
  const canvas = page.locator(".react-flow");
  const panel = page.getByRole("complementary", { name: "ブロックの設定" });

  // 条件分岐に条件を足すと、左辺は入力全体（path なし）になる（"/" は空文字キーを指す）。
  await page.getByRole("button", { name: "最初のブロック" }).click();
  await page.getByRole("button", { name: /^条件分岐/ }).click();
  await panel.getByRole("button", { name: "条件を追加" }).click();
  await page.getByRole("button", { name: "保存", exact: true }).click();
  await expect(page.getByText(/保存しました/).first()).toBeVisible({ timeout: 15_000 });
  const wf = (await (await page.request.get(`/api/workflows/${workflowId}`)).json()) as {
    ir: { nodes: { type: string; params: Record<string, unknown> }[] };
  };
  const condition = wf.ir.nodes.find((n) => n.type === "control.branch")?.params.condition as {
    all: { cmp: { left: unknown } }[];
  };
  expect(condition.all).toHaveLength(2);
  expect(condition.all[1].cmp.left).toEqual({ $from: "input" });

  // 「AI に聞く」は指示が空のまま置かれ（入力全体のオブジェクトを指さない）、欄の下で入力を促す。
  // （モデルの照合は環境で変わるので、ここでは保存しない。）
  await canvas.getByLabel("次のブロックを追加").first().click();
  await page.getByRole("button", { name: /^AI に聞く/ }).click();
  await expect(panel.getByText("入力してください")).toBeVisible();
  await panel.getByRole("textbox", { name: "AI への指示", exact: true }).fill("今日の予定を 3 行でまとめてください");
  await expect(panel.getByText("入力してください")).toHaveCount(0);
});

// ── 差し込み・入力項目・モデルの選択（BUG-1・GAP-12・BUG-4）───────────────────

type LlmModel = { id: string; label: string };
type LlmModels = { models: LlmModel[] } & Record<string, unknown>;

/// ワークフローで使えるモデルの一覧（`/llm/models?purpose=workflow`・モデル欄と同じ）。LLM が構成されて
/// いなければ null。
async function llmModels(page: Page): Promise<LlmModels | null> {
  const res = await page.request.get("/api/llm/models?purpose=workflow");
  return res.ok() ? ((await res.json()) as LlmModels) : null;
}

/// モデル欄が読む一覧の URL（`?purpose=workflow` を含めて当てる。glob は query まで一致を求める）。
const isLlmModels = (url: URL) => url.pathname.endsWith("/api/llm/models");

/// 「要約する」（AI に聞く）1 つだけのワークフローを作る（model は渡したときだけ書く。省略は組織の既定）。
async function createLlmFlow(
  page: Page,
  opts: { prompt?: unknown; model?: string | null; inputSchema?: unknown },
): Promise<string> {
  const csrf = (await page.context().cookies()).find((c) => c.name === "shiki_csrf")?.value ?? "";
  const name = uniqueName("wf-vars");
  const res = await page.request.post("/api/workflows", {
    headers: { "x-csrf-token": csrf, "content-type": "application/json" },
    data: {
      ir: {
        ir_version: 1,
        name,
        display_name: name,
        triggers: [{ kind: "interactive" }],
        input_schema: opts.inputSchema ?? null,
        nodes: [
          {
            id: "ask",
            type: "llm.invoke",
            label: "要約する",
            params: { prompt: opts.prompt ?? "", ...(opts.model ? { model: opts.model } : {}) },
          },
        ],
        edges: [],
      },
    },
  });
  expect(res.ok(), await res.text()).toBeTruthy();
  return ((await res.json()) as { id: string }).id;
}

async function openFlow(page: Page, id: string) {
  await page.goto(`/workflows/${id}`);
  await expect(page.locator(".react-flow").getByText("要約する").first()).toBeVisible({ timeout: 15_000 });
}

async function saveFlow(page: Page) {
  await page.getByRole("button", { name: "保存", exact: true }).click();
  await expect(page.getByText(/保存しました/).first()).toBeVisible({ timeout: 15_000 });
}

type SavedIr = { input_schema: unknown; nodes: { id: string; params: Record<string, unknown> }[] };

async function savedIr(page: Page, id: string): Promise<SavedIr> {
  return ((await (await page.request.get(`/api/workflows/${id}`)).json()) as { ir: SavedIr }).ir;
}

test("差し込む: 入力項目を足し、AI に聞くの指示に差し込んで保存すると $template と vars になる（BUG-1・GAP-12）", async ({
  page,
}) => {
  test.setTimeout(120_000);
  await loginViaKeycloak(page);
  const models = await llmModels(page);
  const id = await createLlmFlow(page, { model: models?.models[0]?.id });
  await openFlow(page, id);
  const canvas = page.locator(".react-flow");

  // きっかけ（手動で実行）の設定で、入力項目「件名（title・必須）」を足す。
  await canvas.getByText("手動で実行").first().click();
  const triggerPanel = page.getByRole("complementary", { name: "きっかけの設定" });
  await triggerPanel.getByRole("button", { name: "項目を追加" }).click();
  const row = triggerPanel.getByTestId("input-field-row");
  // 表示名は 1 文字ずつ打つ（語の間の空白が打てる・前後の空白は欄を離れたときに落ちる）。
  await row.getByLabel("表示名").pressSequentially("件名 A ");
  await expect(row.getByLabel("表示名")).toHaveValue("件名 A ");
  await row.getByLabel("キー").fill("title");
  await expect(row.getByLabel("表示名")).toHaveValue("件名 A");
  await row.getByRole("switch", { name: "必須" }).click();

  // 指示を書き、末尾に「差し込む」から件名を入れる（固定値から文章の組み立てに切り替わる）。
  await canvas.getByText("要約する").first().click();
  const panel = page.getByRole("complementary", { name: "ブロックの設定" });
  const prompt = panel.locator('[data-value-expr="AI への指示"]');
  await prompt.getByLabel("AI への指示").fill("次の件名を 1 行で要約してください: ");
  await prompt.getByRole("button", { name: "差し込む" }).click();
  await page.getByRole("menuitem", { name: "件名 A（title）" }).click();
  await expect(prompt.getByLabel("AI への指示")).toHaveValue("次の件名を 1 行で要約してください: {title}");
  await expect(prompt.getByRole("combobox").filter({ hasText: "文章を組み立て" })).toBeVisible();
  await expect(prompt.getByTestId("template-var").getByLabel("差し込み名")).toHaveValue("title");
  await expect(prompt.getByRole("combobox", { name: "「title」に入る値" })).toContainText("件名 A（title）");

  // 保存した定義: 入力項目は JSON Schema、指示は $template と vars（入力の /title を指す）。
  await saveFlow(page);
  const ir = await savedIr(page, id);
  expect(ir.input_schema).toEqual({
    type: "object",
    properties: { title: { type: "string", title: "件名 A" } },
    required: ["title"],
    "x-order": ["title"],
  });
  expect(ir.nodes[0].params.prompt).toEqual({
    $template: "次の件名を 1 行で要約してください: {title}",
    vars: { title: { $from: "input", path: "/title" } },
  });
});

test("差し込む: 固定値に {{title}} と書くとその場で知らせ、変換ボタンで文章の組み立てにする（BUG-1）", async ({
  page,
}) => {
  test.setTimeout(120_000);
  await loginViaKeycloak(page);
  const models = await llmModels(page);
  const id = await createLlmFlow(page, {
    model: models?.models[0]?.id,
    inputSchema: { type: "object", properties: { title: { type: "string", title: "件名" } } },
  });
  await openFlow(page, id);
  await page.locator(".react-flow").getByText("要約する").first().click();
  const panel = page.getByRole("complementary", { name: "ブロックの設定" });
  const prompt = panel.locator('[data-value-expr="AI への指示"]');

  // n8n・Dify の書き方（{{title}}）は差し込みにならず、文字のまま送られる。
  await prompt.getByLabel("AI への指示").fill("{{title}} を要約してください");
  const warning = prompt.getByTestId("literal-var-warning");
  await expect(warning).toContainText("「{{title}}」は差し込みになりません");
  await warning.getByRole("button", { name: "差し込みに変換する" }).click();
  await expect(warning).toHaveCount(0);
  await expect(prompt.getByLabel("AI への指示")).toHaveValue("{title} を要約してください");
  await expect(prompt.getByRole("combobox").filter({ hasText: "文章を組み立て" })).toBeVisible();
  await expect(prompt.getByRole("combobox", { name: "「title」に入る値" })).toContainText("件名（title）");

  await saveFlow(page);
  const ir = await savedIr(page, id);
  expect(ir.nodes[0].params.prompt).toEqual({
    $template: "{title} を要約してください",
    vars: { title: { $from: "input", path: "/title" } },
  });
});

test("差し込む: 文字を求める欄だけに出し、CSV を保存の内容は入力全体ではなく空で置く（BUG-1・BUG-15）", async ({
  page,
}) => {
  test.setTimeout(120_000);
  await loginViaKeycloak(page);
  await page.goto("/workflows");
  await expect(page.getByText("読み込み中…")).toHaveCount(0);
  await page.getByRole("button", { name: "新しいワークフロー", exact: true }).click();
  await page.waitForURL(/\/workflows\/[0-9a-f-]+$/i, { timeout: 20_000 });
  const canvas = page.locator(".react-flow");
  const panel = page.getByRole("complementary", { name: "ブロックの設定" });
  const insertIn = (label: string) =>
    panel.locator(`[data-value-expr="${label}"]`).getByRole("button", { name: "差し込む" });

  // 実行するワークフローは一覧から選ぶ（BUG-107）。「入力や結果から選ぶ」にすると、名前は文字の欄なので
  // 差し込める。渡す入力項目（オブジェクト）は差し込むと文字列に変わるので出さない（参照は「前の結果から」で
  // $from のまま入れる）。
  await page.getByRole("button", { name: "最初のブロック" }).click();
  await page.getByRole("button", { name: /^別のフローを開始/ }).click();
  await expect(panel.getByTestId("workflow-start-select")).toBeVisible();
  await panel.getByRole("button", { name: "入力や結果から選ぶ" }).click();
  await expect(insertIn("実行するワークフロー")).toHaveCount(1);
  await expect(panel.locator('[data-value-expr="渡す入力項目（省略可）"]')).toBeVisible();
  await expect(insertIn("渡す入力項目（省略可）")).toHaveCount(0);

  // 「CSV を保存」の内容は空の固定値で置かれ（入力全体を JSON のまま .csv にしない）、欄の下で入力を促す。
  await canvas.getByLabel("次のブロックを追加").first().click();
  await page.getByRole("button", { name: /^CSV を保存/ }).click();
  const content = panel.locator('[data-value-expr="CSV の内容"]');
  await expect(content.getByRole("combobox").filter({ hasText: "固定値" })).toBeVisible();
  await expect(content.getByLabel("CSV の内容")).toHaveValue("");
  await expect(content.getByText("入力してください")).toBeVisible();
  await expect(insertIn("CSV の内容")).toHaveCount(1);
  await content.getByLabel("CSV の内容").fill("品目,金額\n交通費,1200");
  await expect(content.getByText("入力してください")).toHaveCount(0);
});

test("結果を画面に返す: 形・見出し・色合い・返す内容を設定して保存でき、表にすると列を選べる（GAP-09）", async ({
  page,
}) => {
  test.setTimeout(120_000);
  await loginViaKeycloak(page);
  const id = await createLlmFlow(page, { prompt: "次の文章を要約してください" });
  await openFlow(page, id);
  const canvas = page.locator(".react-flow");
  const panel = page.getByRole("complementary", { name: "ブロックの設定" });

  // 尻尾のプラスから「結果を画面に返す」を足す。既定は文章で、返す内容は空なら入力を促す。
  await canvas.getByLabel("次のブロックを追加").first().click();
  await page.getByRole("button", { name: /^結果を画面に返す/ }).click();
  await expect(canvas.getByText("結果を画面に返す").first()).toBeVisible();
  const content = panel.locator('[data-value-expr="返す内容"]');
  await expect(content.getByText("入力してください")).toBeVisible();
  await expect(panel.getByTestId("reply-visibility-note")).toContainText("起動した本人の画面だけ");
  await panel.getByLabel("見出し（省略可）").fill("受付");
  await panel.getByRole("combobox", { name: "色合い" }).click();
  await page.getByRole("option", { name: "うまくいった（緑）" }).click();
  await content.getByLabel("返す内容").fill("受け付けました");
  await saveFlow(page);
  const reply = (await savedIr(page, id)).nodes.find((n) => n.id.startsWith("reply"));
  expect(reply?.params).toEqual({ format: "text", label: "受付", tone: "success", value: "受け付けました" });

  // 表にすると列（見出しと位置）を足せる。文章に戻すと列は外れる。
  await panel.getByRole("combobox", { name: "形" }).click();
  await page.getByRole("option", { name: "表" }).click();
  await panel.getByRole("button", { name: "列を足す" }).click();
  await panel.getByLabel("1 列目の見出し").fill("条項");
  await panel.getByLabel("1 列目の位置").fill("/clause");
  await expect(panel.getByLabel("1 列目の位置")).toHaveValue("/clause");
  await panel.getByRole("combobox", { name: "形" }).click();
  await page.getByRole("option", { name: "文章" }).click();
  await expect(panel.getByRole("button", { name: "列を足す" })).toHaveCount(0);
});

test("入力項目: キーを変えるとブロックの参照も新しいキーになり、消すと残った参照を知らせる（GAP-12）", async ({
  page,
}) => {
  test.setTimeout(120_000);
  await loginViaKeycloak(page);
  const models = await llmModels(page);
  const id = await createLlmFlow(page, {
    model: models?.models[0]?.id,
    inputSchema: {
      type: "object",
      properties: { title: { type: "string", title: "件名" }, body: { type: "string", title: "本文" } },
    },
    prompt: { $template: "{title} を要約してください", vars: { title: { $from: "input", path: "/title" } } },
  });
  await openFlow(page, id);
  const canvas = page.locator(".react-flow");
  const triggerPanel = page.getByRole("complementary", { name: "きっかけの設定" });
  const panel = page.getByRole("complementary", { name: "ブロックの設定" });

  // 件名のキーを打ち直す（いったん空にすると誤りの間は定義に書き戻さず、打ち終えたキーへ参照が付いてくる）。
  await canvas.getByText("手動で実行").first().click();
  const titleRow = triggerPanel
    .getByTestId("input-field-row")
    .filter({ has: page.getByRole("button", { name: "入力項目「件名」を削除" }) });
  const key = titleRow.getByLabel("キー", { exact: true });
  await key.fill("");
  await expect(titleRow.getByText("キーを入れてください")).toBeVisible();
  await key.pressSequentially("subject");
  await expect(key).toHaveValue("subject");
  await expect(triggerPanel.getByTestId("input-dangling-refs")).toHaveCount(0);

  // ブロックの差し込みは新しいキーを指す（定義に無いキーの注意は出ない）。
  await canvas.getByText("要約する").first().click();
  await expect(panel.getByRole("combobox", { name: "「title」に入る値" })).toContainText("件名（subject）");
  await expect(panel.getByTestId("ref-missing-input")).toHaveCount(0);
  await saveFlow(page);
  const ir = await savedIr(page, id);
  expect(Object.keys((ir.input_schema as { properties: Record<string, unknown> }).properties).sort()).toEqual([
    "body",
    "subject",
  ]);
  expect(ir.nodes[0].params.prompt).toEqual({
    $template: "{title} を要約してください",
    vars: { title: { $from: "input", path: "/subject" } },
  });

  // 項目を消すと、参照が残っているブロックをきっかけの設定とブロックの設定の両方で知らせる。
  await canvas.getByText("手動で実行").first().click();
  await triggerPanel.getByRole("button", { name: "入力項目「件名」を削除" }).click();
  const dangling = triggerPanel.getByTestId("input-dangling-refs");
  await expect(dangling).toContainText("subject");
  await expect(dangling).toContainText("「要約する」");
  await canvas.getByText("要約する").first().click();
  await expect(panel.getByTestId("ref-missing-input")).toContainText("入力項目に「subject」がありません");

  // 「項目に足す」で同じキーの項目を戻すと、注意は消える。
  await canvas.getByText("手動で実行").first().click();
  await dangling.getByRole("button", { name: "項目に足す" }).click();
  await expect(dangling).toHaveCount(0);
  await expect(triggerPanel.getByTestId("input-field-row")).toHaveCount(2);
  await expect(triggerPanel.getByLabel("キー", { exact: true }).nth(1)).toHaveValue("subject");
});

/// 作ったワークフローを片付ける（実行はしないので run は残らない）。
async function deleteFlow(page: Page, id: string) {
  const csrf = (await page.context().cookies()).find((c) => c.name === "shiki_csrf")?.value ?? "";
  const res = await page.request.delete(`/api/artifacts/${id}`, { headers: { "x-csrf-token": csrf } });
  expect(res.status(), await res.text()).toBeLessThan(300);
}

test("入力項目: 並びは保存して開き直しても変わらず、上下で並べ替えた順が実行のダイアログにも出る（BUG-34・D-60）", async ({
  page,
}) => {
  test.setTimeout(150_000);
  await loginViaKeycloak(page);
  const models = await llmModels(page);
  const id = await createLlmFlow(page, { model: models?.models[0]?.id, prompt: "次の申請を要約してください" });
  // 途中で落ちても作ったワークフローを残さない（BUG-63・66）。
  try {
    await openFlow(page, id);
    const canvas = page.locator(".react-flow");
    const triggerPanel = page.getByRole("complementary", { name: "きっかけの設定" });
    const shownOrder = () =>
      triggerPanel
        .getByTestId("input-field-row")
        .getByLabel("表示名")
        .evaluateAll((els) => els.map((e) => (e as HTMLInputElement).value));
    const reopen = async () => {
      await page.reload();
      await expect(canvas.getByText("要約する").first()).toBeVisible({ timeout: 15_000 });
      await canvas.getByText("手動で実行").first().click();
    };

    // BUG-34 の再現手順: キーの辞書順（amount・category・title）と違う順に 3 つ足して保存し、開き直す。
    await canvas.getByText("手動で実行").first().click();
    for (const [title, key] of [
      ["件名", "title"],
      ["金額", "amount"],
      ["区分", "category"],
    ]) {
      await triggerPanel.getByRole("button", { name: "項目を追加" }).click();
      const row = triggerPanel.getByTestId("input-field-row").last();
      await row.getByLabel("表示名").fill(title);
      await row.getByLabel("キー", { exact: true }).fill(key);
    }
    await expect.poll(shownOrder).toEqual(["件名", "金額", "区分"]);
    await saveFlow(page);
    expect((await savedIr(page, id)).input_schema).toMatchObject({ "x-order": ["title", "amount", "category"] });
    await reopen();
    await expect.poll(shownOrder).toEqual(["件名", "金額", "区分"]);

    // 上下のボタンで並べ替える。フォーカスは動かした項目のボタンに付いていき、続けてキーボードで動かせる
    // （先頭まで来て「上へ」が押せなくなったら「下へ」に移る）。
    await triggerPanel.getByRole("button", { name: "入力項目「区分」を上へ" }).click();
    await expect.poll(shownOrder).toEqual(["件名", "区分", "金額"]);
    await expect(triggerPanel.getByRole("button", { name: "入力項目「区分」を上へ" })).toBeFocused();
    await page.keyboard.press("Enter");
    await expect.poll(shownOrder).toEqual(["区分", "件名", "金額"]);
    await expect(triggerPanel.getByRole("button", { name: "入力項目「区分」を上へ" })).toBeDisabled();
    await expect(triggerPanel.getByRole("button", { name: "入力項目「区分」を下へ" })).toBeFocused();
    await expect(triggerPanel.getByRole("button", { name: "入力項目「金額」を下へ" })).toBeDisabled();
    await saveFlow(page);
    expect((await savedIr(page, id)).input_schema).toMatchObject({ "x-order": ["category", "title", "amount"] });
    await reopen();
    await expect.poll(shownOrder).toEqual(["区分", "件名", "金額"]);

    // 実行のダイアログも同じ順に並ぶ（開くだけで実行はしない）。
    await page.getByRole("button", { name: "実行", exact: true }).click();
    const dialog = page.getByRole("dialog", { name: "いま実行する" });
    await expect(dialog.getByRole("textbox", { name: "件名" })).toBeVisible();
    expect(await dialog.locator("label").allTextContents()).toEqual(["区分", "件名", "金額"]);
    await page.keyboard.press("Escape");
    await expect(dialog).toHaveCount(0);
  } finally {
    await deleteFlow(page, id);
  }
});

test("差し込む: {{key}} を直すボタンと差し込み名の変更で、本文の {名前} と vars が対応したまま保存される（BUG-1）", async ({
  page,
}) => {
  test.setTimeout(120_000);
  await loginViaKeycloak(page);
  const models = await llmModels(page);
  const id = await createLlmFlow(page, {
    model: models?.models[0]?.id,
    inputSchema: { type: "object", properties: { title: { type: "string", title: "件名" } } },
    prompt: { $template: "{{title}} と {title} を要約してください", vars: { title: { $from: "input", path: "/title" } } },
  });
  await openFlow(page, id);
  await page.locator(".react-flow").getByText("要約する").first().click();
  const panel = page.getByRole("complementary", { name: "ブロックの設定" });
  const prompt = panel.locator('[data-value-expr="AI への指示"]');
  const text = prompt.getByLabel("AI への指示");

  // {{title}} は波括弧そのものとして出る。ボタンで差し込みに直す。
  await expect(prompt.getByText("「{{title}}」は波括弧そのものとして出ます（差し込まれません）")).toBeVisible();
  await prompt.getByRole("button", { name: "{title} に直す" }).click();
  await expect(text).toHaveValue("{title} と {title} を要約してください");

  // 差し込み名を変えると本文の {title} も全部変わる。続けて差し込むと同じ参照の名前を使い回す。
  await prompt.getByTestId("template-var").getByLabel("差し込み名").fill("subject");
  await expect(text).toHaveValue("{subject} と {subject} を要約してください");
  await prompt.getByRole("button", { name: "差し込む" }).click();
  await page.getByRole("menuitem", { name: "件名（title）" }).click();
  await expect(text).toHaveValue("{subject} と {subject} を要約してください{subject}");
  await expect(prompt.getByTestId("template-var")).toHaveCount(1);

  await saveFlow(page);
  const ir = await savedIr(page, id);
  expect(ir.nodes[0].params.prompt).toEqual({
    $template: "{subject} と {subject} を要約してください{subject}",
    vars: { subject: { $from: "input", path: "/title" } },
  });
});

test("差し込む: 差し込みに変換した直後は、固定値のときのカーソル位置ではなく末尾に差し込む（BUG-1）", async ({
  page,
}) => {
  test.setTimeout(120_000);
  await loginViaKeycloak(page);
  const models = await llmModels(page);
  const id = await createLlmFlow(page, {
    model: models?.models[0]?.id,
    inputSchema: { type: "object", properties: { title: { type: "string", title: "件名" } } },
  });
  await openFlow(page, id);
  await page.locator(".react-flow").getByText("要約する").first().click();
  const prompt = page.getByRole("complementary", { name: "ブロックの設定" }).locator('[data-value-expr="AI への指示"]');
  const text = prompt.getByLabel("AI への指示");
  await text.fill("{{title}} を要約 {x}");
  // カーソルを固定値の 2 文字目に置いてから変換する（テンプレートでは文字の位置がずれる）。
  await text.click();
  await page.keyboard.press("Home");
  await page.keyboard.press("ArrowRight");
  await page.keyboard.press("ArrowRight");
  await prompt.getByTestId("literal-var-warning").getByRole("button", { name: "差し込みに変換する" }).click();
  await expect(text).toHaveValue("{title} を要約 {x}");
  await prompt.getByRole("button", { name: "差し込む" }).click();
  await page.getByRole("menuitem", { name: "件名（title）" }).click();
  await expect(text).toHaveValue("{title} を要約 {x}{title}");
});

test("モデル: AI に聞くのモデルを一覧から選び、答えの長さの上限と一緒に保存できる（BUG-4）", async ({ page }) => {
  test.setTimeout(120_000);
  await loginViaKeycloak(page);
  const models = await llmModels(page);
  test.skip(!models || models.models.length === 0, "LLM が構成されていない（使えるモデルがない）環境");
  const [first] = models!.models;
  // 今と違うモデルを選ぶ（1 つしかなければ同じものを選び直す）。
  const target = models!.models.at(-1)!;
  const id = await createLlmFlow(page, { prompt: "今日の予定をまとめてください", model: first.id });
  await openFlow(page, id);
  await page.locator(".react-flow").getByText("要約する").first().click();
  const panel = page.getByRole("complementary", { name: "ブロックの設定" });

  // ボタンには今のモデルの表示名が出て、押すと一覧（チャットと同じ部品）が開く。
  const button = panel.getByTestId("workflow-model-button");
  await expect(button).toContainText(first.label);
  await button.click();
  const picker = page.getByTestId("workflow-model-picker");
  await expect(picker.getByRole("listbox", { name: "モデル" })).toBeVisible();
  await picker.getByTestId(`model-option-${target.id}`).click();
  await expect(picker).toHaveCount(0);
  await expect(button).toContainText(target.label);

  // 答えの長さの上限は数で入れる（空欄で標準）。
  await panel.getByRole("spinbutton", { name: "答えの長さの上限（トークン・省略可）" }).fill("800");

  await saveFlow(page);
  const ir = await savedIr(page, id);
  expect(ir.nodes[0].params.model).toBe(target.id);
  expect(ir.nodes[0].params.max_tokens).toBe(800);
});

test("モデル: 一覧に無いモデルは値を消さずに赤字で知らせ、一覧を取れないときは自由入力に戻す（BUG-4）", async ({
  page,
}) => {
  test.setTimeout(120_000);
  await loginViaKeycloak(page);
  const models = await llmModels(page);
  test.skip(!models || models.models.length === 0, "LLM が構成されていない（使えるモデルがない）環境");
  const saved = models!.models[0];
  const id = await createLlmFlow(page, { prompt: "今日の予定をまとめてください", model: saved.id });

  // 保存の後に管理者がそのモデルを外した状況（一覧から外し、別のモデルだけを返す）。
  const other = { id: "e2e-other-model", label: "ほかのモデル", provider_label: "テスト", processed_abroad: false, supports_effort: false, description: null };
  await page.route(isLlmModels, (route) => route.fulfill({ json: { ...models!, models: [other] } }));
  await openFlow(page, id);
  await page.locator(".react-flow").getByText("要約する").first().click();
  const panel = page.getByRole("complementary", { name: "ブロックの設定" });
  await expect(panel.getByText(`このモデルは使えません: ${saved.id}`)).toBeVisible();
  await expect(panel.getByTestId("workflow-model-button")).toContainText(saved.id);

  // 一覧を取れない（LLM の構成の不調など）ときは、自由入力に戻して注意書きを出す（値はそのまま）。
  await page.unroute(isLlmModels);
  await page.route(isLlmModels, (route) =>
    route.fulfill({ status: 503, json: { detail: "AI が構成されていません" } }),
  );
  await page.reload();
  await page.locator(".react-flow").getByText("要約する").first().click();
  await expect(panel.getByText("モデルの一覧を読み込めませんでした")).toBeVisible({ timeout: 15_000 });
  await expect(panel.getByLabel("モデル ID")).toHaveValue(saved.id);
});

/// スマホ幅（設定は下からのシート）: 入力項目の追加・差し込む・モデルの一覧がシートの中で使え、横にはみ出さない。
test("スマホ: 入力項目・差し込む・モデルの一覧をシートの中で使える（BUG-1・GAP-12・BUG-4）", async ({
  browser,
  page,
}) => {
  test.setTimeout(180_000);
  const phone = await phonePage(browser, page);
  const models = await llmModels(page);
  const id = await createLlmFlow(page, { model: models?.models[0]?.id });
  await phone.goto(`/workflows/${id}`);
  const flow = phone.getByTestId("workflow-mobile-flow");
  await expect(flow).toBeVisible({ timeout: 15_000 });
  const noOverflow = async (sheet: ReturnType<Page["getByRole"]>) =>
    expect(await sheet.evaluate((el) => el.scrollWidth <= el.clientWidth + 1)).toBe(true);

  // きっかけのシートで入力項目を足す（カードの間の「＋」は文字を持たないので hasText で外す）。
  await flow.getByRole("button", { name: /手動で実行/ }).filter({ hasText: "手動で実行" }).click();
  // シートの読み上げ名は見出し（SheetTitle）になる。開いているシートは 1 つだけ。
  const triggerSheet = phone.getByRole("dialog");
  await triggerSheet.getByRole("button", { name: "項目を追加" }).click();
  const row = triggerSheet.getByTestId("input-field-row");
  await row.getByLabel("表示名").fill("件名");
  await row.getByLabel("キー").fill("title");
  await noOverflow(triggerSheet);
  await phone.keyboard.press("Escape");
  await expect(triggerSheet).toHaveCount(0);

  // ブロックのシートで差し込み、モデルの一覧をシートの中に広げて選ぶ。
  await flow.getByRole("button", { name: /要約する/ }).filter({ hasText: "要約する" }).click();
  const sheet = phone.getByRole("dialog");
  await expect(sheet.getByText("要約する").first()).toBeVisible();
  const prompt = sheet.locator('[data-value-expr="AI への指示"]');
  await prompt.getByLabel("AI への指示").fill("要約: ");
  await prompt.getByRole("button", { name: "差し込む" }).click();
  await phone.getByRole("menuitem", { name: "件名（title）" }).click();
  await expect(prompt.getByLabel("AI への指示")).toHaveValue("要約: {title}");
  await noOverflow(sheet);
  if (models && models.models.length > 0) {
    const target = models.models.at(-1)!;
    await sheet.getByTestId("workflow-model-button").click();
    const picker = sheet.getByTestId("workflow-model-picker");
    await expect(picker).toBeVisible();
    await picker.getByTestId(`model-option-${target.id}`).click();
    await expect(picker).toHaveCount(0);
    await expect(sheet.getByTestId("workflow-model-button")).toContainText(target.label);
    await expect(sheet).toBeVisible();
  }
  await noOverflow(sheet);
});

/// ログインは PC の画面で済ませ、そのセッションをタッチ端末の文脈へ持ち込む（chat-mobile.spec と同じ）。
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

// ── 第 1 弾の残りの直し（BUG-30〜32・40・41・D-37・D-42・D-43）─────────────────────

/// IR を BFF 経由で保存してエディタで開く（座標なし・自動配置）。
async function openIr(page: Page, ir: Record<string, unknown>): Promise<string> {
  const csrf = (await page.context().cookies()).find((c) => c.name === "shiki_csrf")?.value ?? "";
  const name = uniqueName("wf-small-fix");
  const res = await page.request.post("/api/workflows", {
    headers: { "x-csrf-token": csrf, "content-type": "application/json" },
    data: { ir: { ir_version: 1, name, display_name: name, triggers: [{ kind: "interactive" }], ...ir } },
  });
  expect(res.ok(), await res.text()).toBeTruthy();
  const id = ((await res.json()) as { id: string }).id;
  await page.goto(`/workflows/${id}`);
  await expect(page.locator(".react-flow__node").first()).toBeVisible({ timeout: 15_000 });
  await settleViewport(page);
  return id;
}

const scriptNode = (id: string, label: string) => ({
  id,
  type: "script.run",
  label,
  params: { source: { inline: "function main(input) { return input; }" } },
});

test("出口を消す設定: 振り分けの行き先を消す・失敗時をフローの失敗に戻すと、その出口のつながりも消え、Undo で戻る（BUG-30）", async ({
  page,
}) => {
  test.setTimeout(120_000);
  await loginViaKeycloak(page);
  await openIr(page, {
    nodes: [
      {
        id: "sw",
        type: "control.switch",
        label: "区分で振り分け",
        params: {
          value: { $from: "input", path: "/kind" },
          cases: [
            { port: "travel", equals: "交通費" },
            { port: "meal", equals: "会食費" },
          ],
        },
      },
      { ...scriptNode("t", "交通費の処理"), on_error: "continue" },
      scriptNode("m", "会食費の処理"),
      scriptNode("rec", "失敗を記録"),
    ],
    edges: [
      { from: "sw", from_port: "travel", to: "t" },
      { from: "sw", from_port: "meal", to: "m" },
      { from: "t", from_port: "error", to: "rec" },
    ],
  });
  const travel = page.getByTestId("rf__edge-e:sw|travel|t");
  const meal = page.getByTestId("rf__edge-e:sw|meal|m");
  const error = page.getByTestId("rf__edge-e:t|error|rec");
  await expect(travel).toHaveCount(1);
  await expect(error).toHaveCount(1);

  // 振り分けの行き先（travel）を消すと、その出口のつながりも同じ 1 手で消える（ほかの出口は残る）。
  await page.getByTestId("rf__node-sw").click();
  const panel = page.getByRole("complementary", { name: "ブロックの設定" });
  await panel.getByRole("button", { name: "行き先を削除" }).first().click();
  await expect(travel).toHaveCount(0);
  await expect(meal).toHaveCount(1);
  await page.locator(".react-flow__pane").click({ position: { x: 24, y: 24 } });
  await page.keyboard.press("Control+z");
  await expect(travel).toHaveCount(1);

  // 失敗したときを「フローを失敗にする」に戻すと、エラー時の出口のつながりが消える。
  await page.getByTestId("rf__node-t").click();
  await panel.getByRole("combobox", { name: "失敗したとき" }).click();
  await page.getByRole("option", { name: "フローを失敗にする" }).click();
  await expect(error).toHaveCount(0);
  await page.locator(".react-flow__pane").click({ position: { x: 24, y: 24 } });
  await page.keyboard.press("Control+z");
  await expect(error).toHaveCount(1);
  // 保存しても消えた出口の誤り（ir.unknown_port）にならない。
  await page.getByTestId("rf__node-sw").click();
  await panel.getByRole("button", { name: "行き先を削除" }).first().click();
  await page.getByRole("button", { name: "保存", exact: true }).click();
  await expect(page.getByText(/保存しました/).first()).toBeVisible({ timeout: 15_000 });
  await expect(page.locator('.react-flow [aria-label^="検証エラー"]')).toHaveCount(0);
});

test("条件の比べる値: 数値の入力項目と比べる数字は数として保存し、文字の項目と比べる数字は文字のまま（BUG-31）", async ({
  page,
}) => {
  test.setTimeout(120_000);
  await loginViaKeycloak(page);
  const cmp = (path: string, op: string) => ({ cmp: { left: { $from: "input", path }, op, right: "" } });
  const id = await openIr(page, {
    input_schema: {
      type: "object",
      properties: { amount: { type: "integer", title: "金額" }, code: { type: "string", title: "コード" } },
    },
    nodes: [
      {
        id: "br",
        type: "control.branch",
        label: "高額か",
        params: { condition: { all: [cmp("/amount", "gte"), cmp("/code", "eq")] } },
      },
    ],
    edges: [],
  });
  await page.getByTestId("rf__node-br").click();
  const panel = page.getByRole("complementary", { name: "ブロックの設定" });
  const rights = panel.locator('[data-value-expr="比べる値"]');
  await rights.nth(0).getByLabel("比べる値").fill("50,000");
  await expect(rights.nth(0).getByTestId("numeric-literal-note")).toBeVisible();
  await rights.nth(1).getByLabel("比べる値").fill("001");
  await expect(rights.nth(1).getByTestId("numeric-literal-note")).toHaveCount(0);
  await page.getByRole("button", { name: "保存", exact: true }).click();
  await expect(page.getByText(/保存しました/).first()).toBeVisible({ timeout: 15_000 });
  const wf = (await (await page.request.get(`/api/workflows/${id}`)).json()) as {
    ir: { nodes: { params: { condition: { all: { cmp: { right: unknown } }[] } } }[] };
  };
  const [amount, code] = wf.ir.nodes[0].params.condition.all;
  expect(amount.cmp.right).toBe(50000);
  expect(code.cmp.right).toBe("001");
});

test("条件の比べる値: 1 文字ずつ打っても途中の「0.」「1.0」「-0」が数の形に潰れず、打ったとおりの値で保存する（BUG-31）", async ({
  page,
}) => {
  test.setTimeout(120_000);
  await loginViaKeycloak(page);
  const cmp = (left: unknown, op: string) => ({ cmp: { left, op, right: "" } });
  const id = await openIr(page, {
    input_schema: {
      type: "object",
      properties: { rate: { type: "number", title: "率" }, amount: { type: "number", title: "金額" } },
    },
    nodes: [
      scriptNode("s", "点数を出す"),
      {
        id: "br",
        type: "control.branch",
        label: "基準を満たすか",
        params: {
          condition: {
            all: [
              cmp({ $from: "input", path: "/rate" }, "lt"),
              cmp({ $from: "input", path: "/amount" }, "gte"),
              cmp({ $from: "nodes.s.output", path: "/score" }, "eq"),
              cmp({ $from: "nodes.s.output", path: "/ratio" }, "lte"),
              cmp({ $from: "nodes.s.output", path: "/code" }, "eq"),
              cmp({ $from: "nodes.s.output", path: "/hour" }, "gte"),
              cmp({ $from: "input", path: "/rate" }, "gt"),
            ],
          },
        },
      },
    ],
    edges: [{ from: "s", to: "br" }],
  });
  await page.getByTestId("rf__node-br").click();
  const panel = page.getByRole("complementary", { name: "ブロックの設定" });
  const rights = panel.locator('[data-value-expr="比べる値"]');
  // 数値の入力項目（率・金額）と、型の分からない前のブロックの結果（点数・比率・コード・時）。全角の数字も数にする。
  // 頭に 0 を重ねたコード（001）は、型の分からない相手との等値では文字のまま比べる。大小比較（時 ≥ 09）では数にする
  // （数字を文字で書いた値との大小比較は実行で失敗する）。整数部を省いた「.5」も数として読む。
  const typed = ["0.08", "-0.5", "1.05", "０．２５", "001", "09", ".5"];
  for (const [i, text] of typed.entries()) {
    const input = rights.nth(i).getByLabel("比べる値");
    await input.click();
    await input.pressSequentially(text, { delay: 40 });
    // 打っている途中も、打ち終わっても、欄には打った文字のまま出る。
    await expect(input).toHaveValue(text);
  }
  for (const i of [0, 1, 2, 3, 5, 6]) await expect(rights.nth(i).getByTestId("numeric-literal-note")).toBeVisible();
  await expect(rights.nth(4).getByTestId("numeric-literal-note")).toHaveCount(0);
  await page.getByRole("button", { name: "保存", exact: true }).click();
  await expect(page.getByText(/保存しました/).first()).toBeVisible({ timeout: 15_000 });
  const wf = (await (await page.request.get(`/api/workflows/${id}`)).json()) as {
    ir: { nodes: { id: string; params: { condition?: { all: { cmp: { right: unknown } }[] } } }[] };
  };
  const all = wf.ir.nodes.find((n) => n.id === "br")?.params.condition?.all ?? [];
  expect(all.map((c) => c.cmp.right)).toEqual([0.08, -0.5, 1.05, 0.25, "001", 9, 0.5]);
});

test("ファイルを読む: 置いた直後はファイルが未選択で、前の結果を指さない（BUG-32）", async ({ page }) => {
  test.setTimeout(120_000);
  await loginViaKeycloak(page);
  await openIr(page, { nodes: [scriptNode("a", "受付")], edges: [] });
  const canvas = page.locator(".react-flow");
  await canvas.getByLabel("次のブロックを追加").first().click();
  await page.getByRole("button", { name: /^ファイルを読む/ }).click();
  // ドライブから選ぶ欄で置かれ（「前の結果から」の参照の欄ではない）、選ぶまで入力を促す。スケジュールで
  // 有効化しても「入力が無いので失敗します」にならない（既定で実行の入力を指さない）。
  const panel = page.getByRole("complementary", { name: "ブロックの設定" });
  await expect(panel.getByText("ファイルが選ばれていません")).toBeVisible();
  await expect(panel.getByRole("button", { name: /選ぶ/ }).first()).toBeVisible();
  await expect(panel.locator('[data-value-expr="読むファイル"]')).toHaveCount(0);
});

test("参照の候補: 前のブロックの出力の項目をカタログの項目から出す（D-42）", async ({ page }) => {
  test.setTimeout(120_000);
  await loginViaKeycloak(page);
  await openIr(page, {
    nodes: [
      { id: "r", type: "storage.read", label: "台帳を読む", params: { file: "8c8a6f6e-2ab7-4a44-a815-9a2b53c4e9a1" } },
      scriptNode("s", "整える"),
      { id: "ask", type: "llm.invoke", label: "要約する", params: { prompt: "要約: " } },
    ],
    edges: [
      { from: "r", from_port: "out", to: "s" },
      { from: "s", from_port: "out", to: "ask" },
    ],
  });
  await page.getByTestId("rf__node-ask").click();
  const panel = page.getByRole("complementary", { name: "ブロックの設定" });
  await panel.locator('[data-value-expr="AI への指示"]').getByRole("button", { name: "差し込む" }).click();
  for (const key of ["id", "name", "content_type", "size", "text"]) {
    await expect(page.getByRole("menuitem", { name: `「台帳を読む」の ${key}`, exact: true })).toBeVisible();
  }
  // script の戻り値は形が決まらないので、項目は出さず結果全体だけを出す。
  await expect(page.getByRole("menuitem", { name: "「整える」の結果（全体）" })).toBeVisible();
  await expect(page.getByRole("menuitem", { name: /^「整える」の (?!結果)/ })).toHaveCount(0);
});

test("保存時の警告: 閉じの無い { と、空の SQL を知らせる（D-37・D-43）", async ({ page }) => {
  test.setTimeout(120_000);
  await loginViaKeycloak(page);
  await openIr(page, {
    nodes: [
      {
        id: "ask",
        type: "llm.invoke",
        label: "要約する",
        params: {
          prompt: { $template: "件名: {title の内容を要約してください", vars: { title: { $from: "input", path: "/title" } } },
        },
      },
      { id: "q", type: "csv.query", label: "集計する", params: { file: "8c8a6f6e-2ab7-4a44-a815-9a2b53c4e9a1", sql: "" } },
    ],
    edges: [{ from: "ask", from_port: "out", to: "q" }],
  });
  const canvas = page.locator(".react-flow");
  await expect(canvas.locator('[aria-label="注意 1 件"]')).toHaveCount(2, { timeout: 15_000 });
  const panel = page.getByRole("complementary", { name: "ブロックの設定" });
  await page.getByTestId("rf__node-ask").click();
  await expect(panel.getByTestId("node-warnings")).toContainText("閉じの無い { があります");
  // 空の必須の欄は注意の帯ではなく、欄の下で入力を促す（有効化・実行の前に止まる）。
  await page.getByTestId("rf__node-q").click();
  await expect(panel.getByTestId("node-warnings")).toHaveCount(0);
  await expect(panel.getByText("入力してください")).toBeVisible();
  await expect(page.getByTestId("workflow-warnings")).toContainText("確認が必要な設定 2 件");
});

test("実行の前の確認: 指示が空のままでは実行せず、理由を知らせる（D-43）", async ({ page }) => {
  test.setTimeout(120_000);
  await loginViaKeycloak(page);
  const id = await createLlmFlow(page, {});
  await page.goto(`/workflows/${id}`);
  await expect(page.locator(".react-flow__node").first()).toBeVisible({ timeout: 15_000 });
  await page.getByRole("button", { name: "実行", exact: true }).click();
  await page.getByRole("button", { name: "実行する" }).click();
  await expect(page.getByText("実行に失敗しました").first()).toBeVisible({ timeout: 15_000 });
  await expect(page.getByText(/「AI への指示」が空です/).first()).toBeVisible();
  // run は作られない（実行履歴へ移らない）。
  await expect(page).toHaveURL(new RegExp(`/workflows/${id}$`));
  const runs = (await (await page.request.get(`/api/workflows/${id}/runs`)).json()) as { items: unknown[] };
  expect(runs.items).toHaveLength(0);
});

test("キャンバス: 短いつながりでは、出口のラベルが尻尾の＋に重ならない（BUG-41）", async ({ page }) => {
  test.setTimeout(120_000);
  await loginViaKeycloak(page);
  await openIr(page, {
    nodes: [
      { id: "br", type: "control.branch", label: "高額か", params: { condition: { cmp: { left: { $from: "input" }, op: "exists" } } } },
      scriptNode("a", "承認へ回す"),
    ],
    edges: [{ from: "br", from_port: "true", to: "a" }],
  });
  const tail = await page.getByTestId("rf__node-br").getByLabel("次のブロックを追加").boundingBox();
  expect(tail).not.toBeNull();
  const labels = page.locator(".react-flow__edgelabel-renderer span").filter({ hasText: /^はい$/ });
  const n = await labels.count();
  for (let i = 0; i < n; i += 1) {
    const b = (await labels.nth(i).boundingBox())!;
    const overlap =
      b.x < tail!.x + tail!.width && tail!.x < b.x + b.width && b.y < tail!.y + tail!.height && tail!.y < b.y + b.height;
    expect(overlap, `ラベル ${JSON.stringify(b)} と尻尾の＋ ${JSON.stringify(tail)}`).toBe(false);
  }
  // 出口の名前はカードの出口の並びに出ている。
  await expect(page.getByTestId("rf__node-br")).toContainText("はい");
});

test("スマホ: ブロックの設定シートは下端に余白を取り、最後の欄が画面の下端に付かない（BUG-40）", async ({
  browser,
  page,
}) => {
  test.setTimeout(180_000);
  const phone = await phonePage(browser, page);
  const id = await createLlmFlow(page, { prompt: "今日の予定をまとめてください" });
  await phone.goto(`/workflows/${id}`);
  const flow = phone.getByTestId("workflow-mobile-flow");
  await expect(flow).toBeVisible({ timeout: 15_000 });
  await flow.getByRole("button", { name: /要約する/ }).filter({ hasText: "要約する" }).click();
  const sheet = phone.getByRole("dialog");
  const last = sheet.getByRole("button", { name: "このブロックを削除" });
  await last.scrollIntoViewIfNeeded();
  // 本文を末尾まで送る（最後の欄がいちばん下に来る位置）。
  await sheet.locator(".overflow-y-auto").first().evaluate((el) => el.scrollTo({ top: el.scrollHeight }));
  await nextFrame(phone);
  const box = (await last.boundingBox())!;
  const viewport = phone.viewportSize()!;
  // 左下に浮くボタン（高さ 44px 程度）の上に出る。
  expect(box.y + box.height).toBeLessThanOrEqual(viewport.height - 64);
});
