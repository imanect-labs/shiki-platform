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

export type Method = "GET" | "POST" | "PUT" | "PATCH" | "DELETE";

/// 送り直してよい方法（同じ要求を 2 回届けても結果が変わらない・RFC 9110 §9.2.2）。POST と PATCH は送り直さない
/// （起動・作成・送信は、届いたかどうかが分からないまま送り直すと二重になりうる）。
const IDEMPOTENT: ReadonlySet<Method> = new Set<Method>(["GET", "PUT", "DELETE"]);

/// 冪等な方法（GET・PUT・DELETE）で閉じた接続に当たったときだけ、つなぎ直して 1 回送り直す（next dev は 5 秒
/// 使わない接続を閉じる・helpers.ts の sendCleanup と同じ判断）。POST・PATCH は、要求が届いていないと言い切れない
/// ので送り直さず、そのまま落とす。
async function send(method: Method, fn: () => Promise<APIResponse>): Promise<APIResponse> {
  try {
    return await fn();
  } catch (e) {
    if (!IDEMPOTENT.has(method) || !/ECONNRESET|socket hang up/.test(String(e))) throw e;
    return fn();
  }
}

/// `/api{path}` を呼び、状態コードと本文を返す（状態コードでは落とさない・権限の否定の確かめにも使う）。
export async function call<T = unknown>(
  page: Page,
  method: Method,
  path: string,
  data?: unknown,
): Promise<Reply<T>> {
  const headers: Record<string, string> = { "x-csrf-token": await csrf(page) };
  if (data !== undefined) headers["content-type"] = "application/json";
  const res = await send(method, () => page.request.fetch(`/api${path}`, { method, headers, data }));
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
  method: Method,
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
