/// 文書検索（permission-aware RAG）API クライアント。生成型のみ使用（手書き型禁止）。
import type { components } from "@/generated/api";

import { apiFetch } from "./api";

export type SearchRequest = components["schemas"]["SearchRequest"];
export type SearchResponse = components["schemas"]["SearchResponse"];
export type SearchResult = components["schemas"]["SearchResult"];
export type SearchDebug = components["schemas"]["SearchDebug"];
export type SearchMode = components["schemas"]["SearchMode"];

export class SearchApiError extends Error {
  constructor(
    public status: number,
    message: string,
  ) {
    super(message);
    this.name = "SearchApiError";
  }
}

/// `POST /search`。403/503 等はステータス付きエラーにして UI 側で文言を出し分ける。
export async function searchDocuments(req: SearchRequest): Promise<SearchResponse> {
  const res = await apiFetch("/search", {
    method: "POST",
    headers: { "Content-Type": "application/json" },
    body: JSON.stringify(req),
  });
  if (!res.ok) {
    const message =
      res.status === 503
        ? "検索基盤が起動中か無効化されています。しばらくして再試行してください。"
        : `検索に失敗しました（HTTP ${res.status}）`;
    throw new SearchApiError(res.status, message);
  }
  return (await res.json()) as SearchResponse;
}
