"use client";

/// 下書きを「ドライブに保存」するときの保存先の初期値（#942）。
///
/// 優先順: ① AI が指定した保存先（下書きの `parent_id`。`"root"` ならルート）→ ② 会話の由来ノートと
/// 同じフォルダ → ③ ルート（`null`）。どれも**初期値**にすぎず、確定はユーザーが保存ダイアログで行う。
/// 読めない・消えたフォルダは黙って次の候補へ落とす（存在を推測させる表示をしない）。

import * as React from "react";

import { getThread } from "@/lib/chat-api";
import { breadcrumb, getNode } from "@/lib/storage";

export type SaveFolder = { id: string; name: string };

/// フォルダ id から表示名（読める範囲のパス「A / B」）を引く。フォルダでなければ null。
async function folderOf(folderId: string): Promise<SaveFolder | null> {
  const crumbs = await breadcrumb(folderId);
  const self = crumbs[crumbs.length - 1];
  if (!self || self.id !== folderId || self.kind !== "folder") return null;
  return { id: folderId, name: crumbs.map((c) => c.name).join(" / ") };
}

async function resolveSaveFolder(
  threadId: string,
  aiParentId: string | undefined,
): Promise<SaveFolder | null> {
  // AI が明示的にルートを指定したら、由来ノートの場所へは戻さない。
  if (aiParentId === "root") return null;
  if (aiParentId) {
    const folder = await folderOf(aiParentId).catch(() => null);
    if (folder) return folder;
  }
  if (!threadId) return null;
  const thread = await getThread(threadId).catch(() => null);
  if (!thread?.originNoteId) return null;
  const note = await getNode(thread.originNoteId).catch(() => null);
  if (!note?.parent_id) return null;
  return folderOf(note.parent_id).catch(() => null);
}

/// 保存先の初期値を引く。`undefined` は解決中、`null` はルート。
///
/// 引くのは `enabled`（保存ダイアログが開いている）間だけ。下書き画面を開いただけで
/// 会話・ノート・パンくずを取りに行かない（保存しないまま閉じることも多い）。
export function useDefaultSaveFolder(
  threadId: string,
  aiParentId: string | undefined,
  enabled: boolean,
): SaveFolder | null | undefined {
  const [folder, setFolder] = React.useState<SaveFolder | null | undefined>(undefined);
  React.useEffect(() => {
    if (!enabled) return;
    let cancelled = false;
    setFolder(undefined);
    void resolveSaveFolder(threadId, aiParentId).then((f) => {
      if (!cancelled) setFolder(f);
    });
    return () => {
      cancelled = true;
    };
  }, [threadId, aiParentId, enabled]);
  return folder;
}
