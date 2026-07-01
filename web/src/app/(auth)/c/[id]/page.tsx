"use client";

import { use } from "react";

import { Conversation } from "@/components/chat/conversation";

/// 会話画面 `/c/[id]`。Next 15 では params は Promise のため use() で展開する。
/// スレッド本体は backend（/threads）から取得し、応答は SSE でストリーミングする。
export default function ChatPage({ params }: { params: Promise<{ id: string }> }) {
  const { id } = use(params);
  return <Conversation threadId={id} />;
}
