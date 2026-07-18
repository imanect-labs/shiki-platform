"use client";

/// Office 文書（Collabora）に紐づくチャットパネル（Task 11.10）。
///
/// 汎用 [`DocChatPanel`] に、アクティブ会話 id を **localStorage** へ保存する
/// [`ActiveThreadStore`] を与えた薄いラッパ。Office 文書は Yjs を持たない（Collabora が
/// バイナリを持つ）ため、会話継続は per-user の localStorage で担保する。会話一覧・1:N は
/// ノートと同じく `origin_note_id` = ファイルの node id で引く。

import * as React from "react";

import { DocChatPanel, useLocalActiveThreadStore } from "@/components/chat/doc-chat-panel";

export function OfficeChatPanel({
  fileId,
  fileName,
}: {
  fileId: string;
  fileName: string;
}) {
  const store = useLocalActiveThreadStore(fileId);
  const cleanName = React.useMemo(() => fileName.replace(/\.(docx|xlsx|pptx|odt|ods|odp)$/i, ""), [fileName]);
  return (
    <DocChatPanel
      store={store}
      nodeId={fileId}
      title={`文書: ${cleanName}`}
      label="この文書の会話"
      // 文書を開けている（＝viewer 以上）なら会話を作れる。AI の編集自体は office.edit が
      // editor@file をサーバ側で再判定するため、ここを editor に限定しない。
      editable
      testIdPrefix="office"
    />
  );
}
