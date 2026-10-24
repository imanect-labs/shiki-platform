import { writeFileSync } from "node:fs";

import { expect, test, type Page } from "@playwright/test";

import { loginViaKeycloak, uniqueName } from "./helpers";

/// 100 万行のテーブルでグリッドの初回描画を測る（Task 15.5 の受け入れ条件「初回描画 p95 < 300ms」・
/// 監査 B3-1）。結果は `docs/data/benchmarks.md` §6 に記録する。
///
/// **手動実行専用**（投入に十数分かかり、DB に数 GB を使うため CI では走らせない）。`DATA_PERF=1` が
/// 無ければ skip する。
///
/// ```bash
/// # web は本番ビルド（pnpm build && pnpm start）で測る。dev サーバはコンパイルの時間が混ざる。
/// DATA_PERF=1 pnpm exec playwright test e2e/data-first-render.manual.spec.ts
/// # 2 回目以降は投入済みのテーブルを使い回す（投入を省く）
/// DATA_PERF=1 DATA_PERF_TABLE=<table uuid> pnpm exec playwright test e2e/data-first-render.manual.spec.ts
/// ```
///
/// - 投入は**一括取込の HTTP API**（`POST /data/tables/{id}/imports/csv`・25 万行ずつ・索引は遅延構築）で行い、
///   取込が `done`（索引の仕上げまで）になるのを待つ。
/// - 計測値はグリッドの `data-first-render-ms`（先頭ページのクエリを出してから描くまで）と、ページ遷移の
///   開始から描くまで（`performance.mark("data-grid:first-render")`）の 2 つ。前者が受け入れ条件の対象。
/// - `DATA_PERF_N`（既定 30）回ページを開き直して p50 / p95 を出す。`DATA_PERF_OUT` に JSON で書き出せる。

const ROWS = Number(process.env.DATA_PERF_ROWS ?? 1_000_000);
const CHUNK = 250_000;
const N = Number(process.env.DATA_PERF_N ?? 30);

const DEPTS = ["営業部", "開発部", "経理部", "人事部", "総務部"];
const STATUSES = ["申請中", "承認", "却下"];

function pct(xs: number[], p: number): number {
  const s = [...xs].sort((a, b) => a - b);
  return s[Math.min(s.length - 1, Math.max(0, Math.ceil((p / 100) * s.length) - 1))];
}

async function csrf(page: Page): Promise<string> {
  return (await page.context().cookies()).find((c) => c.name === "shiki_csrf")?.value ?? "";
}

/// 決定的な合成データ（行ごとに値が変わる・日付は 2024〜2026 年）。
function csvChunk(from: number, to: number): string {
  const lines = ["name,amount,status,day,dept"];
  for (let i = from; i < to; i++) {
    const d = new Date(Date.UTC(2024, 0, 1) + ((i * 7919) % 1000) * 86_400_000).toISOString().slice(0, 10);
    lines.push(`経費 ${i},${(i * 7_919) % 500_000},${STATUSES[i % 3]},${d},${DEPTS[i % 5]}`);
  }
  return `${lines.join("\n")}\n`;
}

async function seed(page: Page): Promise<string> {
  const headers = { "X-CSRF-Token": await csrf(page) };
  const created = await page.request.post("/api/data/tables", {
    headers,
    data: {
      name: uniqueName(`経費 ${ROWS.toLocaleString("ja-JP")} 行`),
      schema: {
        fields: [
          { key: "name", display_name: "件名", type: "text", indexed: true },
          { key: "amount", display_name: "金額", type: "number", indexed: true },
          { key: "status", display_name: "状態", type: "select", options: STATUSES, indexed: true },
          { key: "day", display_name: "日付", type: "date", indexed: true },
          { key: "dept", display_name: "部署", type: "text" },
        ],
      },
    },
  });
  expect(created.status(), await created.text()).toBe(201);
  const tableId = ((await created.json()) as { id: string }).id;
  for (let from = 0; from < ROWS; from += CHUNK) {
    const to = Math.min(ROWS, from + CHUNK);
    const started = Date.now();
    const res = await page.request.post(`/api/data/tables/${tableId}/imports/csv?index_mode=deferred`, {
      headers: { ...headers, "Content-Type": "text/csv; charset=utf-8" },
      data: csvChunk(from, to),
      timeout: 300_000,
    });
    expect(res.status(), await res.text()).toBe(202);
    const { import_id } = (await res.json()) as { import_id: string };
    let status = "";
    while (status !== "done") {
      await page.waitForTimeout(2_000);
      const s = (await (await page.request.get(`/api/data/tables/${tableId}/imports/${import_id}`)).json()) as {
        status: string;
        rows_applied: number;
        rows_invalid: number;
        last_error?: string | null;
      };
      expect(s.status, s.last_error ?? "").not.toBe("failed");
      expect(s.rows_invalid).toBe(0);
      status = s.status;
    }
    console.log(`[seed] ${to.toLocaleString("en-US")} 行（この分 ${((Date.now() - started) / 1000).toFixed(1)} 秒）`);
  }
  return tableId;
}

