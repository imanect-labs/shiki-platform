"use client";

/// 未保存の下書き Word 文書のクライアント側ストアと遷移 URL（#332）。
///
/// ノートの下書き確定型（issue #282）と同型: チャットの save_document が document_draft を
/// 返し、下書き文書画面（`/office/draft?thread=&name=`）で詰めてから「ドライブに保存」
/// （POST /documents・blank.docx + append_markdown で .docx 化）で実体化する。
/// 本文（`content`）は Markdown 文字列。

import { createDraftStore, parseDraftPayload } from "@/lib/drafts/store";

export const documentDraftStore = createDraftStore("document");

/// 下書き文書画面への遷移 URL（(threadId, name) キー・カード/ストリームで共有）。
export function documentDraftHref(threadId: string, name: string): string {
  return `/office/draft?thread=${encodeURIComponent(threadId)}&name=${encodeURIComponent(name)}`;
}

/// document_draft イベント/ブロックの payload（`{name, markdown}`）を厳格に検証する（fail-closed）。
export function parseDocumentDraft(raw: unknown): { name: string; markdown: string } | null {
  const d = parseDraftPayload(raw, "markdown");
  return d ? { name: d.name, markdown: d.content } : null;
}
