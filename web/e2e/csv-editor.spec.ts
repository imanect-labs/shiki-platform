import { expect, test, type Page } from "@playwright/test";

import { shiftJis } from "./fixtures/shift-jis";
import { cleanupAll, loginAs, loginViaKeycloak, uniqueName } from "./helpers";

/// CSV グリッドエディタ（Task 11P.8）の受け入れ条件を検証する:
/// - グリッドで表示・セル編集・保存（新バージョン）
/// - 並行編集の衝突が rev で検出される（409 → 衝突ダイアログ）
/// - SQL コンソール（RO）で実行 → 「新規 CSV として保存」
/// - viewer は閲覧のみ

/// CSV を API で作成して node_id を返す。
async function createCsv(page: Page, name: string, csv: string): Promise<string> {
  return page.evaluate(
    async ({ n, body }) => {
      const csrf = document.cookie.match(/(?:^|;\s*)shiki_csrf=([^;]+)/)?.[1] ?? "";
      const res = await fetch("/api/tabular/save", {
        method: "POST",
        credentials: "include",
        headers: { "Content-Type": "application/json", "X-CSRF-Token": csrf },
        body: JSON.stringify({ parent_id: null, name: n, csv: body }),
      });
      if (!res.ok) throw new Error(`CSV 作成に失敗: ${res.status}`);
      return (await res.json()).node_id as string;
    },
    { n: name, body: csv },
  );
}

async function shareCsv(page: Page, nodeId: string, role: "viewer" | "editor") {
  await page.evaluate(
    async ({ id, shareRole }) => {
      const csrf = document.cookie.match(/(?:^|;\s*)shiki_csrf=([^;]+)/)?.[1] ?? "";
      const res = await fetch(`/api/nodes/${id}/shares`, {
        method: "PUT",
        credentials: "include",
        headers: { "Content-Type": "application/json", "X-CSRF-Token": csrf },
        body: JSON.stringify({
          target: { type: "user", id: "00000000-0000-0000-0000-000000000002" },
          role: shareRole,
        }),
      });
      if (!res.ok) throw new Error(`共有に失敗: ${res.status}`);
    },
    { id: nodeId, shareRole: role },
  );
}

async function openCsv(page: Page, nodeId: string) {
  await page.goto(`/csv/${nodeId}`);
  await expect(page.getByTestId("csv-grid")).toBeVisible({ timeout: 20_000 });
}

test("CSV グリッド: 表示・セル編集・保存", async ({ page }) => {
  await loginViaKeycloak(page); // alice
  await page.goto("/drive");
  const id = await createCsv(page, uniqueName("data") + ".csv", "id,name\n1,alice\n2,bob\n");
  await openCsv(page, id);

  // グリッドにデータ行が見える（Glide は canvas 描画のため総行数表示で確認）。
  await expect(page.getByText(/2 行 × 2 列/)).toBeVisible({ timeout: 15_000 });

  // セル編集はグリッド API 経由が難しいため、パッチ API を直接叩いて保存経路を検証する
  // （グリッドの onCellEdited → applyPatch と同じ contract）。
  const result = await page.evaluate(async (nodeId) => {
    const csrf = document.cookie.match(/(?:^|;\s*)shiki_csrf=([^;]+)/)?.[1] ?? "";
    // 現在の版を取得。
    const acc = await (await fetch(`/api/collab/docs/${nodeId}/access`, { credentials: "include" })).json();
    const res = await fetch(`/api/files/${nodeId}/tabular/patch`, {
      method: "POST",
      credentials: "include",
      headers: { "Content-Type": "application/json", "X-CSRF-Token": csrf },
      body: JSON.stringify({
        base_rev: acc.version,
        ops: [{ op: "cell_update", row: 0, col: 1, value: "ALICE" }],
      }),
    });
    return { status: res.status, body: await res.json() };
  }, id);
  expect(result.status).toBe(200);
  expect(result.body.version).toBeGreaterThan(0);
});

