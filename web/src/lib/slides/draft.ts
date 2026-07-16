"use client";

/// 未保存の下書きスライドのクライアント側ストアと遷移 URL（Task 11.3）。
///
/// ノートの下書き確定型（issue #282）と同型: チャットの save_slide が slide_draft を返し、
/// 下書きスライド画面（`/slides/draft?thread=&name=`）で詰めてから「ドライブに保存」
/// （POST /slides）で実体化する。本文（`content`）は正規化スライド JSON 文字列。

import { createDraftStore, parseDraftPayload } from "@/lib/drafts/store";

export const slideDraftStore = createDraftStore("slide");

/// 下書きスライド画面への遷移 URL（(threadId, name) キー・カード/ストリームで共有）。
export const SLIDE_DRAFT_PATH = "/slides/draft";

export function slideDraftHref(threadId: string, name: string): string {
  const params = new URLSearchParams({ thread: threadId, name });
  return `${SLIDE_DRAFT_PATH}?${params.toString()}`;
}

/// slide_draft イベント/ブロックの payload を厳格に検証する（fail-closed）。
export function parseSlideDraft(raw: unknown): { name: string; content: string } | null {
  return parseDraftPayload(raw, "content");
}
