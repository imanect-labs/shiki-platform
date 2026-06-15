import type { components } from "@/generated/api";

/// OpenAPI(utoipa) から生成した型。手書きの API 型は持たない。
export type MeResponse = components["schemas"]["MeResponse"];

export const API_BASE =
  process.env.NEXT_PUBLIC_API_BASE_URL ?? "http://localhost:8080";

/// Authorization ヘッダを自動付与する fetch ラッパ。
/// SSE(fetch-stream) でも同じ経路でヘッダを付与する想定。
export async function authedFetch(
  path: string,
  token: string | undefined,
  init?: RequestInit,
): Promise<Response> {
  const headers = new Headers(init?.headers);
  if (token) {
    headers.set("Authorization", `Bearer ${token}`);
  }
  return fetch(`${API_BASE}${path}`, { ...init, headers });
}

/// 現在のユーザー情報を取得する。
export async function fetchMe(token: string): Promise<MeResponse> {
  const res = await authedFetch("/me", token);
  if (!res.ok) {
    throw new Error(`/me が ${res.status} を返しました`);
  }
  return (await res.json()) as MeResponse;
}