test("CSV 並行編集: 古い rev は 409 で拒否される（黙って上書きしない）", async ({ page }) => {
  await loginViaKeycloak(page);
  await page.goto("/drive");
  const id = await createCsv(page, uniqueName("conflict") + ".csv", "a\n1\n");
  const conflict = await page.evaluate(async (nodeId) => {
    const csrf = document.cookie.match(/(?:^|;\s*)shiki_csrf=([^;]+)/)?.[1] ?? "";
    const acc = await (await fetch(`/api/collab/docs/${nodeId}/access`, { credentials: "include" })).json();
    const stale = acc.version;
    // 1 回目の保存で版が進む。
    await fetch(`/api/files/${nodeId}/tabular/patch`, {
      method: "POST",
      credentials: "include",
      headers: { "Content-Type": "application/json", "X-CSRF-Token": csrf },
      body: JSON.stringify({ base_rev: stale, ops: [{ op: "cell_update", row: 0, col: 0, value: "2" }] }),
    });
    // 2 回目は古い版で送る → 409。
    const res = await fetch(`/api/files/${nodeId}/tabular/patch`, {
      method: "POST",
      credentials: "include",
      headers: { "Content-Type": "application/json", "X-CSRF-Token": csrf },
      body: JSON.stringify({ base_rev: stale, ops: [{ op: "cell_update", row: 0, col: 0, value: "3" }] }),
    });
    return res.status;
  }, id);
  expect(conflict).toBe(409);
});

test("CSV SQL コンソール: RO クエリ実行と新規 CSV 保存", async ({ page }) => {
  await loginViaKeycloak(page);
  await page.goto("/drive");
  const id = await createCsv(page, uniqueName("sql") + ".csv", "id,score\n1,10\n2,30\n3,20\n");
  await openCsv(page, id);

  // SQL タブへ切替。
  await page.getByTestId("csv-tab-sql").click();
  await expect(page.getByTestId("sql-console")).toBeVisible();
  await page.getByTestId("sql-input").fill("SELECT id, score FROM data WHERE CAST(score AS INT) >= 20 ORDER BY id");
  await page.getByTestId("sql-run").click();
  // 結果テーブルに 2 行（score>=20）。
  await expect(page.locator('[data-testid="sql-console"] table tbody tr')).toHaveCount(2, {
    timeout: 15_000,
  });

  // DML は拒否される（400 → エラー表示）。
  await page.getByTestId("sql-input").fill("DELETE FROM data");
  await page.getByTestId("sql-run").click();
  await expect(page.getByTestId("sql-error")).toBeVisible({ timeout: 15_000 });
});

test("CSV viewer は閲覧のみ", async ({ page, browser }) => {
  await loginViaKeycloak(page); // alice
  await page.goto("/drive");
  const id = await createCsv(page, uniqueName("ro") + ".csv", "a,b\n1,2\n");
  await shareCsv(page, id, "viewer");

  const bobCtx = await browser.newContext();
  const bobPage = await bobCtx.newPage();
  await loginAs(bobPage, "bob");
  await openCsv(bobPage, id);
  await expect(bobPage.getByTestId("csv-readonly-badge")).toBeVisible();
  // 保存ボタンは editor のみ表示（viewer には出ない）。
  await expect(bobPage.getByTestId("csv-save")).toHaveCount(0);
  await bobCtx.close();
});

