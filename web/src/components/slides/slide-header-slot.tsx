"use client";

/// スライド詳細の統一ヘッダ注入（ノートの NoteSyncSlot と同型・Task 11.1）。
/// 戻る/名前/閲覧のみ/プレゼンス/同期ピルを共通ヘッダへ寄せる。null を返すだけ。

import { ArrowLeft, Eye, MessageSquare } from "lucide-react";
import Link from "next/link";

import { PresenceAvatars } from "@/components/notes/presence";
import { SyncPill } from "@/components/notes/note-status";
import { usePageHeader } from "@/components/shell/page-header-context";
import { Button } from "@/components/ui/button";
import type { CollabProvider, CollabStatus } from "@/lib/collab";

export function SlideHeaderSlot({
  name,
  editable,
  status,
  synced,
  provider,
  chatOpen = false,
  onToggleChat,
}: {
  name: string;
  editable: boolean;
  status: CollabStatus;
  synced: boolean;
  provider: CollabProvider | null;
  /// アシスタントパネルの開閉（Task 11.10。onToggleChat 未指定ならボタンを出さない）。
  chatOpen?: boolean;
  onToggleChat?: () => void;
}) {
  usePageHeader(
    () => (
      <div className="flex min-w-0 flex-1 items-center gap-2.5">
        <Link
          href="/drive"
          className="flex size-8 shrink-0 items-center justify-center rounded-md text-muted-foreground transition-colors hover:bg-accent hover:text-foreground active:scale-95"
          aria-label="ドライブへ戻る"
        >
          <ArrowLeft className="size-4" />
        </Link>
        <span className="min-w-0 flex-1 truncate text-sm font-medium text-foreground">{name}</span>
        {!editable ? (
          <span
            className="inline-flex items-center gap-1 rounded-full border bg-muted/50 px-2.5 py-0.5 text-xs font-medium text-muted-foreground"
            data-testid="slide-readonly-badge"
          >
            <Eye className="size-3.5" aria-hidden />
            閲覧のみ
          </span>
        ) : null}
        {provider ? <PresenceAvatars provider={provider} /> : null}
        <SyncPill status={status} synced={synced} />
        {onToggleChat ? (
          <Button
            type="button"
            variant={chatOpen ? "secondary" : "ghost"}
            size="sm"
            onClick={onToggleChat}
            aria-pressed={chatOpen}
            data-testid="slide-chat-toggle"
          >
            <MessageSquare className="mr-1.5 size-4" aria-hidden />
            アシスタント
          </Button>
        ) : null}
      </div>
    ),
    [name, editable, status, synced, provider, chatOpen, onToggleChat],
  );
  return null;
}
