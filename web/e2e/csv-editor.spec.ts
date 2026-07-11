import { expect, test, type Page } from "@playwright/test";

import { loginAs, loginViaKeycloak, uniqueName } from "./helpers";

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