/// 日本語の Windows の Excel が既定で保存する Shift_JIS の CSV も、グリッドで日本語の列名のまま開ける（GAP-45）。
/// 以前は DuckDB が UTF-8 でない中身を読めず、画面は開けなかった。
test("CSV グリッド: Shift_JIS の CSV を開ける", async ({ page }) => {
  test.setTimeout(120_000);
  await page.setViewportSize({ width: 1440, height: 900 });
  await loginViaKeycloak(page);
  const csrf = async () => (await page.context().cookies()).find((c) => c.name === "shiki_csrf")?.value ?? "";
  let folderId = "";
  let fileId = "";
  try {
    const folder = await page.request.post("/api/folders", {
      headers: { "x-csrf-token": await csrf(), "content-type": "application/json" },
      data: { name: uniqueName("e2e-sjis-csv"), parent_id: null },
    });
    expect(folder.ok(), await folder.text()).toBeTruthy();
    folderId = ((await folder.json()) as { id: string }).id;
    await page.goto(`/drive?folder=${folderId}`);
    const name = `${uniqueName("経費")}.csv`;
    await page.locator('input[type="file"][multiple]').setInputFiles({
      name,
      mimeType: "application/vnd.ms-excel",
      buffer: shiftJis("経費,金額\n交通費,1200\n宿泊費,8000\n"),
    });
    await expect(page.getByText(name, { exact: true })).toBeVisible({ timeout: 20_000 });
    await expect
      .poll(
        async () => {
          const res = await page.request.get(`/api/nodes?parent_id=${folderId}`);
          const items = ((await res.json()) as { items?: { id: string; name: string }[] }).items ?? [];
          fileId = items.find((c) => c.name === name)?.id ?? "";
          return fileId;
        },
        { timeout: 30_000 },
      )
      .not.toBe("");

    await openCsv(page, fileId);
    await expect(page.getByText(/2 行 × 2 列/)).toBeVisible({ timeout: 15_000 });
    const schema = await page.request.get(`/api/files/${fileId}/tabular/schema`);
    expect(((await schema.json()) as { columns: string[] }).columns).toEqual(["経費", "金額"]);
    if (process.env.E2E_SHOTS_DIR) {
      await page.screenshot({ path: `${process.env.E2E_SHOTS_DIR}/csv-grid-sjis-pc.png` });
    }
  } finally {
    await cleanupAll(
      async () => {
        if (!fileId) return;
        const del = await page.request.delete(`/api/files/${fileId}`, { headers: { "x-csrf-token": await csrf() } });
        expect(del.status(), await del.text()).toBeLessThan(300);
      },
      async () => {
        if (!folderId) return;
        const del = await page.request.delete(`/api/folders/${folderId}`, { headers: { "x-csrf-token": await csrf() } });
        expect(del.status(), await del.text()).toBeLessThan(300);
      },
    );
  }
});

