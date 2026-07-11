/// ノート API クライアント（Task 11P.2/11P.3）。型は OpenAPI 生成（@/generated/api）。

import { apiFetch } from "@/lib/api";
import type { components } from "@/generated/api";

export type NodeResponse = components["schemas"]["NodeResponse"];
export type CollabAccess = components["schemas"]["CollabAccessResponse"];

/// ノート（.md ファイル）を作成する。markdown 省略時は空ノート。
export async function createNote(input: {
  parentId?: string | null;
  name: string;
  markdown?: string | null;
}): Promise<NodeResponse> {
  const res = await apiFetch("/notes", {
    method: "POST",
    headers: { "Content-Type": "application/json" },
    body: JSON.stringify({
      parent_id: input.parentId ?? null,
      name: input.name,
      markdown: input.markdown ?? null,
    }),
  });
  if (!res.ok) {
    throw new Error(`ノートの作成に失敗しました (${res.status})`);
  }
  return (await res.json()) as NodeResponse;
}

/// 共同編集アクセスモード（editor/viewer）を取得する。404 は「無い/読めない」。
export async function getCollabAccess(nodeId: string): Promise<CollabAccess | null> {
  const res = await apiFetch(`/collab/docs/${nodeId}/access`);
  if (res.status === 404) return null;
  if (!res.ok) {
    throw new Error(`アクセス情報の取得に失敗しました (${res.status})`);
  }
  return (await res.json()) as CollabAccess;
}
