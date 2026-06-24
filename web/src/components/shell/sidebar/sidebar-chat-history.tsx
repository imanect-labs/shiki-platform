"use client";

import { MessageSquareText } from "lucide-react";

/// サイドバー中段のチャット履歴（スクロール領域）。
/// チャット backend（Phase 3）は未実装のため、フェイク履歴は置かず空状態を出す。
/// レール時は領域ごと隠す（アイコン列に履歴は出さない）。
export function SidebarChatHistory({ collapsed }: { collapsed: boolean }) {
  if (collapsed) return <div className="flex-1" aria-hidden />;

  return (
    <div className="flex min-h-0 flex-1 flex-col">
      <div className="px-5 pb-1 pt-3">
        <span className="text-xs font-medium text-sidebar-foreground/50">チャット履歴</span>
      </div>
      <div className="min-h-0 flex-1 overflow-y-auto px-2 pb-2">
        <div className="flex flex-col items-center gap-2 px-3 py-8 text-center">
          <MessageSquareText className="size-5 text-sidebar-foreground/40" aria-hidden />
          <p className="text-xs leading-relaxed text-sidebar-foreground/50">
            まだチャットはありません。
            <br />
            「新しいチャット」から始めましょう。
          </p>
        </div>
      </div>
    </div>
  );
}
