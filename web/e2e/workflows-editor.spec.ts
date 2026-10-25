import { expect, test, type Page } from "@playwright/test";

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
  const name = panel.getByPlaceholder("スクリプト", { exact: true });
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
  await panel.getByPlaceholder("例: 次の文章を 3 行で要約してください").fill("今日の予定を 3 行でまとめてください");
  await expect(panel.getByText("入力してください")).toHaveCount(0);
});
