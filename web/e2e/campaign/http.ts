import { expect, type APIResponse, type Page } from "@playwright/test";

/// キャンペーンの E2E（docs/workflow/quality-campaign.md §7 の APP-01〜12）が共有する、ログイン済みの画面の
/// セッションで backend API を呼ぶ道具（CSRF の二重送信つき）。組む・裏取りする・押す、のどれもここを通す。

export type Json = Record<string, unknown>;

/// 応答の状態コードと本文（JSON でなければ null）。
export type Reply<T = unknown> = { status: number; body: T; text: string };

/// CSRF の二重送信の値（ログインした画面の cookie）。
export async function csrf(page: Page): Promise<string> {
  return (await page.context().cookies()).find((c) => c.name === "shiki_csrf")?.value ?? "";
}

/// 閉じた接続に当たったときだけ、つなぎ直して 1 回送り直す（next dev は 5 秒使わない接続を閉じる。閉じかけの接続で
/// read ECONNRESET になったときは、要求はサーバーに届いていない・helpers.ts の sendCleanup と同じ判断）。
async function send(fn: () => Promise<APIResponse>): Promise<APIResponse> {
  try {
    return await fn();
  } catch (e) {
    if (!/ECONNRESET|socket hang up/.test(String(e))) throw e;
    return fn();
  }
}

/// `/api{path}` を呼び、状態コードと本文を返す（状態コードでは落とさない・権限の否定の確かめにも使う）。
export async function call<T = unknown>(
  page: Page,
  method: "GET" | "POST" | "PUT" | "PATCH" | "DELETE",
  path: string,
  data?: unknown,
): Promise<Reply<T>> {
  const headers: Record<string, string> = { "x-csrf-token": await csrf(page) };
  if (data !== undefined) headers["content-type"] = "application/json";
  const res = await send(() => page.request.fetch(`/api${path}`, { method, headers, data }));
  const text = await res.text();
  let body: unknown = null;
  try {
    body = text ? JSON.parse(text) : null;
  } catch {
    body = null;
  }
  return { status: res.status(), body: body as T, text };
}

/// `/api{path}` を呼び、成功（`expected` のどれか・省略時は 2xx）でなければ本文つきで落とす。
export async function ok<T = unknown>(
  page: Page,
  method: "GET" | "POST" | "PUT" | "PATCH" | "DELETE",
  path: string,
  data?: unknown,
  expected?: number[],
): Promise<T> {
  const r = await call<T>(page, method, path, data);
  const good = expected ? expected.includes(r.status) : r.status >= 200 && r.status < 300;
  expect(good, `${method} ${path} → ${r.status} ${r.text.slice(0, 600)}`).toBe(true);
  return r.body;
}

/// 送信ごとの冪等キー（英数字と `-` の 8〜64 文字）。
export function idemKey(prefix = "k"): string {
  return `${prefix}-${Date.now().toString(36)}-${Math.random().toString(36).slice(2, 10)}`;
}
