/// Word 文書（.docx）API クライアント（#332）。型は OpenAPI 生成（@/generated/api）。

import { apiFetch } from "@/lib/api";
import type { components } from "@/generated/api";

export type NodeResponse = components["schemas"]["NodeResponse"];

/// Word 文書（.docx）を空テンプレ（blank.docx）から作成する。
/// 本文入りの作成は AI の save_document（Collabora へ paste）に一本化した（#381）ため、
/// この API は本文を受けない＝変換サービス（worker）にも Collabora にも依存しない。
export async function createDocument(input: {
  parentId?: string | null;
  name: string;
}): Promise<NodeResponse> {
  const res = await apiFetch("/documents", {
    method: "POST",
    headers: { "Content-Type": "application/json" },
    body: JSON.stringify({ parent_id: input.parentId ?? null, name: input.name }),
  });
  if (!res.ok) {
    throw new Error(`Word 文書の作成に失敗しました (${res.status})`);
  }
  return (await res.json()) as NodeResponse;
}

/// Excel ブック（.xlsx）を空テンプレから作成する（#381）。
/// 変換を伴わないため worker にも Collabora にも依存しない（開くのは Collabora Calc）。
export async function createSheet(input: {
  parentId?: string | null;
  name: string;
}): Promise<NodeResponse> {
  const res = await apiFetch("/sheets", {
    method: "POST",
    headers: { "Content-Type": "application/json" },
    body: JSON.stringify({ parent_id: input.parentId ?? null, name: input.name }),
  });
  if (!res.ok) {
    throw new Error(`Excel ブックの作成に失敗しました (${res.status})`);
  }
  return (await res.json()) as NodeResponse;
}
