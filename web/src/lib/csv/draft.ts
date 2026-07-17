"use client";

/// 未保存の下書き CSV のクライアント側ストアと遷移 URL（Task 11.11）。
///
/// ノートの下書き確定型（issue #282）と同型: チャットの save_csv が csv_draft を返し、
/// 下書き CSV 画面（`/csv/draft?thread=&name=`）で詰めてから「ドライブに保存」
/// （POST /tabular/save）で実体化する。本文（`content`）は CSV 文字列。

import { createDraftStore, parseDraftPayload } from "@/lib/drafts/store";

export const csvDraftStore = createDraftStore("csv");

/// 下書き CSV 画面への遷移 URL（(threadId, name) キー・カード/ストリームで共有）。
export function csvDraftHref(threadId: string, name: string): string {
  return `/csv/draft?thread=${encodeURIComponent(threadId)}&name=${encodeURIComponent(name)}`;
}

/// csv_draft イベント/ブロックの payload（`{name, csv}`）を厳格に検証する（fail-closed）。
export function parseCsvDraft(raw: unknown): { name: string; csv: string } | null {
  const d = parseDraftPayload(raw, "csv");
  return d ? { name: d.name, csv: d.content } : null;
}