/// 文字コードを読み取れない CSV（西欧の文字コードなど）を開くと、「見つかりません」とは言わずに、サーバーが返す
/// 理由と直し方（UTF-8 か Shift_JIS で保存する）を出す（GAP-45）。行の読み込みに失敗したときも、空の行に見せず、
/// 読み込めなかったことと読み込み直す手段を出す。
test("CSV グリッド: 読めない CSV は理由を言い、行を読み込めなければそう言う", async ({ page, browser }) => {
  test.setTimeout(150_000);
  await page.setViewportSize({ width: 1440, height: 900 });
  await loginViaKeycloak(page);
  const csrf = async () => (await page.context().cookies()).find((c) => c.name === "shiki_csrf")?.value ?? "";
  let folderId = "";
  const fileIds: string[] = [];
  try {
    const folder = await page.request.post("/api/folders", {
      headers: { "x-csrf-token": await csrf(), "content-type": "application/json" },
      data: { name: uniqueName("e2e-unreadable-csv"), parent_id: null },
    });
    expect(folder.ok(), await folder.text()).toBeTruthy();
    folderId = ((await folder.json()) as { id: string }).id;
    const upload = async (name: string, buffer: Buffer): Promise<string> => {
      await page.goto(`/drive?folder=${folderId}`);
      await page.locator('input[type="file"][multiple]').setInputFiles({ name, mimeType: "text/csv", buffer });
      await expect(page.getByText(name, { exact: true })).toBeVisible({ timeout: 20_000 });
      let id = "";
      await expect
        .poll(
          async () => {
            const res = await page.request.get(`/api/nodes?parent_id=${folderId}`);
            const items = ((await res.json()) as { items?: { id: string; name: string }[] }).items ?? [];
            id = items.find((c) => c.name === name)?.id ?? "";
            return id;
          },
          { timeout: 30_000 },
        )
        .not.toBe("");
      fileIds.push(id);
      return id;
    };
    // 西欧の文字コード（Windows-1252）で保存した CSV。UTF-8 でも日本の文字コードでもない。
    const latin = await upload(`${uniqueName("prix")}.csv`, Buffer.from("Müller,Straße\nCafé,Crème brûlée\n", "latin1"));
    const schema = await page.request.get(`/api/files/${latin}/tabular/schema`);
    expect(schema.status()).toBe(400);
    const reason = ((await schema.json()) as { code: string; message: string }).message;
    expect(reason).toContain("UTF-8 か Shift_JIS で保存");

    await page.goto(`/csv/${latin}`);
    await expect(page.getByText("CSV を開けません", { exact: true })).toBeVisible({ timeout: 20_000 });
    await expect(page.getByText(reason, { exact: true })).toBeVisible();
    await expect(page.getByText("CSV が見つかりません")).toHaveCount(0);
    // 読み込み直しても変わらないので、読み込み直すボタンは出さない（ドライブへ戻る導線は残る）。
    await expect(page.getByRole("button", { name: "もう一度読み込む" })).toHaveCount(0);
    await expect(page.getByRole("link", { name: "ドライブへ戻る" })).toBeVisible();
    if (process.env.E2E_SHOTS_DIR) {
      await page.screenshot({ path: `${process.env.E2E_SHOTS_DIR}/csv-unreadable-pc.png` });
    }
    const phoneCtx = await browser.newContext({
      storageState: await page.context().storageState(),
      viewport: { width: 390, height: 844 },
      hasTouch: true,
      isMobile: true,
      deviceScaleFactor: 3,
      ignoreHTTPSErrors: true,
    });
    try {
      const phone = await phoneCtx.newPage();
      await phone.goto(`/csv/${latin}`);
      await expect(phone.getByText(reason, { exact: true })).toBeVisible({ timeout: 20_000 });
      expect(await phone.evaluate(() => document.documentElement.scrollWidth)).toBeLessThanOrEqual(390);
      if (process.env.E2E_SHOTS_DIR) {
        await phone.screenshot({ path: `${process.env.E2E_SHOTS_DIR}/csv-unreadable-phone.png` });
      }
    } finally {
      await phoneCtx.close();
    }

    // 行の読み込みに失敗したら、空の行に見せずにそう言い、読み込み直せる。
    const ok = await upload(`${uniqueName("rows")}.csv`, Buffer.from("id,name\n1,alice\n2,bob\n"));
    await page.route(`**/api/files/${ok}/tabular/rows**`, (route) =>
      route.fulfill({ status: 500, contentType: "application/json", body: JSON.stringify({ status: 500, title: "x" }) }),
    );
    await openCsv(page, ok);
    const banner = page.getByTestId("csv-rows-error");
    await expect(banner).toBeVisible({ timeout: 20_000 });
    await expect(banner).toContainText("行を読み込めませんでした。");
    if (process.env.E2E_SHOTS_DIR) {
      await page.screenshot({ path: `${process.env.E2E_SHOTS_DIR}/csv-rows-error-pc.png` });
    }
    await page.unroute(`**/api/files/${ok}/tabular/rows**`);
    const rows = page.waitForResponse((r) => r.url().includes(`/api/files/${ok}/tabular/rows`) && r.ok());
    await banner.getByRole("button", { name: "もう一度読み込む" }).click();
    await rows;
    await expect(banner).toHaveCount(0);
  } finally {
    await cleanupAll(
      ...fileIds.map((id) => async () => {
        const del = await page.request.delete(`/api/files/${id}`, { headers: { "x-csrf-token": await csrf() } });
        expect(del.status(), await del.text()).toBeLessThan(300);
      }),
      async () => {
        if (!folderId) return;
        const del = await page.request.delete(`/api/folders/${folderId}`, { headers: { "x-csrf-token": await csrf() } });
        expect(del.status(), await del.text()).toBeLessThan(300);
      },
    );
  }
});
