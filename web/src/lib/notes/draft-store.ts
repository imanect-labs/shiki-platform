"use client";

/// 未保存の下書きノートのクライアント側ストア（issue #282）。
///
/// 実体は generic 下書きストア（[`@/lib/drafts/store`]・kind="note"）への委譲。localStorage
/// キー（`shiki.note-drafts.v1`）と公開 API はリファクタ前と互換で、旧形式（`markdown`
/// フィールド）のエントリはストア側が読み取り時に `content` へ移行する。

import {
  createDraftStore,
  parseDraftPayload,
  type DraftEntry,
  type DraftSource,
} from "@/lib/drafts/store";

export type { DraftSource };

/// 下書きエントリ（本文は `content`＝Markdown）。
export type NoteDraftEntry = DraftEntry;

const store = createDraftStore("note");

/// 変更購読（React 外からも使える）。
export const subscribeDrafts = store.subscribe;

/// 下書きを upsert する（同キーは rev+1 で更新）。戻り値は確定エントリ。
export function upsertDraft(
  threadId: string,
  name: string,
  markdown: string,
  source: DraftSource,
  parentId?: string,
): NoteDraftEntry {
  return store.upsert(threadId, name, markdown, source, parentId);
}

export function getDraft(threadId: string, name: string): NoteDraftEntry | null {
  return store.get(threadId, name);
}

/// 会話の下書き一覧（更新日昇順＝タブは作られた順）。
export function listDrafts(threadId: string): NoteDraftEntry[] {
  return store.list(threadId);
}

export function removeDraft(threadId: string, name: string): void {
  store.remove(threadId, name);
}

/// React 購読フック（会話の下書き一覧）。
export function useDrafts(threadId: string): NoteDraftEntry[] {
  return store.useDrafts(threadId);
}

/// note_draft イベント/ブロックの payload を厳格に検証する（fail-closed）。
export function parseNoteDraft(
  raw: unknown,
): { name: string; markdown: string; parentId?: string } | null {
  const d = parseDraftPayload(raw, "markdown");
  return d ? { name: d.name, markdown: d.content, parentId: d.parentId } : null;
}