test.describe("グリッドの初回描画（100 万行・手動計測）", () => {
  test.skip(!process.env.DATA_PERF, "DATA_PERF=1 のときだけ走らせる（手動の性能計測）");

  test("100 万行で初回描画 p95 < 300ms", async ({ page }) => {
    test.setTimeout(3 * 60 * 60_000);
    await loginViaKeycloak(page);
    const tableId = process.env.DATA_PERF_TABLE ?? (await seed(page));
    const url = `/data/${tableId}`;

    // 温め（初回のコンパイル・接続の確立・プランキャッシュ）。
    for (let i = 0; i < 3; i++) {
      await page.goto(url);
      await page.locator("[data-first-render-ms]").waitFor({ timeout: 60_000 });
    }

    const grid: number[] = [];
    const nav: number[] = [];
    for (let i = 0; i < N; i++) {
      await page.goto(url);
      const el = page.locator("[data-first-render-ms]");
      await el.waitFor({ timeout: 60_000 });
      grid.push(Number(await el.getAttribute("data-first-render-ms")));
      nav.push(
        await page.evaluate(() =>
          Math.round(performance.getEntriesByName("data-grid:first-render").at(-1)?.startTime ?? -1),
        ),
      );
    }
    // 件数（非同期の 2 本目）が出るのを待って、100 万行のテーブルであることを確かめる。
    await expect(page.getByTestId("data-count")).not.toHaveText("", { timeout: 60_000 });
    const countLabel = await page.getByTestId("data-count").textContent();

    // 無限スクロール: 下へ送り続けて 5,000 行まで読む（keyset で何ページ目でも同じコスト）。
    const box = await page.getByTestId("data-grid").boundingBox();
    if (!box) throw new Error("grid not visible");
    await page.mouse.move(box.x + 300, box.y + 300);
    const t0 = Date.now();
    while (Number(await page.getByTestId("data-grid").getAttribute("data-loaded")) < 5_000 && Date.now() - t0 < 120_000) {
      await page.mouse.wheel(0, 4_000);
      await page.waitForTimeout(50);
    }
    const loaded = Number(await page.getByTestId("data-grid").getAttribute("data-loaded"));
    const scrollMs = Date.now() - t0;

    const result = {
      tableId,
      rows: ROWS,
      n: N,
      countLabel,
      gridFirstRenderMs: { p50: pct(grid, 50), p95: pct(grid, 95), max: Math.max(...grid) },
      navigationToRenderMs: { p50: pct(nav, 50), p95: pct(nav, 95), max: Math.max(...nav) },
      scroll: { loadedRows: loaded, ms: scrollMs },
      samples: { grid, nav },
    };
    console.log(JSON.stringify(result, null, 2));
    if (process.env.DATA_PERF_OUT) writeFileSync(process.env.DATA_PERF_OUT, JSON.stringify(result, null, 2));
    if (process.env.DATA_PERF_SHOT) await page.screenshot({ path: process.env.DATA_PERF_SHOT });

    expect(loaded).toBeGreaterThanOrEqual(5_000);
    expect(result.gridFirstRenderMs.p95).toBeLessThan(300);
  });
});
