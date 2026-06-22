import type { components } from "@/generated/api";

/// OpenAPI(utoipa) から生成した型。手書きの API 型は持たない。
export type MeResponse = components["schemas"]["MeResponse"];

/// 同一オリジンの BFF プロキシ経由で shiki-server を叩く（Next rewrites で /api/* → server）。
const API_BASE = "/api";

/// CSRF Cookie 名。サーバの定数 `crate::session::CSRF_COOKIE`（"shiki_csrf"）と一致させる契約。
/// サーバ側も設定不可の固定値にしてあるため、ここをハードコードしてもドリフトしない。
const CSRF_COOKIE = "shiki_csrf";

/// double-submit CSRF 用に CSRF Cookie の値を読む（httpOnly ではないので JS から読める）。
export function csrfToken(): string | undefined {
  if (typeof document === "undefined") return undefined;
  const match = document.cookie.match(new RegExp(`(?:^|;\\s*)${CSRF_COOKIE}=([^;]+)`));
  return match ? decodeURIComponent(match[1]) : undefined;
}

/// セッション Cookie を送る fetch ラッパ（Authorization ヘッダは使わない）。
/// 状態変更系メソッドには CSRF ヘッダを自動付与する。
export async function apiFetch(path: string, init?: RequestInit): Promise<Response> {
  const headers = new Headers(init?.headers);
  const method = (init?.method ?? "GET").toUpperCase();
  if (method !== "GET" && method !== "HEAD") {
    const token = csrfToken();
    if (token) headers.set("X-CSRF-Token", token);
  }
  return fetch(`${API_BASE}${path}`, { ...init, headers, credentials: "include" });
}

/// 現在のユーザー情報を取得する（未ログインなら 401）。
export async function fetchMe(): Promise<MeResponse> {
  const res = await apiFetch("/me");
  if (!res.ok) {
    throw new Error(`/me が ${res.status} を返しました`);
  }
  return (await res.json()) as MeResponse;
}
