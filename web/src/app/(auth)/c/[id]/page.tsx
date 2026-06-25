"use client";

import { use } from "react";

import { Conversation } from "@/components/chat/conversation";

/// 会話画面 `/c/[id]`。Next 15 では params は Promise のため use() で展開する。
/// セッション本体は client の chat-store（localStorage）から購読する。
export default function ChatPage({ params }: { params: Promise<{ id: string }> }) {
  const { id } = use(params);
  return <Conversation sessionId={id} />;
}
