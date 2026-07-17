"use client";

/// ノートに紐づくチャットパネル（Task 11P.5 ＋ issue #282・分割ビューのサイドパネル）。
///
/// 汎用の [`DocChatPanel`] に、アクティブ会話 id を **Yjs Map "meta"**（共同編集者間で同期）へ
/// 読み書きする [`ActiveThreadStore`] を与えた薄いラッパ。Office 文書版（localStorage 保存）と
/// UI・会話 1:N ロジックを共有する。

import * as React from "react";
import type * as Y from "yjs";

import { DocChatPanel, type ActiveThreadStore } from "@/components/chat/doc-chat-panel";

/// meta からアクティブ会話 id を読む（active_thread_id 優先・旧 thread_id 後方互換）。
function readActiveThreadId(meta: Y.Map<unknown>): string | null {
  const v = meta.get("active_thread_id") ?? meta.get("thread_id");
  return typeof v === "string" && v.length > 0 ? v : null;
}

export function NoteChatPanel({
  meta,
  noteId,
  noteName,
  editable,
  initialThreadId,
}: {
  meta: Y.Map<unknown>;
  /// ノートのストレージ node id（origin_note_id に使う）。
  noteId: string;
  noteName: string;
  /// editor のみが会話を新規作成できる（viewer は既存会話の閲覧のみ）。
  editable: boolean;
  /// サイドバー等から特定会話を開くときの初期アクティブ（?thread=）。
  initialThreadId?: string | null;
}) {
  const cleanName = React.useMemo(() => noteName.replace(/\.md$/i, ""), [noteName]);
  // Yjs meta をアクティブ会話 id の保存先にする（新旧キーを両方書き、旧クライアントも追従）。
  const store = React.useMemo<ActiveThreadStore>(
    () => ({
      read: () => readActiveThreadId(meta),
      write: (id) => {
        meta.set("active_thread_id", id);
        meta.set("thread_id", id);
      },
      subscribe: (onChange) => {
        meta.observe(onChange);
        return () => meta.unobserve(onChange);
      },
    }),
    [meta],
  );

  return (
    <DocChatPanel
      store={store}
      nodeId={noteId}
      title={`ノート: ${cleanName}`}
      label="このノートの会話"
      editable={editable}
      initialThreadId={initialThreadId}
      testIdPrefix="note"
    />
  );
}
