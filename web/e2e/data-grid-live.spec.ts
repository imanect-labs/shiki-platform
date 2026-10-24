import { expect, test, type Page } from "@playwright/test";

import { loginViaKeycloak, uniqueName } from "./helpers";

/// グリッドの変更の購読（SSE）の繋ぎ直しと、読み込み位置の失効（D-2・D-4・D-9）。
///
/// サーバの停止・購読の中身は `page.route` で決め打ちにする（EventSource・グリッドは本物）。BFF の中継
/// （`/api/data/tables/{id}/stream` の route handler）が上流の切断を伝えることは、上流を落とせる構成での
/// 手動確認に任せる（E2E は共有のサーバを止められないため）。

async function csrf(page: Page): Promise<string> {
  return (await page.context().cookies()).find((c) => c.name === "shiki_csrf")?.value ?? "";
}

async function seedTable(page: Page, n: number): Promise<{ tableId: string; ids: string[] }> {
  const headers = { "X-CSRF-Token": await csrf(page) };
  const res = await page.request.post("/api/data/tables", {
    headers,
    data: {
      name: uniqueName("e2e-購読"),
      schema: { fields: [{ key: "code", display_name: "コード", type: "text" }, { key: "amount", display_name: "金額", type: "number" }] },
    },
  });
  expect(res.status(), await res.text()).toBe(201);
  const tableId = ((await res.json()) as { id: string }).id;
  const ids: string[] = [];
  for (let i = 0; i < n; i++) {
    const r = await page.request.post(`/api/data/tables/${tableId}/records`, { headers, data: { data: { code: `R-${i}`, amount: i } } });
    expect(r.status()).toBe(201);
    ids.push(((await r.json()) as { id: string }).id);
  }
  return { tableId, ids };
}

const sse = (events: string[]) => ({
  status: 200,
  headers: { "content-type": "text/event-stream", "cache-control": "no-cache" },
  body: `retry: 300\n\n${events.join("\n\n")}\n\n`,
});

test("サーバに届かない間は「再接続しています」を出し、戻ったら最後に受け取った番号から繋ぎ直す（D-2）", async ({ page }) => {
  await loginViaKeycloak(page);
  const { tableId } = await seedTable(page, 1);
  let mode: "events" | "down" | "up" = "events";
  const streamUrls: string[] = [];
  await page.route(new RegExp(`/api/data/tables/${tableId}/stream`), async (route) => {
    streamUrls.push(route.request().url());
    if (mode === "down") return route.fulfill({ status: 502, body: "" });
    // 番号 41・42 のイベントを送って閉じる（＝サーバが落ちた）。以後は繋がらない。
    if (mode === "events") {
      mode = "down";
      return route.fulfill(
        sse([
          `event: stream.ready\ndata: ${JSON.stringify({ position: 40 })}`,
          `id: 41\nevent: awareness\ndata: ${JSON.stringify({ principal: "00000000-0000-0000-0000-000000000002", record_id: null, field: null, at: new Date().toISOString() })}`,
          `id: 42\nevent: record.removed\ndata: ${JSON.stringify({ record_id: "00000000-0000-0000-0000-00000000dead" })}`,
        ]),
      );
    }
    return route.fulfill(sse([`event: stream.ready\ndata: ${JSON.stringify({ position: 42 })}`]));
  });
  // REST も落ちている（BFF の 502）。
  await page.route(new RegExp(`/api/data/tables/${tableId}/viewer-context`), async (route) =>
    mode === "down" ? route.fulfill({ status: 502, body: "" }) : route.continue(),
  );

  await page.goto(`/data/${tableId}`);
  await expect(page.getByTestId("data-grid")).toHaveAttribute("data-loaded", "1");
  const banner = page.getByTestId("data-grid-live-problem");
  await expect(banner).toContainText("再接続しています", { timeout: 20_000 });
  // 行は見えたまま。
  await expect(page.getByTestId("data-grid")).toHaveAttribute("data-loaded", "1");

  // サーバが戻る: 予約した繋ぎ直しで、最後に受け取った番号（42）から購読し直し、案内が消える。
  mode = "up";
  await expect(banner).toBeHidden({ timeout: 20_000 });
  expect(streamUrls.at(-1)).toContain("since=42");
});

test("非表示のタブは購読を止め、表示に戻ると続きから繋ぎ直す（D-4）", async ({ page }) => {
  await loginViaKeycloak(page);
  const { tableId } = await seedTable(page, 1);
  const streamUrls: string[] = [];
  await page.route(new RegExp(`/api/data/tables/${tableId}/stream`), async (route) => {
    streamUrls.push(route.request().url());
    // 番号 7 まで受け取ったところで開いたまま（retry を長くして、自動の繋ぎ直しと区別する）。
    await route.fulfill({
      status: 200,
      headers: { "content-type": "text/event-stream" },
      body: `retry: 600000\n\nevent: stream.ready\ndata: {"position":6}\n\nid: 7\nevent: record.removed\ndata: {"record_id":"00000000-0000-0000-0000-00000000beef"}\n\n`,
    });
  });
  await page.goto(`/data/${tableId}`);
  await expect(page.getByTestId("data-grid")).toHaveAttribute("data-loaded", "1");
  await expect.poll(() => streamUrls.length).toBeGreaterThan(0);
  const before = streamUrls.length;

  const setVisibility = (state: "hidden" | "visible") =>
    page.evaluate((s) => {
      Object.defineProperty(document, "visibilityState", { configurable: true, get: () => s });
      document.dispatchEvent(new Event("visibilitychange"));
    }, state);
  await setVisibility("hidden");
  await page.waitForTimeout(800);
  // 非表示の間は繋がない。
  expect(streamUrls.length).toBe(before);
  await setVisibility("visible");
  await expect.poll(() => streamUrls.length).toBe(before + 1);
  expect(streamUrls.at(-1)).toContain("since=7");
});

test("続きの読み込み位置が使えなくなったら、空行のまま止めず先頭から読み直す（D-9）", async ({ page }) => {
  await loginViaKeycloak(page);
  const { tableId } = await seedTable(page, 2);
  let queries = 0;
  // 開発モードの StrictMode では先頭ページが 2 回取られるので、偽の位置は失効を返すまで付け続ける。
  let rejected = false;
  await page.route(new RegExp(`/api/data/tables/${tableId}/query$`), async (route) => {
    const body = route.request().postDataJSON() as { with_count?: boolean; cursor?: string };
    if (body.with_count) return route.continue();
    queries += 1;
    if (!body.cursor && !rejected) {
      // 先頭ページに偽の続きの位置を付ける（サーバの再起動で鍵が変わったカーソルの代わり）。
      const res = await route.fetch();
      const json = (await res.json()) as Record<string, unknown>;
      return route.fulfill({ response: res, json: { ...json, next_cursor: "stale-cursor" } });
    }
    if (body.cursor === "stale-cursor") {
      rejected = true;
      return route.fulfill({
        status: 400,
        json: { status: 400, title: "Bad Request", code: "cursor_invalid", message: "読み込み位置が無効です" },
      });
    }
    return route.continue();
  });
  await page.goto(`/data/${tableId}`);
  await expect(page.getByText("読み込み位置が古くなったため、先頭から読み直しました").first()).toBeVisible({ timeout: 15_000 });
  await expect(page.getByTestId("data-grid")).toHaveAttribute("data-loaded", "2");
  expect(rejected).toBe(true);
  expect(queries).toBeGreaterThanOrEqual(3);
});
