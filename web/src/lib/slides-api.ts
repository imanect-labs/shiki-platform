/// スライド API クライアント（Task 11.1・design §4.8.3）。型は OpenAPI 生成（@/generated/api）。

import { apiFetch } from "@/lib/api";
import type { components } from "@/generated/api";

export type NodeResponse = components["schemas"]["NodeResponse"];

/// スライドドキュメント（正規化 JSON）の 1 枚。サーバの collab::slide::model と対。
export interface SlideData {
  id: string;
  html: string;
  notes: string;
  bg: Record<string, unknown> | null;
}

/// スライド（.slide ファイル）を作成する。content 省略時はタイトル 1 枚。
export async function createSlide(input: {
  parentId?: string | null;
  name: string;
  content?: unknown;
}): Promise<NodeResponse> {
  const res = await apiFetch("/slides", {
    method: "POST",
    headers: { "Content-Type": "application/json" },
    body: JSON.stringify({
      parent_id: input.parentId ?? null,
      name: input.name,
      content: input.content ?? null,
    }),
  });
  if (!res.ok) {
    throw new Error(`スライドの作成に失敗しました (${res.status})`);
  }
  return (await res.json()) as NodeResponse;
}
