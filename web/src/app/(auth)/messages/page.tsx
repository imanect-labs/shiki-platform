"use client";

/// 職員間メッセージ（Phase 14 Stage 4・design §4.14 / FR-18）の画面。
///
/// 現時点では **UI モック**で、`crates/messaging`・OpenFGA の `channel` 型・SSE 配線は未実装。
/// 画面の作法（3 ペイン・content blocks・権限に従う添付・参加チャンネルに限る検索）を先に固める。

import { MessagesWorkspace } from "@/components/messages/messages-workspace";

export default function MessagesPage() {
  return <MessagesWorkspace />;
}
