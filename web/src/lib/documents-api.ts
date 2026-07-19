/// Word 文書（.docx）API クライアント（#332）。型は OpenAPI 生成（@/generated/api）。

import { apiFetch } from "@/lib/api";
import type { components } from "@/generated/api";

export type NodeResponse = components["schemas"]["NodeResponse"];

/// Word 文書（.docx）を作成する。markdown 省略時は空ドキュメント（blank.docx）。
/// 本文ありの場合はサーバ側で blank.docx + append_markdown（office.edit と同経路）により
/// .docx 化される。worker 不達は 503。
export async function createDocument(input: {
  parentId?: string | null;
  name: string;
  markdown?: string | null;
}): Promise<NodeResponse> {
  const res = await apiFetch("/documents", {
    method: "POST",
    headers: { "Content-Type": "application/json" },
    body: JSON.stringify({
      parent_id: input.parentId ?? null,
      name: input.name,
      markdown: input.markdown ?? null,
    }),
  });
  if (res.status === 503) {
    throw new Error(
      "文書変換サービスに接続できません。時間をおいて再試行してください (503)",
    );
  }
  if (!res.ok) {
    throw new Error(`Word 文書の作成に失敗しました (${res.status})`);
  }
  return (await res.json()) as NodeResponse;
}
